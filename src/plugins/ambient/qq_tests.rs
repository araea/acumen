// Runs in bridge::tests so the mock and the real server exercise the same bridge.
#[tokio::test]
async fn qq_management_is_scoped_bounded_and_deduplicated() {
    let group = -8_000_501;
    let (ctx, writer, calls, server) = fixture(group).await;
    let dir =
        crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "qq-actions").unwrap();
    let mut config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx, "ambient");
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
    config.management_groups = vec![group];
    ctx.config
        .write()
        .unwrap()
        .plugins
        .insert("ambient".into(), build_config(config.clone()));
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
            "root-delete",
            json!({"action":"group_file","operation":{"op":"delete_folder","folder_id":"/"}})
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
            .find(|(m, _)| m == "reaction.clear")
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
    config.management_groups.clear();
    ctx.config
        .write()
        .unwrap()
        .plugins
        .insert("ambient".into(), build_config(config));
    assert_eq!(
        action(&bridge, "revoked", json!({"action":"kick","user_id":"42"})).await["ok"],
        false
    );
    server.abort();
}

#[tokio::test]
async fn qq_empty_payload_and_kernel_errors_never_become_facts() {
    let group = -8_000_502;
    let (ctx, writer, calls, server) = fixture(group).await;
    let dir =
        crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "qq-query").unwrap();
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx, "ambient");
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
    for what in ["unread", "capacity"] {
        let result = request(&bridge, json!({"id":what,"op":"group","what":what})).await;
        assert_eq!(result["ok"], false, "{result}");
    }
    let next = request(
        &bridge,
        json!({"id":"next","op":"group","what":"search","query":"张","next":"20"}),
    )
    .await;
    assert_eq!(next["ok"], true);
    let history = calls.lock().unwrap().clone();
    let search = &history
        .iter()
        .find(|(m, _)| m == "internal/group_member_search")
        .unwrap()
        .1;
    assert_eq!(search["offset"], 20);
    assert_eq!(search["guild_id"], group.to_string());
    server.abort();
}

#[tokio::test]
async fn qq_file_upload_uses_multipart_and_consumes_a_message() {
    let group = -8_000_503;
    let (ctx, writer, calls, server) = fixture(group).await;
    let dir =
        crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "qq-file").unwrap();
    tokio::fs::write(dir.path().join("note.txt"), "测试文件")
        .await
        .unwrap();
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx, "ambient");
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
    let uploaded = action(&bridge, "upload", json!({"action":"group_file","operation":{"op":"upload","source":"note.txt","name":"note.txt","folder_id":"folder-1"}})).await;
    assert_eq!(uploaded["ok"], true, "{uploaded}");
    let history = calls.lock().unwrap().clone();
    assert_eq!(history[0].0, "upload.create");
    assert_eq!(history[1].0, "internal/group_file");
    assert_eq!(history[1].1["file"], "internal:red/10000/_tmp/test");
    assert_eq!(history[1].1["folder_id"], "folder-1");
    assert!(history[1].1.get("source").is_none());
    let context = request(&bridge, json!({"id":"ctx","op":"context"})).await;
    assert_eq!(
        context["result"]["messages_remaining"],
        config.max_messages - 1
    );
    server.abort();
}

