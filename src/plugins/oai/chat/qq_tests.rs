// Runs in bridge::tests so the mock and the real server exercise the same bridge.
#[tokio::test]
async fn qq_management_is_scoped_bounded_and_deduplicated() {
    let group = "-8000501";
    let (ctx, writer, calls, server) = fixture(group).await;
    let dir =
        crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "qq-actions").unwrap();
    let mut config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx);
    let bridge = start(
        &ctx,
        &writer,
        group,
        window::with_group(group, |s| s.seq),
        &config,
        dir.path(),
        dir.path(),
    )
    .await
    .unwrap();
    let blocked = action(
        &bridge,
        "blocked",
        json!({"action":"mute","user_id":"42","duration_seconds":60}),
    )
    .await;
    assert_eq!(blocked["ok"], false);
    assert!(calls.lock().unwrap().is_empty());
    // 管理动作的授权在开一轮时就定下了：授权改完得重开一轮（一轮只活几十秒）。
    config.management_groups = vec![group.to_string()];
    let bridge = start(&ctx, &writer, group, 0, &config, dir.path(), dir.path())
        .await
        .unwrap();
    let mute = json!({"action":"mute","user_id":"42","duration_seconds":60});
    let first = action(&bridge, "mute", mute.clone()).await;
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first, action(&bridge, "mute", mute).await);
    assert_eq!(
        action(
            &bridge,
            "lift",
            json!({"action":"mute","user_id":"42","duration_seconds":0})
        )
        .await["ok"],
        true
    );
    assert_eq!(
        action(
            &bridge,
            "too-long",
            json!({"action":"mute","user_id":"42","duration_seconds":2592001})
        )
        .await["ok"],
        false
    );
    assert_eq!(
        action(
            &bridge,
            "other-group",
            json!({"action":"mute","guild_id":"999","user_id":"42","duration_seconds":60})
        )
        .await["ok"],
        false
    );
    assert_eq!(
        action(&bridge, "kick", json!({"action":"kick","user_id":"42"})).await["ok"],
        true
    );
    assert_eq!(
        action(
            &bridge,
            "self-card",
            json!({"action":"card","card":"新名片"})
        )
        .await["ok"],
        true
    );
    assert_eq!(
        action(
            &bridge,
            "clear",
            json!({"action":"react_clear","message_id":"123"})
        )
        .await["ok"],
        true
    );
    assert_eq!(
        action(
            &bridge,
            "over-long-mute",
            json!({"action":"mute_all","duration_seconds":31*86400})
        )
        .await["ok"],
        false
    );
    let history = calls.lock().unwrap().clone();
    let mutes: Vec<_> = history
        .iter()
        .filter(|(m, _)| m == "guild.member.mute")
        .collect();
    assert_eq!(mutes.len(), 2);
    assert_eq!(mutes[0].1["duration"], 60000);
    assert_eq!(mutes[1].1["duration"], 0);
    assert_eq!(mutes[0].1["guild_id"], group.to_string());
    assert_eq!(
        history
            .iter()
            .find(|(m, _)| m == "guild.member.kick")
            .unwrap()
            .1["permanent"],
        false
    );
    assert_eq!(
        history
            .iter()
            .find(|(m, _)| m == "internal/card")
            .unwrap()
            .1["user_id"],
        "10000"
    );
    assert!(
        history
            .iter()
            .find(|(m, _)| m == "internal/reaction_clear")
            .unwrap()
            .1
            .get("emoji_id")
            .is_none()
    );
    assert_eq!(
        action(
            &bridge,
            "quiet-member",
            json!({"action":"mute","user_id":"43","duration_seconds":60})
        )
        .await["ok"],
        true
    );
    assert_eq!(
        action(
            &bridge,
            "not-a-member",
            json!({"action":"mute","user_id":"999","duration_seconds":60})
        )
        .await["ok"],
        false
    );
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|(method, body)| method == "guild.member.mute" && body["user_id"] == "999")
    );
    // 撤回授权之后重开一轮：授权在开一轮时定下，改完立刻生效。
    config.management_groups.clear();
    let bridge = start(&ctx, &writer, group, 0, &config, dir.path(), dir.path())
        .await
        .unwrap();
    assert_eq!(
        action(&bridge, "revoked", json!({"action":"kick","user_id":"42"})).await["ok"],
        false
    );
    server.abort();
}

#[tokio::test]
async fn qq_immediate_clear_reports_the_exact_compensated_scope() {
    let group = "-8000504";
    let (ctx, writer, calls, server) = fixture(group).await;
    let dir = crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "qq-reactions")
        .unwrap();
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx);
    let bridge = start(
        &ctx,
        &writer,
        group,
        window::with_group(group, |s| s.seq),
        &config,
        dir.path(),
        dir.path(),
    )
    .await
    .unwrap();
    for emoji in ["76", "14"] {
        assert_eq!(
            action(
                &bridge,
                emoji,
                json!({"action":"react","message_id":"123","emoji_id":emoji})
            )
            .await["ok"],
            true
        );
    }
    let cleared = action(
        &bridge,
        "clear",
        json!({"action":"react_clear","message_id":"123"}),
    )
    .await;
    assert_eq!(cleared["ok"], true, "{cleared}");
    assert_eq!(cleared["result"]["status"], "partial");
    assert_eq!(cleared["result"]["cleared"], json!(["76", "14"]));
    assert_eq!(cleared["result"]["scope"], "this_turn");
    assert_eq!(
        cleared,
        action(
            &bridge,
            "clear",
            json!({"action":"react_clear","message_id":"123"})
        )
        .await
    );
    let history = calls.lock().unwrap().clone();
    assert_eq!(
        history
            .iter()
            .filter(|(m, _)| m == "reaction.delete")
            .count(),
        2
    );
    server.abort();
}
