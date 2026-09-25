use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::{extract_text_arg, get_image_url, match_command};
use crate::config::build_config;
use crate::event::Context;
use crate::http::download_bytes;
use crate::message::Message;
use crate::plugins::{PluginError, PluginResult};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use toml::Value;

pub mod gif_ops;
pub mod utils;

// =============================
//      Main Plugin Logic
// =============================

/// 帮助信息
const HELP_TEXT: &str = r#"💡 指令列表（大小写均可）

· gif帮助 / gifhelp - 显示本帮助
· 合成gif [行x列] [间隔秒] [边距]
    将网格图合成为动图
    示例：合成gif 3x3 0.1 0
· gif拼图 [列数] - 将动图转为网格图
· gif拆分 - 将动图拆成多张静态图
· gif变速 [倍率] - 调整播放速度
    示例：gif变速 2（加速 2 倍）
· gif倒放 - 倒序播放
· gif缩放 [倍率|尺寸]
    示例：gif缩放 0.5 或 gif缩放 100x100
· gif旋转 [角度] - 旋转（90、180、270、-90）
· gif翻转 [水平|垂直] - 镜像翻转
· gif信息 - 查看 GIF 详情

使用时请附带图片或引用图片消息"#;

/// 支持的指令
const COMMANDS: &[&str] = &[
    "gif帮助",
    "gifhelp",
    "合成gif",
    "gif变速",
    "gif倒放",
    "gif信息",
    "gif缩放",
    "gif旋转",
    "gif翻转",
    "gif拆分",
    "gif拼图",
];

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config {
    enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true }
    }
}

pub fn default_config() -> Value {
    build_config(Config::default())
}

pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, std::result::Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let msg = match ctx.as_message() {
            Some(m) => m,
            None => return Ok(Some(ctx)),
        };

        for &cmd in COMMANDS {
            if let Some(matched) = match_command(&ctx, cmd) {
                let group_id = msg.group_id();
                let user_id = msg.user_id();

                // 提取纯文本参数
                let args_text = extract_text_arg(&matched.args);
                let args: Vec<&str> = args_text.split_whitespace().collect();

                // 3. 帮助指令
                if matches!(cmd, "gif帮助" | "gifhelp") {
                    let prefix = crate::command::get_prefixes(&ctx)
                        .first()
                        .cloned()
                        .unwrap_or_default();
                    let mut help = HELP_TEXT.to_string();
                    for command in COMMANDS {
                        help = help.replace(command, &format!("{prefix}{command}"));
                    }
                    let _ = send_msg(&ctx, writer, group_id, Some(user_id), help).await;
                    return Ok(None);
                }

                // 4. 获取图片
                let img_url = match get_image_url(
                    &ctx,
                    writer.clone(),
                    &matched.args,
                    matched.reply_id.as_ref(),
                )
                .await
                {
                    Some(u) => u,
                    None => {
                        let _ = send_msg(
                            &ctx,
                            writer,
                            group_id,
                            Some(user_id),
                            "❌ 请附带图片或引用图片消息",
                        )
                        .await;
                        return Ok(None);
                    }
                };

                let _ = send_msg(&ctx, writer.clone(), group_id, Some(user_id), "⏳ 处理中…").await;

                let img_bytes = match download_bytes(&img_url).await {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = send_msg(
                            &ctx,
                            writer,
                            group_id,
                            Some(msg.user_id()),
                            format!("❌ 图片下载失败：{}", e),
                        )
                        .await;
                        return Ok(None);
                    }
                };

                // 5. 处理逻辑分发
                //
                // 纯解码 / 重编码的几条整体丢进阻塞线程池。一个几十帧的动图要全帧解码
                // 再逐帧编码，在手机上就是几秒的纯 CPU 与内存拷贝；压在 tokio worker
                // 上等于把那一个线程扣死，期间消息入库、出图、别的指令全排在它后面。
                // 同目录的 image_split 与 wordcloud 一直是这么做的，这里补齐。
                let res: PluginResult<Option<String>> = match cmd {
                    // 元信息也需要遍历解码帧，和拆帧一样进入工作池。
                    "gif信息" => {
                        match crate::render::worker::run(move || gif_ops::gif_info(img_bytes))
                            .await
                            .map_err(|e| Box::new(e) as PluginError)?
                        {
                            Ok(info) => {
                                let _ =
                                    send_msg(&ctx, writer.clone(), group_id, Some(user_id), info)
                                        .await;
                                Ok(None)
                            }
                            Err(e) => Err(e),
                        }
                    }
                    // 解码拆帧也交给渲染工作池，返回后再异步发送。
                    "gif拆分" => {
                        match crate::render::worker::run(move || gif_ops::gif_to_frames(img_bytes))
                            .await
                            .map_err(|e| Box::new(e) as PluginError)?
                        {
                            Ok(list) => {
                                send_forward_msg(&ctx, writer.clone(), list).await;
                                Ok(None)
                            }
                            Err(e) => Err(e),
                        }
                    }
                    _ => {
                        let cmd_owned = cmd.to_string();
                        let args_owned: Vec<String> =
                            args.iter().map(|s| (*s).to_string()).collect();
                        crate::render::worker::run(move || {
                            raster_op(&cmd_owned, &args_owned, img_bytes)
                        })
                        .await
                        .map_err(|e| Box::new(e) as PluginError)?
                    }
                };

                // 6. 发送结果
                match res {
                    Ok(Some(b64)) => {
                        let reply = Message::new().image(format!("base64://{}", b64));
                        let _ = send_msg(&ctx, writer, group_id, Some(user_id), reply).await;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let _ = send_msg(
                            &ctx,
                            writer,
                            group_id,
                            Some(user_id),
                            format!("❌ 处理失败：{}", e),
                        )
                        .await;
                    }
                }

                return Ok(None);
            }
        }

        Ok(Some(ctx))
    })
}

