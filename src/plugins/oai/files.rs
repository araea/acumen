//! DeepSeek 官方 Files API：把群里发来的 PDF / Word / PPT / 文本文件上传解析，
//! 再让模型在对话里按 `file_id` 直接读。
//!
//! Chat Completions 的文档块是扁平的 `{"type":"file","file_id":"…"}`（不是 rig 编码出的
//! 那种 `{"type":"file","file":{"file_id":…}}`），所以这里给出正确的块，由
//! [`super::llm::inject_extras`] 把它拼进最终请求。上传表单里 `purpose` 必须排在 `file`
//! 之前，所以 multipart 自己按字节拼，不交给 reqwest 的 Form 去排顺序。
//!
//! 文档：<https://api-docs.deepseek.com/zh-cn/guides/files_api>、
//! <https://api-docs.deepseek.com/zh-cn/guides/document_parsing>。

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::OaiConfig;
use super::inbox::Attachment;
use super::native;

const LOG_TARGET: &str = "Plugin/OAI/Files";
const BOUNDARY: &str = "----acumenDeepSeekFileBoundary7d93a1";
/// 单文件上限 100 MiB；一次请求最多 20 个文档。
const MAX_BYTES: usize = 100 * 1024 * 1024;
const MAX_DOCS: usize = 20;

/// 纯文本、表格与电子书不需要解析，`purpose` 用 `user_data`（这类上传不计费）。
const PLAIN: &[&str] = &[
    "txt", "md", "markdown", "csv", "json", "jsonl", "xml", "yaml", "yml", "toml", "log", "ini",
    "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "h", "cpp", "cc", "hpp", "cs",
    "sh", "bash", "zsh", "sql", "html", "htm", "css", "scss", "lua", "rb", "php", "swift",
    "kt", "scala", "dart", "vue", "svelte", "gradle", "makefile", "xlsx", "xls", "epub", "mobi",
];

/// 按文件名选 `purpose`：能直接当文本读的走 `user_data`，其余按配置做 OCR/版面解析。
pub(crate) fn purpose_of(name: &str, cfg: &native::DeepSeekConfig) -> &'static str {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ext.is_empty() || PLAIN.contains(&ext.as_str()) {
        "user_data"
    } else {
        cfg.purpose()
    }
}

/// 上传一个文件，返回可直接引用的 `file_id`。上传与解析都在服务端完成，回包即代表可用。
pub(crate) async fn upload(key: &str, bytes: Vec<u8>, name: &str, purpose: &str) -> Result<String> {
    if bytes.is_empty() {
        bail!("文件是空的");
    }
    if bytes.len() > MAX_BYTES {
        bail!("文件超过 100 MiB 上限");
    }
    let safe = safe_filename(name);
    let body = multipart_body(purpose.as_bytes(), &safe, &bytes);
    let response = crate::http::client()
        .post(format!("{}/files", native::OFFICIAL))
        .bearer_auth(key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        bail!("Files API {status}: {}", clip(&text));
    }
    let value: Value = serde_json::from_str(&text)?;
    file_id_of(&value)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Files API 没返回 id：{}", clip(&text)))
}

/// 把一批附件传上去，返回能拼进请求的 `file_id` 列表；单个失败不影响其余。
pub(crate) async fn upload_all(cfg: &OaiConfig, files: &[Attachment]) -> Vec<String> {
    if !cfg.deepseek.enabled || !cfg.deepseek.files || files.is_empty() {
        return Vec::new();
    }
    let Some(key) = native::official_key(cfg) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for file in files.iter().take(MAX_DOCS) {
        if ids.len() >= MAX_DOCS {
            break;
        }
        match upload_one(&key, file, cfg).await {
            Ok(id) => {
                info!(target: LOG_TARGET, "文件 {} 解析完成：{id}", file.name);
                ids.push(id);
            }
            Err(error) => {
                debug!(target: LOG_TARGET, "文件 {} 上传没成：{error}", file.name);
            }
        }
    }
    ids
}

async fn upload_one(key: &str, file: &Attachment, cfg: &OaiConfig) -> Result<String> {
    let response = crate::http::client()
        .get(&file.url)
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?.to_vec();
    if bytes.len() > MAX_BYTES {
        bail!("文件超过 100 MiB 上限");
    }
    let purpose = purpose_of(&file.name, &cfg.deepseek);
    upload(key, bytes, &file.name, purpose).await
}

/// Chat Completions 里引用文档的那一块。扁平结构，字段名是 `file_id` 不是嵌套的 `file`。
pub(crate) fn document_part(file_id: &str) -> Value {
    json!({"type": "file", "file_id": file_id})
}

fn file_id_of(value: &Value) -> Option<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// 自己拼 multipart：`purpose` 字段严格排在 `file` 字段之前（接口硬性要求）。
fn multipart_body(purpose: &[u8], filename: &str, file: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(file.len() + 512);
    out.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\n")
            .as_bytes(),
    );
    out.extend_from_slice(purpose);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(file);
    out.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    out
}

/// 文件名里的引号、换行会破掉 multipart 头；只留干净的一截。
fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '"' | '\r' | '\n' | '\\' | '/' => '_',
            _ => ch,
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "file.bin".to_string()
    } else {
        cleaned.chars().take(200).collect()
    }
}

fn clip(text: &str) -> String {
    text.trim().chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::native::DeepSeekConfig;

    #[test]
    fn plain_text_uses_free_user_data() {
        let cfg = DeepSeekConfig::default();
        assert_eq!(purpose_of("notes.txt", &cfg), "user_data");
        assert_eq!(purpose_of("table.XLSX", &cfg), "user_data");
        assert_eq!(purpose_of("book.epub", &cfg), "user_data");
        assert_eq!(purpose_of("report.pdf", &cfg), "text_recognition");
        assert_eq!(purpose_of("slides.PPTX", &cfg), "text_recognition");
        let rich = DeepSeekConfig {
            file_purpose: "rich_layout_recognition".into(),
            ..cfg
        };
        assert_eq!(purpose_of("report.pdf", &rich), "rich_layout_recognition");
    }

    #[test]
    fn multipart_puts_purpose_before_file() {
        let body = multipart_body(b"text_recognition", "a.pdf", b"%PDF-1.4");
        let text = String::from_utf8_lossy(&body);
        let purpose_at = text.find("name=\"purpose\"").unwrap();
        let file_at = text.find("name=\"file\"").unwrap();
        assert!(purpose_at < file_at, "{text}");
        assert!(text.contains("text_recognition"));
        assert!(text.ends_with(&format!("--{BOUNDARY}--\r\n")));
        assert!(text.contains("%PDF-1.4"));
    }

    #[test]
    fn filename_is_sanitized() {
        assert_eq!(safe_filename("a\"b/c\r\n.pdf"), "a_b_c__.pdf");
        assert_eq!(safe_filename("  "), "file.bin");
    }

    #[test]
    fn document_part_is_flat_file_id() {
        assert_eq!(
            document_part("file-api-123"),
            json!({"type": "file", "file_id": "file-api-123"})
        );
    }

    #[test]
    fn upload_response_id_is_read() {
        let value: Value = serde_json::from_str(
            r#"{"id":"file-api-abc","object":"file","bytes":70,"purpose":"user_data"}"#,
        )
        .unwrap();
        assert_eq!(file_id_of(&value), Some("file-api-abc"));
        assert_eq!(file_id_of(&json!({"object": "file"})), None);
    }
}