/// Every call goes through the Rust bridge; the fixed sandbox is an explicit opt-in.
#[tokio::test]
#[ignore = "AYJX_AMBIENT_LIVE_GROUP=280183116；会发送并撤回测试消息、修改并恢复自己的名片、短暂禁言小号、创建并清理文件夹"]
async fn live_qq_sandbox_actions_and_environment() {
    assert_eq!(
        std::env::var("AYJX_AMBIENT_LIVE_GROUP").as_deref(),
        Ok("280183116")
    );
    let group = 280183116;
    let (ctx, _, _, mock) = fixture(group).await;
    mock.abort();
    let config_text = tokio::fs::read_to_string("config.toml").await.unwrap();
    let disk: toml::Value = toml::from_str(&config_text).unwrap();
    let connection = disk["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["protocol"].as_str() == Some("satori"))
        .unwrap();
    let endpoint = connection["url"]
        .as_str()
        .unwrap()
        .trim_end_matches('/')
        .trim_end_matches("/v1/events");
    let token = std::env::var("AYJX_SATORI_TOKEN").ok().or_else(|| {
        connection
            .get("access_token")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    });
    // Empty selectors are accepted during discovery.
    ctx.bot.login_user.set(LoginUser::default());
    let writer: LockedWriter = Arc::new(crate::adapters::satori::SatoriClient::new(
        endpoint.into(),
        token,
    ));
    let login: Value = writer.call(&ctx, "login.get", json!({})).await.unwrap();
    let me = login["user"]["id"].as_str().unwrap();
    ctx.bot.login_user.set(LoginUser {
        id: me.into(),
        ..Default::default()
    });
    let member: Value = writer
        .call(
            &ctx,
            "guild.member.get",
            json!({"guild_id":group.to_string(),"user_id":me}),
        )
        .await
        .unwrap();
    assert!(
        member["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "owner")
    );
    let old_card = member["nick"].as_str().unwrap_or("").to_string();
    let mut config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx, "ambient");
    config.management_groups = vec![group];
    config.lookup_budget = 12;
    config.send_freshness_seconds = 0;
    ctx.config
        .write()
        .unwrap()
        .plugins
        .insert("ambient".into(), build_config(config.clone()));
    let roster: Value = writer
        .call(
            &ctx,
            "guild.member.list",
            json!({"guild_id":group.to_string()}),
        )
        .await
        .unwrap();
    // Use a regular controlled member and keep the automatic unmute short even on cancellation.
    let target = roster["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["roles"]
                .as_array()
                .is_some_and(|rs| rs.iter().all(|r| r["id"] == "member"))
        })
        .unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    window::with_group(group, |state| {
        *state = Default::default();
        state.receive(Turn {
            user_id: target.parse().unwrap(),
            name: "沙盒成员".into(),
            text: "沙盒验证".into(),
            ..Default::default()
        });
    });
    let dir =
        crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "qq-live").unwrap();
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
    let context = request(&bridge, json!({"id":"context","op":"context"})).await;
    assert_eq!(context["ok"], true);
    assert_eq!(
        context["result"]["capabilities"]["management_enabled"],
        true
    );
    assert!(
        context["result"]["capabilities"]["extension_actions"]
            .as_array()
            .unwrap()
            .len()
            > 50
    );
    let mut failures = Vec::new();
    for what in [
        "capacity",
        "message_limit",
        "signin",
        "join_link",
        "apps",
        "file_info",
        "first_unread",
        "faces",
    ] {
        let out = request(&bridge, json!({"id":what,"op":"group","what":what})).await;
        println!("live query {what}: ok={}", out["ok"]);
        if out["ok"] != true {
            failures.push(format!("{what}: {out}"));
        }
    }
    // Unknown and failed kernel envelopes are covered by deterministic tests above.
    // Perform cleanup before assertions, including when a write returns an error.
    for (name, value) in [
        ("card", json!({"action":"card","card":"ayjx兼容测试"})),
        (
            "mute",
            json!({"action":"mute","user_id":target,"duration_seconds":60}),
        ),
        (
            "unmute",
            json!({"action":"mute","user_id":target,"duration_seconds":0}),
        ),
        ("mark_read", json!({"action":"mark_read"})),
    ] {
        let out = action(&bridge, name, value).await;
        println!("live action {name}: ok={}", out["ok"]);
        if out["ok"] != true {
            failures.push(format!("{name}: {out}"));
        }
        if name == "card" {
            let readback = writer
                .call::<_, Value>(
                    &ctx,
                    "guild.member.get",
                    json!({"guild_id":group.to_string(),"user_id":me}),
                )
                .await;
            if !readback.is_ok_and(|v| v["nick"] == "ayjx兼容测试") {
                failures.push("card readback mismatch".into());
            }
        }
    }
    let restored = action(
        &bridge,
        "restore-card",
        json!({"action":"card","card":old_card}),
    )
    .await;
    if restored["ok"] != true {
        failures.push(format!("restore card: {restored}"));
    }
    let sent = action(&bridge, "send", json!({"action":"send","parts":[{"type":"text","text":"ayjx 兼容验证：表态和精华测试，稍后撤回。"}]})).await;
    let mid = sent
        .pointer("/result/message_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(mid) = mid {
        for (name, value) in [
            (
                "react",
                json!({"action":"react","message_id":mid,"emoji_id":"76"}),
            ),
            (
                "react_clear",
                json!({"action":"react_clear","message_id":mid}),
            ),
            ("essence", json!({"action":"essence","message_id":mid})),
            (
                "unessence",
                json!({"action":"essence","message_id":mid,"remove":true}),
            ),
        ] {
            let out = action(&bridge, name, value).await;
            println!("live action {name}: ok={}", out["ok"]);
            if out["ok"] != true {
                failures.push(format!("{name}: {out}"));
            }
        }
        let out = action(
            &bridge,
            "recall",
            json!({"action":"recall","message_id":mid}),
        )
        .await;
        if out["ok"] != true {
            failures.push(format!("recall: {out}"));
        }
    } else {
        failures.push(format!("send receipt: {sent}"));
    }
    // Separate round for file operations so cleanup is not limited by the first round's quota.
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
    let folder_name = format!("ayjx-test-{}", chrono::Utc::now().timestamp());
    let made = action(
        &bridge,
        "mkdir",
        json!({"action":"group_file","operation":{"op":"create_folder","name":folder_name}}),
    )
    .await;
    if let Some(folder) = made
        .pointer("/result/data/folder_id")
        .and_then(Value::as_str)
    {
        let renamed = action(&bridge, "rename-folder", json!({"action":"group_file","operation":{"op":"rename_folder","folder_id":folder,"name":format!("{folder_name}-renamed")}})).await;
        if renamed["ok"] != true {
            failures.push(format!("rename folder: {renamed}"));
        }
        let deleted = action(
            &bridge,
            "rmdir",
            json!({"action":"group_file","operation":{"op":"delete_folder","folder_id":folder}}),
        )
        .await;
        if deleted["ok"] != true {
            failures.push(format!("delete folder: {deleted}"));
        }
        println!(
            "live folder create/rename/delete: {}/{}/{}",
            made["ok"], renamed["ok"], deleted["ok"]
        );
    } else {
        failures.push(format!("create folder: {made}"));
    }
    let final_card: Value = writer
        .call(
            &ctx,
            "guild.member.get",
            json!({"guild_id":group.to_string(),"user_id":me}),
        )
        .await
        .unwrap();
    assert_eq!(final_card["nick"].as_str().unwrap_or(""), old_card);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn qq_immediate_clear_reports_the_exact_compensated_scope() {
    let group = -8_000_504;
    let (ctx, writer, calls, server) = fixture(group).await;
    let dir = crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "qq-reactions")
        .unwrap();
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx, "ambient");
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

