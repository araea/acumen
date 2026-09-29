//! 内联媒体走 `upload.create`。
//!
//! 插件习惯把生成的图直接写成 `base64://…`。Satori 的[资源链接](https://satori.chat/advanced/resource.html)
//! 指南把这类内联编码列为不推荐：整段字节挤进消息体，体积大幅增加，实现端还得在处理消息时
//! 解码。规范给的路线是先 `upload.create` 拿到 `internal:` 链接，再把链接写进元素；实现端
//! 有自己的上传能力（satori-qq、satori-wx 都有）就直接用，还能绕开内联体积上限——satori-wx
//! 的内联图片上限 8 MiB、内联媒体 12 MiB，上传则到 1 GiB。
//!
//! 上传失败不是发送失败：退回内联，和以前一样。

use super::{BotStatus, SatoriClient};
use crate::message::{Message, Segment};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::future::BoxFuture;
use simd_json::OwnedValue;
use simd_json::base::ValueAsScalar;

/// 解出来的内联载荷。
pub struct Inline {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub ext: &'static str,
}

/// `base64://…` 或 `data:<mime>;base64,…`；其余（`http(s):`、`file:`、`internal:`、本地路径）
/// 都不是内联。
pub fn decode_inline(src: &str) -> Option<Inline> {
    let payload = if let Some(rest) = src.strip_prefix("base64://") {
        rest
    } else if let Some(rest) = src.strip_prefix("data:") {
        let (head, body) = rest.split_once(',')?;
        if !head
            .split(';')
            .any(|part| part.eq_ignore_ascii_case("base64"))
        {
            return None;
        }
        body
    } else {
        return None;
    };
    let bytes = STANDARD.decode(payload.trim()).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let (mime, ext) = sniff(&bytes);
    Some(Inline { bytes, mime, ext })
}

/// 按魔数认格式：内联载荷没有可信的文件名，上传的 `Content-Type` 又是必需的。
fn sniff(bytes: &[u8]) -> (&'static str, &'static str) {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        ("image/png", "png")
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        ("image/jpeg", "jpg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        ("image/gif", "gif")
    } else if starts(b"BM") && bytes.len() > 14 {
        ("image/bmp", "bmp")
    } else if starts(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        ("image/webp", "webp")
    } else if starts(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        ("audio/wav", "wav")
    } else if bytes.get(4..8) == Some(b"ftyp") {
        ("video/mp4", "mp4")
    } else if starts(b"OggS") {
        ("audio/ogg", "ogg")
    } else if starts(b"#!AMR") {
        ("audio/amr", "amr")
    } else if starts(b"#!SILK") || starts(b"\x02#!SILK") {
        ("audio/silk", "silk")
    } else if starts(b"ID3") || (bytes.len() > 1 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0) {
        ("audio/mpeg", "mp3")
    } else {
        ("application/octet-stream", "bin")
    }
}

/// 消息里带媒体的段类型（内部消息模型的叫法）。
const MEDIA: [&str; 4] = ["image", "record", "video", "file"];

/// 把消息里的内联媒体换成 `upload.create` 给的 `internal:` 链接，返回换好的副本。
/// `client` / `bot` 必须是这条消息最终要发往的那条连接：`internal:` 链接只在签发它的
/// 实现端、按登录解析得开。
pub async fn externalize(
    client: &SatoriClient,
    bot: &BotStatus,
    message: &OwnedValue,
) -> OwnedValue {
    let Ok(mut parsed) = simd_json::serde::from_owned_value::<Message>(message.clone()) else {
        return message.clone();
    };
    if !rewrite(client, bot, &mut parsed).await {
        return message.clone();
    }
    simd_json::serde::to_owned_value(parsed).unwrap_or_else(|_| message.clone())
}

/// 返回是否改动过。
fn rewrite<'a>(
    client: &'a SatoriClient,
    bot: &'a BotStatus,
    message: &'a mut Message,
) -> BoxFuture<'a, bool> {
    Box::pin(async move {
        let mut changed = false;
        for segment in &mut message.0 {
            changed |= rewrite_segment(client, bot, segment).await;
        }
        changed
    })
}

