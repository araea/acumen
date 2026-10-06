//! 手动导出音频：引用一条视频消息发「导出音频」，画面去掉、声音留下进群。
//!
//! 点歌与视频解析的成品是一条视频气泡；想只留声音时不必再搜一遍——引用那条视频
//! 发一句「导出音频」，本模块把音轨抽出来，作为一个音频文件发回群里。取到的
//! 引用消息里认得 `video` 元素就行，不挑来源：机器人取的片、群友发的视频都能导。
//!
//! 与点歌主流程不同，这是一条手动指令：成不了要在群里回一句原因（口径与
//! 「转链接」一致），不让人对着一条没下文的引用干等。

use crate::adapters::satori::{LockedWriter, api, send_msg};
use crate::command::CommandMatch;
use crate::event::Context;
use crate::message::Message;
use crate::plugins::oai::utils::safe_file_name;
use crate::plugins::video_parse;
use crate::plugins::{get_config_or_default, get_data_dir};
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use simd_json::base::ValueAsScalar;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::time;

use super::LOG_TARGET;

/// 触发词。
pub(super) const COMMANDS: &[&str] = &["导出音频", "提取音频"];

/// 下载单条预算：引用的是群里现成的片子，与取片同量级。
const DOWNLOAD_BUDGET: Duration = Duration::from_secs(180);
/// ffmpeg 抽音轨的预算：流拷贝是秒级，重编码一首歌也就几十秒。
const FFMPEG_BUDGET: Duration = Duration::from_secs(300);

// ================= Main =================

/// 一条「导出音频」指令的完整流程：取引用 → 下载 → 抽音轨 → 文件进群。
///
/// 返回 `Err` 时原因已经是一句人话，调用方除了记日志还要把它回进群里。
pub(super) async fn run(ctx: &Context, writer: &LockedWriter, matched: &CommandMatch) -> Result<()> {
    let msg = ctx.as_message().ok_or_else(|| anyhow!("不是消息事件"))?;
    let reply_id = matched.reply_id.as_deref().ok_or_else(|| {
        anyhow!("没有引用消息：先引用那条视频，再发「导出音频」")
    })?;

    // 引用消息里的视频元素：`url` 已按资源链接规范改写成可直接 GET 的地址
    // （`internal:` 走实现端代理），`name` 是实现端带来的原始文件名，未必有。
    let quoted = api::get_msg(ctx, writer.clone(), reply_id)
        .await
        .map_err(|error| anyhow!("引用的消息取不到：{error}"))?;
    let (url, title) = quoted
        .message
        .0
        .iter()
        .find_map(|seg| {
            if seg.type_.as_str() != "video" {
                return None;
            }
            let url = seg
                .data
                .get("url")
                .and_then(|value| value.as_str())
                .map(str::to_string)?;
            let title = seg
                .data
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            Some((url, title))
        })
        .ok_or_else(|| anyhow!("引用的消息里没有视频"))?;

    // 体积上限沿用视频解析的口径：同一条片子的另一份用途，不该有两套尺度。
    let take = get_config_or_default::<video_parse::Config>(ctx);
    let cap = take.max_size_mb.clamp(1, 2048) * 1_048_576;

    let dir = get_data_dir("song").await.map_err(|error| anyhow!("{error}"))?;
    let mut scratch = Scratch::default();
    let input = dir.join(format!("export-{:032x}.part", rand::random::<u128>()));
    scratch.push(input.clone());
    let output = dir.join(format!("export-{:032x}.m4a", rand::random::<u128>()));
    scratch.push(output.clone());

    // 与取片共用同一个闸门：下载、抽轨、上传都不是白给的，别跟取片挤在一起。
    let _permit = video_parse::TAKE_GATE
        .acquire()
        .await
        .map_err(|_| anyhow!("取片闸门不可用"))?;

    let size = download(&url, &input, cap).await?;
    extract_audio(&input, &output).await?;

    let file_name = output_name(&title);
    let bytes = tokio::fs::read(&output).await?;
    let uploaded = writer
        .upload(ctx, bytes, &file_name, "audio/mp4")
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let resource = uploaded
        .get("file")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("上传没有返回资源"))?
        .to_string();

    // 音频文件是「顺媒体」，只能单独成条，不带引用也不带文字（同条引用会把它
    // 顶成空气泡，见视频解析的 `send`）。
    send_msg(
        ctx,
        writer.clone(),
        msg.group_id(),
        Some(msg.user_id()),
        Message::new().file(resource, Some(file_name.clone())),
    )
    .await
    .map_err(|error| anyhow!("{error}"))?;

    info!(
        target: LOG_TARGET,
        "已导出音频：{}（{:.1} MB）",
        file_name,
        size as f64 / 1_048_576.0,
    );
    Ok(())
}

