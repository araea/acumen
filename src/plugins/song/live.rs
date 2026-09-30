//! 点歌前半段的真机验证：真搜 B 站、真问一次模型，不发群消息。
//!
//! 取片与发送那后半段与视频解析共用同一条路，那边已有真机用例钉着；这里
//! 只验搜索与挑选两头。要真的听一遍成品，群里发一句 `点歌 晴天` 就是了。
//!
//! ```sh
//! cargo test --bin acumen song::live -- --ignored --nocapture
//! ```

use super::*;

/// 真搜一遍：候选非空、时长折算有效，长视频已被筛掉。
#[tokio::test]
#[ignore = "真连 B 站：只搜索，不发群消息"]
async fn live_search_finds_a_pool() {
    let found = search::videos("周杰伦 晴天", 20).await.expect("搜索失败");
    assert!(!found.is_empty(), "一首传唱度这么高的歌不该搜不到");
    for candidate in found.iter().take(5) {
        println!(
            "{} · {} · {} · 播放 {}",
            candidate.bvid,
            candidate.title,
            candidate.duration_label(),
            candidate.play,
        );
        assert!(candidate.duration > 0, "{}", candidate.title);
    }
}

/// 真问一次模型： mimo 那一档要接得住，并且回话能认出编号。
#[tokio::test]
#[ignore = "真连 B 站与模型接口：只搜索与挑选，不发群消息"]
async fn live_pick_returns_a_number() {
    let pool = search::videos("周杰伦 晴天", 20)
        .await
        .expect("搜索失败");
    let max = Config::default().max_seconds;
    let pool: Vec<_> = pool
        .into_iter()
        .filter(|item| item.duration <= max)
        .take(Config::default().candidates as usize)
        .collect();
    assert!(pool.len() >= 2, "候选太少（{} 条），模型无从挑起", pool.len());

    // 模型接口不进 Context：自检直接从 config.toml 取供应商配置。
    let text = tokio::fs::read_to_string("config.toml")
        .await
        .expect("要在仓库根目录跑");
    let disk: toml::Value = toml::from_str(&text).unwrap();
    let mimo = &disk["oai"]["providers"]["mimo"];
    let base = mimo["api_base"].as_str().expect("config.toml 没有 mimo 接口");
    let key = mimo["api_key"].as_str().expect("config.toml 没有 mimo 密钥");
    let model = Config::default().model;
    let model_id = oai::utils::split_provider(&model).1;

    let history = vec![Message::User {
        content: vec![UserContent::Text(Text::new(prompt_for("周杰伦 晴天", &pool)))],
    }];
    let reply = llm::complete(base, key, &model_id, history, None)
        .await
        .expect("模型请求失败");
    println!("模型回话：{reply}");
    let index = parse_choice(&reply, pool.len()).expect("回话里认不出编号");
    println!(
        "选中：{} · {} · {}",
        pool[index].title,
        pool[index].author,
        pool[index].duration_label(),
    );
}