#[tokio::test]
#[ignore = "真实模型验证；所有 QQ 动作只发到本地假服务"]
async fn live_agent_uses_the_new_card_action() {
    let group = -8_000_505;
    let (ctx, writer, calls, server) = fixture(group).await;
    let dir = crate::plugins::oai::agent::ScratchDir::under(&std::env::temp_dir(), "social-live")
        .unwrap();
    // 铺开的是进程级的那几样（搭话记忆、心情、偷来的表情包库）：动同一批全局数据的
    // 用例要串行，否则一边在写、一边被重铺，谁先谁后看运气。`memory::exclusive()`
    // 就是那把锁——几样状态共用它。
    let _guard = memory::exclusive();
    super::super::setup(dir.path()).await.unwrap();
    let config = crate::plugins::get_config_or_default::<AmbientConfig>(&ctx, "ambient");
    window::with_group(group, |s| {
        let mut t = s.recent(1)[0].clone();
        t.text = "@你 把你自己在本群的群名片改成「蹲群看热闹」，改好就行，不用再发文字".into();
        t.mentions_me = true;
        t.call.at_me = true;
        *s = Default::default();
        s.receive(t);
    });
    let turns = window::with_group(group, |s| s.recent(20));
    let mut seq = 1;
    let (api_base, api_key, reply_model) = live_endpoint(&config.reply_model);
    let raw = super::super::speak::compose(
        &api_base,
        &api_key,
        &reply_model,
        dir.path(),
        &super::super::skill_dirs(dir.path()),
        super::super::PERSONA,
        &config,
        &Default::default(),
        Some(std::time::Duration::from_secs(70)),
        &turns,
        &[],
        super::super::speak::Called::Mention,
        &super::super::Scene::build(group, &config, &turns, "群友刚刚在与你正常交流".into()),
        Some((&ctx, &writer, group, &mut seq)),
    )
    .await
    .unwrap();
    let methods: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _)| m.clone())
        .collect();
    println!("模型动作选择：{methods:?}，最终正文：{raw}");
    assert!(methods.contains(&"internal/card".into()), "{methods:?}");
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|(m, p)| m == "internal/card"
                && p["card"] == "蹲群看热闹"
                && p["user_id"] == "10000")
    );
    assert!(!methods.contains(&"message.create".into()));
    assert!(raw.contains("[silent]"));
    server.abort();
}