async fn rewrite_segment(client: &SatoriClient, bot: &BotStatus, segment: &mut Segment) -> bool {
    if segment.type_ == "node" {
        // 合并转发的节点里还有一层消息。
        let Some(content) = segment.data.get("content").cloned() else {
            return false;
        };
        let Ok(mut inner) = simd_json::serde::from_owned_value::<Message>(content) else {
            return false;
        };
        if !rewrite(client, bot, &mut inner).await {
            return false;
        }
        if let Ok(value) = simd_json::serde::to_owned_value(inner) {
            segment.data.insert("content".into(), value);
            return true;
        }
        return false;
    }
    if !MEDIA.contains(&segment.type_.as_str()) {
        return false;
    }
    let Some(inline) = segment
        .data
        .get("file")
        .and_then(|value| value.as_str())
        .and_then(decode_inline)
    else {
        return false;
    };
    let name = segment
        .data
        .get("name")
        .and_then(|value| value.as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}.{}", segment.type_, inline.ext));
    let uploaded = match client
        .upload_as(bot, inline.bytes, &name, inline.mime)
        .await
    {
        Ok(uploaded) => uploaded,
        Err(error) => {
            crate::warn!(target: "Bot", "内联媒体上传失败，改按内联发送：{error}");
            return false;
        }
    };
    let Some(resource) = uploaded.get("file").and_then(serde_json::Value::as_str) else {
        crate::warn!(target: "Bot", "upload.create 没有返回 file 资源，改按内联发送");
        return false;
    };
    segment
        .data
        .insert("file".into(), OwnedValue::from(resource.to_string()));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn inline_sources_decode_and_others_do_not() {
        let b64 = STANDARD.encode(PNG);
        let raw = decode_inline(&format!("base64://{b64}")).unwrap();
        assert_eq!(
            (raw.mime, raw.ext, raw.bytes.as_slice()),
            ("image/png", "png", PNG)
        );
        let url = decode_inline(&format!("data:image/png;base64,{b64}")).unwrap();
        assert_eq!(url.bytes, PNG);
        for src in [
            "https://example.com/a.png",
            "file:///sdcard/a.png",
            "internal:red/1/_tmp/x",
            "/sdcard/a.png",
            "data:text/plain,hello",
            "base64://",
            "base64://***not base64***",
        ] {
            assert!(decode_inline(src).is_none(), "{src}");
        }
    }

    #[test]
    fn formats_are_told_by_magic_number() {
        for (bytes, mime) in [
            (&b"GIF89a\x01\x00"[..], "image/gif"),
            (&[0xFF, 0xD8, 0xFF, 0xE0][..], "image/jpeg"),
            (&b"RIFF\0\0\0\0WEBPVP8 "[..], "image/webp"),
            (&b"RIFF\0\0\0\0WAVEfmt "[..], "audio/wav"),
            (&b"\0\0\0\x18ftypmp42"[..], "video/mp4"),
            (&b"#!SILK_V3"[..], "audio/silk"),
            (&b"ID3\x03\0"[..], "audio/mpeg"),
            (&b"\x01\x02\x03\x04"[..], "application/octet-stream"),
        ] {
            assert_eq!(sniff(bytes).0, mime);
        }
    }

    /// 上传走的是消息将要发往的那条连接：本地起一个假实现端，收到 multipart 就回一个
    /// `internal:` 链接；不可达时保持内联，消息照发。
    #[tokio::test]
    async fn inline_media_becomes_an_internal_link_or_stays_inline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (seen, mut received) = tokio::sync::mpsc::unbounded_channel::<String>();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 65536];
                let mut total = 0;
                loop {
                    let n = stream.read(&mut buf[total..]).await.unwrap();
                    total += n;
                    let text = String::from_utf8_lossy(&buf[..total]).to_string();
                    if n == 0 || text.contains("--\r\n") {
                        seen.send(text).unwrap();
                        break;
                    }
                }
                let body = r#"{"file":"internal:red/10000/_tmp/abc"}"#;
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let client = SatoriClient::new(endpoint, None);
        let bot = BotStatus {
            adapter: "satori-qq".into(),
            platform: "red".into(),
            login_user: crate::event::LoginUser {
                id: "10000".into(),
                ..Default::default()
            }
            .into(),
        };
        let b64 = STANDARD.encode(PNG);
        let message = simd_json::serde::to_owned_value(
            Message::new()
                .text("看图")
                .image(format!("base64://{b64}"))
                .image("https://example.com/keep.png"),
        )
        .unwrap();
        let out = externalize(&client, &bot, &message).await;
        let content = crate::adapters::satori::message::to_content(&out);
        assert!(
            content.contains(r#"<img src="internal:red/10000/_tmp/abc"/>"#)
                && content.contains(r#"<img src="https://example.com/keep.png"/>"#)
                && !content.contains("base64"),
            "{content}"
        );
        let request = received.try_recv().unwrap();
        assert!(request.starts_with("POST /v1/upload.create "), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("satori-user-id: 10000"),
            "{request}"
        );
        assert!(request.contains("Content-Type: image/png"), "{request}");
        assert!(request.contains("filename=\"image.png\""), "{request}");
        assert!(received.try_recv().is_err(), "只有内联的那一张要上传");
        server.abort();

        // 实现端不可达：原样保留内联，消息不丢。
        let dead = SatoriClient::new("http://127.0.0.1:9".into(), None);
        let kept = externalize(&dead, &bot, &message).await;
        assert!(crate::adapters::satori::message::to_content(&kept).contains("base64://"));
    }
}