// ================= 下载 =================

/// 把引用视频落成本地文件，边写边按上限收口。地址来自实现端自己的代理路由，
/// 不需要站点之类的头。
async fn download(url: &str, path: &Path, cap: u64) -> Result<u64> {
    let response = crate::http::client()
        .get(url)
        .timeout(DOWNLOAD_BUDGET)
        .send()
        .await?
        .error_for_status()?;
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(path).await?;
    let mut total = 0u64;

    let attempt = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            total += chunk.len() as u64;
            if total > cap {
                return Err(anyhow!("视频超过大小上限（{} MB）", cap / 1_048_576));
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(total)
    };

    match time::timeout(DOWNLOAD_BUDGET, attempt).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("下载超时")),
    }
}

// ================= 抽音轨 =================

/// Termux 的 PATH 在 runit 服务环境里未必带 `bin`，从 `PREFIX` 直接定位。
fn ffmpeg_program() -> String {
    std::env::var("PREFIX").map_or_else(|_| "ffmpeg".to_string(), |prefix| format!("{prefix}/bin/ffmpeg"))
}

/// 抽音轨：先流拷贝（无损、秒级），音轨编码进不了 m4a 容器时再重编码一次。
async fn extract_audio(input: &Path, output: &Path) -> Result<()> {
    let program = ffmpeg_program();
    let codecs: &[&[&str]] = &[&["-c:a", "copy"], &["-c:a", "aac", "-b:a", "192k"]];
    for codec in codecs {
        let mut command = tokio::process::Command::new(&program);
        command
            .arg("-y")
            .arg("-i")
            .arg(input)
            .arg("-vn")
            .args(*codec)
            .arg(output)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let status = time::timeout(FFMPEG_BUDGET, command.status())
            .await
            .map_err(|_| anyhow!("ffmpeg 超时"))?
            .map_err(|error| anyhow!("起不了 ffmpeg：{error}"))?;
        if status.success() && usable_audio(output) {
            return Ok(());
        }
        let _ = tokio::fs::remove_file(output).await;
    }
    anyhow::bail!("ffmpeg 抽不出音轨")
}

/// 成品至少要像样：空文件与几字节的残骸都不算数。
fn usable_audio(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.len() > 1024)
}

/// 音频文件的名字：引用元素带了原始文件名就接着用（换掉扩展名），没有就按
/// 时间起一个。
fn output_name(title: &str) -> String {
    let stem = Path::new(title)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty());
    let stem = match stem {
        Some(stem) => stem.to_string(),
        None => chrono::Utc::now().format("audio-%Y%m%d-%H%M%S").to_string(),
    };
    format!("{}.m4a", safe_file_name(&stem))
}

// ================= 清理 =================

/// 本单落在本地的文件（原始视频与抽出的音轨），析构时删掉。
///
/// 下载超时、抽轨失败、上传失败，每一条都是提前返回；清理挂在值的生命周期上，
/// 返回路径怎么写都不会漏。
#[derive(Default)]
struct Scratch {
    paths: Vec<PathBuf>,
}

impl Scratch {
    fn push(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // 析构里不能 await；一次 unlink 是微秒级，直接同步做掉。
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}