/// 「我在这个群里是谁」走的是真平台：名片、头衔、群名与那张头像都得真取回来。
///
/// 这一步没法用 mock 证明——mock 里塞什么它就回什么，而这件事的全部价值在于
/// **平台上写的到底是什么**。所以拿沙箱群真问一遍，顺带确认判定模型收得下头像：
/// 收不下的话，头像那一句会安静地变成空串，线上看不出任何异常。只读，不改任何东西。
#[tokio::test]
#[ignore = "AYJX_AMBIENT_LIVE_GROUP=280183116；只读地问一遍自己的群身份，并让判定模型看一眼头像"]
async fn live_identity_reads_the_name_the_room_sees() {
    assert_eq!(
        std::env::var("AYJX_AMBIENT_LIVE_GROUP").as_deref(),
        Ok("280183116")
    );
    let group = 280183116;
    let (ctx, _, _, mock) = fixture(group).await;
    mock.abort();
    let disk: toml::Value = toml::from_str(&tokio::fs::read_to_string("config.toml").await.unwrap())
        .unwrap();
    let connection = disk["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["protocol"].as_str() == Some("satori"))
        .unwrap();
    let endpoint = connection["url"]
        .as_str()
        .unwrap()
        .trim_end_matches('/')
        .trim_end_matches("/v1/events");
    let token = std::env::var("AYJX_SATORI_TOKEN").ok().or_else(|| {
        connection
            .get("access_token")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    });
    ctx.bot.login_user.set(LoginUser::default());
    let writer: LockedWriter = Arc::new(crate::adapters::satori::SatoriClient::new(
        endpoint.into(),
        token,
    ));
    let login: Value = writer.call(&ctx, "login.get", json!({})).await.unwrap();
    let me: i64 = login["user"]["id"].as_str().unwrap().parse().unwrap();
    ctx.bot.login_user.set(LoginUser {
        id: me.to_string(),
        name: login["user"]["name"].as_str().map(str::to_string),
        avatar: login["user"]["avatar"].as_str().map(str::to_string),
        ..Default::default()
    });

    // 这个群的 `guild.get` 只回 id 和头像，不回群名——群名的兜底是群消息自己带的那个。
    super::super::identity::note_group_name(group, "浅金黄昏");
    let identity = super::super::identity::probe(
        &ctx,
        &writer,
        group,
        me,
        login["user"]["name"].as_str().unwrap_or_default().to_string(),
    )
    .await;
    println!("群身份：{identity:?}");
    assert_eq!(identity.user_id, me);
    // 群里看到的那个名字必须真拿到了：这一段的全部意义就是它。
    assert!(!identity.display().is_empty(), "{identity:?}");
    assert_eq!(identity.group_name, "浅金黄昏", "{identity:?}");
    assert!(identity.joined_at > 0, "{identity:?}");
    let brief = identity.brief(group, chrono::Local::now().timestamp());
    println!("注入提示词的那一段：\n{brief}");
    assert!(brief.contains(identity.display()), "{brief}");
    assert!(brief.contains(&identity.group_name), "{brief}");

    // 头像：先下下来转成模型收得下的样子，再让判定模型看一眼。
    let avatar = login["user"]["avatar"].as_str().unwrap();
    let image = super::super::vision::usable_image(avatar)
        .await
        .expect("头像下不下来或者解不开");
    // 判定模型与接口都照 config.toml 那份读：夹具里的 ctx 拿的是默认配置，
    // 而这个测试要问的恰恰是「线上这台机器配的那个模型收不收得下图」。
    let oai: crate::plugins::oai::OaiConfig =
        disk["oai"].clone().try_into().expect("[oai] 读不出来");
    let gate: AmbientConfig = disk["ambient"].clone().try_into().expect("[ambient] 读不出来");
    let (provider, model) = crate::plugins::oai::utils::split_provider(&gate.gate_model);
    // 不带供应商前缀时接口在 oai 的运行时配置里（不在 config.toml），那种配法这个
    // 测试覆盖不到——线上判定模型写的是 `deepseek/…`，接口取自 [oai.providers]。
    let (base, key) =
        crate::plugins::oai::resolve_endpoint(&oai.providers, "", "", provider.as_deref())
            .filter(|(base, key)| !base.is_empty() && !key.is_empty())
            .expect("判定模型的接口没配好：gate_model 要写成 `供应商/模型`");
    let note = super::super::identity::describe(&base, &key, &model, &image)
        .await
        .expect("判定模型看不了图：头像那一句会安静地变成空串");
    println!("头像：{note}");
    assert!(!note.is_empty());
    assert!(note.chars().count() <= 60, "{note}");
}