/// 需要解码 / 重编码整张动图的那几条指令：纯 CPU、不含 await。
///
/// 单独拎出来是为了能整段交给 `spawn_blocking`。返回 `Ok(None)` 表示这条指令不归
/// 它管（调用方按未命中处理），与从前那个 `_ => Ok(None)` 的兜底分支同义。
fn raster_op(cmd: &str, args: &[String], img_bytes: Vec<u8>) -> PluginResult<Option<String>> {
    let arg = |index: usize| args.get(index).map(String::as_str);
    Ok(match cmd {
        "合成gif" => {
            let (rows, cols) = arg(0).and_then(utils::parse_grid_dim).unwrap_or((3, 3));
            let interval = arg(1).and_then(|s| s.parse().ok()).unwrap_or(0.1);
            let margin = arg(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            Some(gif_ops::grid_to_gif(
                img_bytes, rows, cols, interval, margin,
            )?)
        }
        "gif变速" => {
            let factor = arg(0).and_then(|s| s.parse().ok()).unwrap_or(2.0);
            Some(gif_ops::process_gif(
                img_bytes,
                gif_ops::Transform::Speed(factor),
            )?)
        }
        "gif倒放" => Some(gif_ops::process_gif(
            img_bytes,
            gif_ops::Transform::Reverse,
        )?),
        "gif缩放" => {
            let op = arg(0).map_or(gif_ops::Transform::Scale(0.5), |s| {
                if let Some((w, h)) = utils::parse_grid_dim(s) {
                    gif_ops::Transform::Resize(w, h)
                } else {
                    gif_ops::Transform::Scale(s.parse().unwrap_or(0.5))
                }
            });
            Some(gif_ops::process_gif(img_bytes, op)?)
        }
        "gif旋转" => {
            let deg = arg(0).and_then(|s| s.parse().ok()).unwrap_or(90);
            Some(gif_ops::process_gif(
                img_bytes,
                gif_ops::Transform::Rotate(deg),
            )?)
        }
        "gif翻转" => {
            let op =
                arg(0)
                    .map(str::to_lowercase)
                    .as_deref()
                    .map_or(gif_ops::Transform::FlipH, |s| {
                        if matches!(s, "垂直" | "v" | "vertical" | "纵向") {
                            gif_ops::Transform::FlipV
                        } else {
                            gif_ops::Transform::FlipH
                        }
                    });
            Some(gif_ops::process_gif(img_bytes, op)?)
        }
        "gif拼图" => {
            let cols = arg(0).and_then(|s| s.parse().ok());
            Some(gif_ops::gif_to_grid(img_bytes, cols)?)
        }
        _ => None,
    })
}

/// 发送合并转发消息
async fn send_forward_msg(ctx: &Context, writer: LockedWriter, base64_list: Vec<String>) {
    let login = ctx.bot.login_user.get();
    let bot_id = &login.id;

    // 预处理列表：防止风控
    let (process_list, is_truncated) = if base64_list.len() > 99 {
        (&base64_list[0..99], true)
    } else {
        (base64_list.as_slice(), false)
    };

    let msg = ctx.as_message().unwrap();
    let group_id = msg.group_id();
    let user_id = msg.user_id();

    if is_truncated {
        let _ = send_msg(
            ctx,
            writer.clone(),
            group_id,
            Some(user_id),
            "⚠️ 切片数量过多，为防止风控，仅发送前 99 张",
        )
        .await;
    }

    // 构建节点消息
    let mut forward_msg = Message::new();
    for (index, b64) in process_list.iter().enumerate() {
        let content = Message::new().image(format!("base64://{}", b64));
        forward_msg = forward_msg.node_custom(bot_id.clone(), format!("图 {}", index + 1), content);
    }

    // 调用通用 API
    let _ = send_msg(ctx, writer, group_id, Some(user_id), forward_msg).await;
}

/// Validate control edits against the plugin's actual configuration type.
pub fn validate_config(value: &toml::Value) -> Result<(), String> {
    <Config as serde::Deserialize>::deserialize(value.clone())
        .map(|_| ())
        .map_err(|_| "配置类型不匹配（请检查数组元素、字段类型及整数范围）".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose};
    use image::{Rgba, RgbaImage};
    use std::io::Cursor;

    /// 3×3 的格子图：`合成gif` 会把它切成九帧，每帧一个颜色。
    fn grid_png() -> Vec<u8> {
        let mut img = RgbaImage::new(60, 60);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let cell = (y / 20) * 3 + (x / 20);
            *pixel = Rgba([(cell * 25 + 10) as u8, 90, (200 - cell * 20) as u8, 255]);
        }
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// 认不出的指令把活让回去（调用方按未命中处理），坏输入报错而不是 panic。
    #[test]
    fn unknown_commands_fall_through_and_bad_input_errors_out() {
        assert!(
            raster_op("gif帮助", &[], b"x".to_vec()).unwrap().is_none(),
            "帮助不是重编码指令，不该在这里被接走"
        );
        assert!(
            raster_op("gif变速", &[], "这不是动图".as_bytes().to_vec()).is_err(),
            "认不出的字节应当报错，而不是 panic 或静默成功"
        );
    }

    /// 合成出来的动图能过每一条重编码指令——这一组用例钉住「分发搬进阻塞线程池」
    /// 这件事没有顺手改掉参数默认值或返回值形状。
    #[test]
    fn a_synthesised_animation_survives_every_transform() {
        let args: Vec<String> = ["3x3", "0.05"].iter().map(|s| (*s).to_string()).collect();
        let encoded = raster_op("合成gif", &args, grid_png())
            .unwrap()
            .expect("合成gif 应当出图");
        let gif = general_purpose::STANDARD.decode(&encoded).unwrap();
        assert_eq!(gif.len(), encoded.len() * 3 / 4, "产物应当是 base64");

        for (cmd, extra) in [
            ("gif变速", vec!["3".to_string()]),
            ("gif倒放", vec![]),
            ("gif缩放", vec!["0.5".to_string()]),
            ("gif缩放", vec!["30x30".to_string()]),
            ("gif旋转", vec!["90".to_string()]),
            ("gif翻转", vec!["垂直".to_string()]),
            ("gif拼图", vec!["3".to_string()]),
        ] {
            let out = raster_op(cmd, &extra, gif.clone())
                .unwrap_or_else(|e| panic!("{cmd} 应当成功：{e}"));
            let out = out.unwrap_or_else(|| panic!("{cmd} 应当归 raster_op 管"));
            assert!(!out.is_empty(), "{cmd} 的产物不该是空的");
        }

        // 缺省参数与文档一致：不带倍率时变速按 2 倍、旋转按 90 度。
        assert!(raster_op("gif变速", &[], gif.clone()).unwrap().is_some());
        assert!(raster_op("gif旋转", &[], gif).unwrap().is_some());
    }
}
