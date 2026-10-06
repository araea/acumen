use crate::adapters::satori::{LockedWriter, send_msg};
use crate::command::{extract_text_arg, get_image_url, match_command};
use crate::event::Context;
use crate::http::download_bytes;
use crate::message::Message;
use crate::plugins::{PluginConfig, PluginError, PluginResult};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

pub mod gif_ops;
pub mod utils;

// =============================
//      Main Plugin Logic
// =============================

/// 帮助信息
const HELP_TEXT: &str = r"💡 指令列表（大小写均可）

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

使用时请附带图片或引用图片消息";

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
pub struct Config {
    enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl PluginConfig for Config {
    const NAME: &'static str = "gif";
}


pub fn handle(
    ctx: Context,
    writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        let Some(msg) = ctx.as_message() else { return Ok(Some(ctx)) };

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
                    send_msg(&ctx, writer, group_id, Some(user_id), help).await?;
                    return Ok(None);
                }

                // 4. 获取图片
                let Some(img_url) = get_image_url(
                    &ctx,
                    writer.clone(),
                    &matched.args,
                    matched.reply_id.as_ref(),
                )
                .await else {
                    send_msg(
                        &ctx,
                        writer,
                        group_id,
                        Some(user_id),
                        "❌ 请附带图片或引用图片消息",
                    )
                    .await?;
                    return Ok(None);
                };

                let img_bytes = match download_bytes(&img_url).await {
                    Ok(b) => b,
                    Err(e) => {
                        send_msg(
                            &ctx,
                            writer,
                            group_id,
                            Some(msg.user_id()),
                            format!("❌ 图片下载失败：{e}"),
                        )
                        .await?;
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
                                send_forward_msg(&ctx, writer.clone(), list).await?;
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
                        let reply = Message::new().image(format!("base64://{b64}"));
                        send_msg(&ctx, writer, group_id, Some(user_id), reply).await?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        send_msg(
                            &ctx,
                            writer,
                            group_id,
                            Some(user_id),
                            format!("❌ 处理失败：{e}"),
                        )
                        .await?;
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
async fn send_forward_msg(
    ctx: &Context,
    writer: LockedWriter,
    base64_list: Vec<String>,
) -> Result<(), PluginError> {
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
        send_msg(
            ctx,
            writer.clone(),
            group_id,
            Some(user_id),
            "⚠️ 切片数量过多，为防止风控，仅发送前 99 张",
        )
        .await?;
    }

    // 构建节点消息
    let mut forward_msg = Message::new();
    for (index, b64) in process_list.iter().enumerate() {
        let content = Message::new().image(format!("base64://{b64}"));
        forward_msg = forward_msg.node_custom(bot_id.clone(), format!("图 {}", index + 1), content);
    }

    // 调用通用 API
    send_msg(ctx, writer, group_id, Some(user_id), forward_msg).await
}

