use super::*;
use serde_json::Value;
fn queue_account_fixture(root: &Path, independent: bool) -> AccountStatus {
    let main = automatic_account_fixture(root, "11");
    if independent {
        let staged = tempfile::tempdir_in(root).unwrap();
        automatic_account_fixture(staged.path(), "22");
        let id = uuid::Uuid::new_v4();
        let directory = root.join("AssistantAccounts");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::copy(
            staged.path().join("account.json"),
            directory.join(format!("{id}.json")),
        )
        .unwrap();
        std::fs::write(
            root.join("assistant-account.json"),
            serde_json::to_vec(&serde_json::json!({
                "mode": "independent", "account": id
            }))
            .unwrap(),
        )
        .unwrap();
    }
    main
}

#[tokio::test]
async fn account_queues_allow_independent_senders_between_segments_but_serialize_reuse() {
    for (independent, assistant_first) in [(true, true), (true, false), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let status = queue_account_fixture(root.path(), independent);
        let (mut app, request, _) = setup(root.path());
        app.account_status = status;
        let (tx, mut rx) = mpsc::channel(32);
        app.local_transport = Some(Arc::new(LocalTransport {
            outcome: AtomicU8::new(0),
            log: root.path().join("deliveries.jsonl"),
            tx: tx.clone(),
        }));
        let reply = app.bridge.candidates()[0].clone();
        app.bridge.approve_candidates(&[reply]).unwrap();
        let mut job = Some(app.bridge.take_ready().unwrap());
        app.enqueue(
            vec!["first-1".into(), "first-2".into()],
            if assistant_first { job.take() } else { None },
            tx.clone(),
        );
        let mut echoes = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut completed = 0;
            while completed < 2 {
                let event = rx.recv().await.unwrap();
                if let UiEvent::LocalEcho(text) = &event {
                    echoes.push(text.clone());
                    if text == "first-1" {
                        app.enqueue(vec!["other-account".into()], job.take(), tx.clone());
                    }
                }
                if matches!(event, UiEvent::DeliveryCompleted) {
                    completed += 1;
                }
                app.handle_ui_event(event);
            }
        })
        .await
        .unwrap();
        let expected = if independent {
            vec!["first-1", "other-account", "first-2"]
        } else {
            vec!["first-1", "first-2", "other-account"]
        };
        assert_eq!(
            echoes, expected,
            "independent={independent}, assistant_first={assistant_first}"
        );
        assert_eq!(result(&app, &request)["state"], "confirmed");
        assert_eq!(app.input, "人工草稿甲🙂乙");
    }
}

#[tokio::test]
async fn account_queues_manual_rejection_does_not_cancel_independent_assistant() {
    let root = tempfile::tempdir().unwrap();
    let status = queue_account_fixture(root.path(), true);
    let (mut app, request, _) = setup(root.path());
    app.account_status = status;
    let (tx, mut rx) = mpsc::channel(32);
    let log = root.path().join("deliveries.jsonl");
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(5),
        log: log.clone(),
        tx: tx.clone(),
    }));
    let reply = app.bridge.candidates()[0].clone();
    app.bridge.approve_candidates(&[reply]).unwrap();
    let job = app.bridge.take_ready().unwrap();
    app.enqueue(vec!["manual-rejected".into()], None, tx.clone());
    tokio::task::yield_now().await;
    app.enqueue(vec!["independent-reply".into()], Some(job), tx);
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut completed = 0;
        while completed < 2 {
            let event = rx.recv().await.unwrap();
            if matches!(
                event,
                UiEvent::DeliveryCompleted | UiEvent::DeliveryNotice(_)
            ) {
                completed += 1;
            }
            app.handle_ui_event(event);
        }
    })
    .await
    .unwrap();
    assert_eq!(result(&app, &request)["state"], "confirmed");
    let rows: Vec<Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|row| (
                row["text"].as_str().unwrap(),
                row["outcome"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("manual-rejected", "Rejected"),
            ("independent-reply", "Confirmed")
        ]
    );
}

#[tokio::test]
async fn delivery_archive_failure_preserves_confirmation_and_revokes_future_sends() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    let (tx, mut rx) = mpsc::channel(32);
    let log = root.path().join("deliveries.jsonl");
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(0),
        log: log.clone(),
        tx: tx.clone(),
    }));
    let reply = app.bridge.candidates()[0].clone();
    app.bridge
        .approve_candidates(std::slice::from_ref(&reply))
        .unwrap();
    let job = app.bridge.take_ready().unwrap();
    app.enqueue(assistant_segments(&job.reply.text), Some(job), tx);
    let blocked = root.path().join("occupied-journal-root");
    std::fs::write(&blocked, "preserve-existing-file").unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = rx.recv().await {
            if matches!(&event, UiEvent::AssistantDelivery { .. }) {
                app.journal = SessionJournal::new(blocked.clone());
                app.handle_ui_event(event);
                break;
            }
            app.handle_ui_event(event);
        }
    })
    .await
    .unwrap();
    assert_eq!(result(&app, &request)["state"], "confirmed");
    assert!(!app.bridge.sending_enabled());
    assert!(!app.runner.running);
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    assert_eq!(
        std::fs::read_to_string(&blocked).unwrap(),
        "preserve-existing-file"
    );
    assert!(app.notice.contains("归档失败"));
}

#[cfg(unix)]
#[tokio::test]
async fn completed_round_archive_failure_revokes_sending_without_losing_manual_input() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("fixture.py");
    std::fs::write(&binary, include_str!("../../tests/fixtures/acp_agent.py")).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.path().join("fixture.json"), r#"{"mode":"no_answer"}"#).unwrap();
    let mut app = replay::app(root.path(), DanmuSession::new("local"));
    app.bridge.available(true);
    app.runner.settings.binary = binary;
    app.input = "保留人工草稿".into();
    let (tx, _rx) = mpsc::channel(8);
    let delivery_log = root.path().join("deliveries.jsonl");
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(0),
        log: delivery_log.clone(),
        tx: tx.clone(),
    }));
    app.runner.start(&app.bridge, false, false).unwrap();
    app.bridge.permission(true).unwrap();
    app.bridge
        .ingest(DanmuEvent::new(DanmuEventKind::Danmu, "请解释事务隔离"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.runner.round_record.is_none() {
            app.runner.tick(&app.bridge);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        app.runner.round_record.as_ref().unwrap()["outcome"],
        "no_reply"
    );
    app.runner
        .record_routing(&app.bridge, &app.journal, &app.session, true)
        .unwrap();
    let blocked = root.path().join("occupied-journal-root");
    std::fs::write(&blocked, "preserve-existing-file").unwrap();
    app.journal = SessionJournal::new(blocked.clone());
    app.advance_bridge(tx);
    assert!(!app.runner.running && !app.bridge.sending_enabled());
    assert_eq!(app.input, "保留人工草稿");
    assert!(!delivery_log.exists());
    assert_eq!(
        std::fs::read_to_string(blocked).unwrap(),
        "preserve-existing-file"
    );
    app.runner.shutdown(&app.bridge).await;
}

#[test]
fn compact_assistant_only_becomes_ready_after_acp_session_is_connected() {
    let root = tempfile::tempdir().unwrap();
    let mut app = replay::app(root.path(), DanmuSession::new("local"));

    assert!(assistant::compact(&app, 16).content.is_empty());
    app.resume_pending = true;
    assert!(assistant::compact(&app, 16).content.contains("AI启动中"));
    app.pause_assistant();
    assert!(assistant::compact(&app, 16).content.is_empty());

    app.runner.running = true;
    app.runner.report.authentication.push("浏览器登录".into());
    let first = assistant::compact(&app, 16);
    assert!(first.content.contains("AI启动中"));
    assert_eq!(first.style.fg, Some(Color::Cyan));

    app.animation_tick += 1;
    let next = assistant::compact(&app, 16);
    assert!(next.content.contains("AI启动中"));
    assert_ne!(first.content, next.content);

    app.runner.report.session = Some("restoring-session".into());
    assert!(assistant::compact(&app, 16).content.contains("AI启动中"));

    app.runner.report.connected = true;
    app.runner.report.session = None;
    assert!(assistant::compact(&app, 16).content.contains("AI启动中"));

    app.runner.report.session = Some("ready-session".into());
    app.runner.settings.automatic = false;
    let ready = assistant::compact(&app, 16);
    assert_eq!(ready.content.as_ref(), crate::bridge::AI_PREFIX);
    assert_eq!(ready.style.fg, Some(Color::Green));

    app.runner.running = false;
    assert!(assistant::compact(&app, 16).content.is_empty());
}
#[cfg(unix)]
#[tokio::test]
async fn compact_startup_animation_stops_and_preserves_the_real_connection_error() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("fixture.py");
    std::fs::write(&binary, include_str!("../../tests/fixtures/acp_agent.py")).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.path().join("fixture.json"), r#"{"mode":"eof"}"#).unwrap();
    let mut app = replay::app(root.path(), DanmuSession::new("local"));
    app.bridge.available(true);
    app.runner.settings.binary = binary;
    let (tx, _rx) = mpsc::channel(8);
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(0),
        log: root.path().join("deliveries.jsonl"),
        tx: tx.clone(),
    }));

    app.runner.start(&app.bridge, false, false).unwrap();
    assert!(assistant::compact(&app, 16).content.contains("AI启动中"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.runner.running {
            app.advance_bridge(tx.clone());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    assert!(app.runner.failed());
    assert!(assistant::compact(&app, 16).content.is_empty());
    assert!(app.notice.contains(&app.runner.note));
    app.runner.shutdown(&app.bridge).await;
}

#[tokio::test]
async fn batch_review_keys_only_change_pending_candidates_and_preserve_the_draft() {
    for (key, expected) in [('a', "accepted"), ('d', "cancelled")] {
        let root = tempfile::tempdir().unwrap();
        let (mut app, first, message) = setup(root.path());
        let second = candidate(&app, "a", "two", &message);
        let uncertain = candidate(&app, "a", "uncertain", &message);
        let shown = app
            .bridge
            .candidates()
            .into_iter()
            .find(|r| r.request_id == "uncertain")
            .unwrap();
        app.bridge
            .approve_candidates(std::slice::from_ref(&shown))
            .unwrap();
        let flight = app.bridge.take_ready().unwrap();
        app.bridge
            .complete(&flight.reply, Outcome::Uncertain.into());
        let (tx, _rx) = mpsc::channel(8);
        app.open_candidate_review().unwrap();
        paint(&mut app);
        app.handle_key(
            KeyEvent::new_with_kind(
                KeyCode::Char(key),
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Repeat,
            ),
            tx.clone(),
        )
        .await
        .unwrap();
        assert_eq!(result(&app, &first)["state"], "awaiting_approval");
        app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(result(&app, &first)["state"], expected);
        assert_eq!(result(&app, &second)["state"], expected);
        assert_eq!(result(&app, &uncertain)["state"], "uncertain");
        assert!(!app.bridge.sending_enabled());
        assert_eq!(app.input, "人工草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
    }
}

#[tokio::test]
async fn bulk_send_rejects_an_identity_change_since_the_review_was_shown() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    app.bridge.permission(false).unwrap();
    app.open_candidate_review().unwrap();
    paint(&mut app);
    app.assistant_accounts.main_changed().unwrap();
    let (tx, _rx) = mpsc::channel(8);
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), tx)
        .await
        .unwrap();
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    assert!(app.bridge.take_ready().is_none());
    assert!(!app.bridge.sending_enabled());
}

#[tokio::test]
async fn command_search_cannot_approve_a_reply_hidden_below_it() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    app.bridge.permission(false).unwrap();
    let (tx, _rx) = mpsc::channel(8);
    paint(&mut app);
    app.handle_key(
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        tx.clone(),
    )
    .await
    .unwrap();
    paint(&mut app);
    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        tx.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    assert!(app.bridge.take_ready().is_none());
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
        .await
        .unwrap();
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(app.input.cursor(), 3);
}

#[tokio::test]
async fn fresh_review_press_reopens_after_a_missing_release_without_repeating() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    let reply = app.bridge.candidates()[0].clone();
    app.bridge
        .edit_candidate(
            &reply.session,
            &reply.caller,
            &reply.request_id,
            &"待审核长回复".repeat(30),
        )
        .unwrap();
    app.bridge.permission(false).unwrap();
    let (tx, _rx) = mpsc::channel(8);
    let press = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    paint(&mut app);
    app.handle_key(press, tx.clone()).await.unwrap();
    assert!(app.assistant_panel.is_some());
    paint(&mut app);
    app.handle_key(
        KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
            crossterm::event::KeyEventKind::Repeat,
        ),
        tx.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
        .await
        .unwrap();
    paint(&mut app);
    app.handle_key(press, tx).await.unwrap();
    assert!(
        app.assistant_panel.is_some(),
        "新的按下事件不能被丢失的松键永久锁住"
    );
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    assert_eq!(app.input, "人工草稿甲🙂乙");
}
#[test]
fn mouse_clicks_cannot_open_or_send_a_visible_candidate() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    paint(&mut app);
    for row in 0..36 {
        for column in 0..120 {
            for kind in [
                MouseEventKind::Down(crossterm::event::MouseButton::Left),
                MouseEventKind::Up(crossterm::event::MouseButton::Left),
            ] {
                app.handle_mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                });
            }
        }
    }
    assert!(app.assistant_panel.is_none());
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(app.input.cursor(), 3);
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    assert!(app.bridge.take_ready().is_none());
}

#[tokio::test]
async fn review_uses_only_the_seen_version_and_one_fresh_press() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, message) = setup(root.path());
    app.bridge.permission(false).unwrap();
    let submit = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    assert!(app.review_key(submit));
    assert!(app.bridge.take_ready().is_none(), "未绘制任何帧不能批准");
    paint(&mut app);
    app.bridge.permission(true).unwrap();
    let second = candidate(&app, "b", "two", &message);
    app.bridge.permission(false).unwrap();
    assert!(app.review_key(submit));
    let first = app.bridge.take_ready().unwrap();
    assert_eq!(first.reply.request_id, "one");
    assert_eq!(result(&app, &second)["state"], "awaiting_approval");
    assert!(!app.bridge.sending_enabled(), "一次审核不能授予自动权限");
    assert!(app.review_key(submit));
    assert!(app.bridge.take_ready().is_none(), "新候选须先重新绘制");
    paint(&mut app);
    app.next_candidate();
    assert!(app.approve_review().is_err(), "发送中不能切换并批准下一条");
    assert!(app.review_key(KeyEvent::new_with_kind(
        KeyCode::Enter,
        KeyModifiers::SHIFT,
        crossterm::event::KeyEventKind::Repeat
    )));
    assert!(
        app.bridge.take_ready().is_none(),
        "长按重复事件不能批准下一条"
    );
    app.bridge.complete(&first.reply, Outcome::Confirmed.into());
    paint(&mut app);
    let current = app.bridge.candidates()[0].clone();
    app.bridge
        .edit_candidate(
            &current.session,
            &current.caller,
            &current.request_id,
            "临时改动",
        )
        .unwrap();
    app.bridge
        .edit_candidate(
            &current.session,
            &current.caller,
            &current.request_id,
            &current.text,
        )
        .unwrap();
    assert!(app.review_key(submit));
    assert!(
        app.bridge.take_ready().is_none(),
        "改回原文也不复用旧版本审核"
    );
    paint(&mut app);
    app.assistant_accounts.main_changed().unwrap();
    assert!(app.review_key(submit));
    assert!(
        app.bridge.take_ready().is_none(),
        "发送身份代次改变后旧动作失效"
    );
    paint(&mut app);
    assert!(app.review_key(submit));
    assert_eq!(app.bridge.take_ready().unwrap().reply.request_id, "two");
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(app.input.cursor(), 3);
    assert_eq!(result(&app, &request)["state"], "confirmed");
}

#[tokio::test]
async fn long_review_must_be_expanded_seen_and_reseen_after_edit() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    app.bridge.permission(false).unwrap();
    let original = app.bridge.candidates()[0].clone();
    app.bridge
        .edit_candidate(
            &original.session,
            &original.caller,
            &original.request_id,
            &"需要逐页审核的正文".repeat(140),
        )
        .unwrap();
    paint(&mut app);
    app.review_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert!(app.assistant_panel.is_some());
    assert!(app.bridge.take_ready().is_none());
    paint(&mut app);
    assert!(app.approve_review().is_err(), "未看完完整正文不能批准");
    let (tx, _rx) = mpsc::channel(8);
    for _ in 0..64 {
        key(&mut app, KeyCode::PageDown, &tx).await;
    }
    paint(&mut app);
    app.edit_review().unwrap();
    app.assistant_panel = None;
    app.input.clear();
    app.handle_paste("编辑后的短正文\r\n/quit\u{3}");
    assert_eq!(app.input, "编辑后的短正文 /quit");
    assert!(!app.quit_requested);
    assert!(app.bridge.take_ready().is_none(), "粘贴不得批准候选");
    app.review_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert!(app.candidate_edit.is_none());
    assert!(app.assistant_panel.is_some());
    assert!(app.bridge.take_ready().is_none(), "编辑保存不是发送授权");
    paint(&mut app);
    app.approve_review().unwrap();
    assert_eq!(
        app.bridge.take_ready().unwrap().reply.text,
        "编辑后的短正文 /quit"
    );
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(result(&app, &request)["state"], "sending");
}

#[cfg(unix)]
#[tokio::test]
async fn sdk_candidate_reaches_only_approved_local_transport_and_unknown_never_retries() {
    use std::os::unix::fs::PermissionsExt;
    for outcome in [0, 1] {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("fixture.py");
        std::fs::write(&binary, include_str!("../../tests/fixtures/acp_agent.py")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.path().join("fixture.json"), r#"{"mode":"normal"}"#).unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.bridge.available(true);
        app.runner.settings.binary = binary;
        app.input = "保留人工草稿".into();
        let (tx, mut rx) = mpsc::channel(32);
        let log = root.path().join("deliveries.jsonl");
        app.local_transport = Some(Arc::new(LocalTransport {
            outcome: AtomicU8::new(outcome),
            log: log.clone(),
            tx: tx.clone(),
        }));
        app.runner.start(&app.bridge, false, false).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "受控问题");
        let message = event.id.clone();
        app.ingest_event(event);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                app.runner.tick(&app.bridge);
                if !app.bridge.candidates().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "{error}: {}; wire={}",
                app.runner.note,
                std::fs::read_to_string(root.path().join("wire.jsonl")).unwrap_or_default()
            )
        });
        let reply = app.bridge.candidates()[0].clone();
        app.advance_bridge(tx.clone());
        assert!(!log.exists());
        app.runner.pause(&app.bridge);
        assert_eq!(app.bridge.candidates()[0].text, "受控候选");
        assert!(
            app.bridge
                .decide(&reply.caller, &reply.request_id, true)
                .is_err()
        );
        paint(&mut app);
        app.approve_review().unwrap();
        assert!(!app.bridge.sending_enabled());
        app.advance_bridge(tx.clone());
        let query = bridge::Request::Result {
            session: reply.session.clone(),
            caller: reply.caller.clone(),
            request_id: reply.request_id.clone(),
        };
        let expected = if outcome == 0 {
            "confirmed"
        } else {
            "uncertain"
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while app.bridge.apply(query.clone()).unwrap()["state"] != expected {
                app.handle_ui_event(rx.recv().await.unwrap());
            }
        })
        .await
        .unwrap();
        if outcome == 1 {
            paint(&mut app);
            app.review_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
            paint(&mut app);
            assert!(
                app.approve_review().is_err(),
                "不确定结果不可复用原授权重发"
            );
            assert!(app.bridge.take_ready().is_none());
            app.next_candidate();
        }
        for _ in 0..3 {
            app.advance_bridge(tx.clone());
        }
        let rows: Vec<Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["text"], "✦ 受控候选");
        assert_eq!(app.input, "保留人工草稿");
        assert_eq!(
            app.bridge.mark(&message),
            Some(if outcome == 0 {
                bridge::Mark::Confirmed
            } else {
                bridge::Mark::Failed
            })
        );
        app.runner.shutdown(&app.bridge).await;
    }
}

#[tokio::test]
async fn help_blocks_drafts_and_review_but_keeps_global_safety_keys() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    let (tx, _rx) = mpsc::channel(8);
    app.command("/help", tx.clone()).await.unwrap();
    paint(&mut app);
    app.expire_notice_at(Instant::now() + Duration::from_secs(10));
    app.handle_paste("/quit\r\n不发送\u{3}");
    key(&mut app, KeyCode::Char('x'), &tx).await;
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert!(app.help.is_some());
    assert!(app.bridge.take_ready().is_none());
    app.handle_key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        tx.clone(),
    )
    .await
    .unwrap();
    assert!(!app.bridge.sending_enabled());
    assert!(app.help.is_some());
    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        tx.clone(),
    )
    .await
    .unwrap();
    assert!(app.help.is_none());
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    assert!(
        app.bridge.take_ready().is_none(),
        "帮助退出键不得穿透批准候选"
    );
    app.command("/help", tx.clone()).await.unwrap();
    key(&mut app, KeyCode::Esc, &tx).await;
    assert!(app.help.is_none());
    app.command("/help", tx.clone()).await.unwrap();
    assert!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), tx)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn assistant_menu_preserves_manual_input_without_starting_or_sending() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(root.path());
    app.bridge.permission(false).unwrap();
    let (tx, _rx) = mpsc::channel(8);
    for code in 6..=9 {
        key(&mut app, KeyCode::F(code), &tx).await;
        assert!(app.assistant_panel.is_none());
        assert!(app.candidate_edit.is_none());
        assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    }
    open_menu(&mut app, &tx).await;
    assert!(app.assistant_panel.is_some());
    assert!(!app.runner.running);
    assert!(!app.bridge.sending_enabled());
    key(&mut app, KeyCode::Esc, &tx).await;
    assert!(app.assistant_panel.is_none());
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(app.input.cursor(), 3);
}

fn candidate(app: &TerminalApp, caller: &str, request: &str, message: &str) -> bridge::Request {
    let req = bridge::Request::Reply {
        session: app.bridge.status()["session"].as_str().unwrap().into(),
        caller: caller.into(),
        request_id: request.into(),
        message_id: message.into(),
        text: "原候选".into(),
        candidate: true,
    };
    app.bridge.apply(req.clone()).unwrap();
    req
}
fn setup(root: &Path) -> (TerminalApp, bridge::Request, String) {
    let mut app = replay::app(root, DanmuSession::new("local"));
    let event = DanmuEvent::new(DanmuEventKind::Danmu, "问题");
    let message = event.id.clone();
    app.ingest_event(event);
    app.bridge.permission(true).unwrap();
    let request = candidate(&app, "a", "one", &message);
    app.input = "人工草稿甲🙂乙".into();
    app.input.set_cursor(3);
    (app, request, message)
}
fn paint(app: &mut TerminalApp) {
    let backend = ratatui::backend::TestBackend::new(120, 36);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| super::super::draw(frame, app))
        .unwrap();
}
async fn key(app: &mut TerminalApp, code: KeyCode, tx: &mpsc::Sender<UiEvent>) {
    paint(app);
    assert!(
        !app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap()
    );
}
async fn open_menu(app: &mut TerminalApp, tx: &mpsc::Sender<UiEvent>) {
    assert!(
        !app.handle_key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap()
    );
}

async fn candidate_menu(app: &mut TerminalApp, approve: bool, tx: &mpsc::Sender<UiEvent>) {
    app.command("/review", tx.clone()).await.unwrap();
    if !approve {
        key(app, KeyCode::Down, tx).await;
    }
    key(app, KeyCode::Enter, tx).await;
    // Close the owned review page; approval itself grants only the displayed reply.
    while approve && app.assistant_panel.is_some() {
        key(app, KeyCode::Esc, tx).await;
    }
}

fn result(app: &TerminalApp, request: &bridge::Request) -> Value {
    let bridge::Request::Reply {
        session,
        caller,
        request_id,
        ..
    } = request
    else {
        unreachable!()
    };
    app.bridge
        .apply(bridge::Request::Result {
            session: session.clone(),
            caller: caller.clone(),
            request_id: request_id.clone(),
        })
        .unwrap()
}

#[tokio::test]
async fn edit_save_binds_identity_preserves_draft_and_sends_edited_text_only_after_approval() {
    let temp = tempfile::tempdir().unwrap();
    let (mut app, request, message) = setup(temp.path());
    let (tx, mut rx) = mpsc::channel(32);
    let log = temp.path().join("deliveries.jsonl");
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(0),
        log: log.clone(),
        tx: tx.clone(),
    }));
    candidate_menu(&mut app, false, &tx).await;
    let other = candidate(&app, "b", "one", &message);
    app.next_candidate();
    app.handle_key(
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        tx.clone(),
    )
    .await
    .unwrap();
    for ch in "修改后的回答".chars() {
        key(&mut app, KeyCode::Char(ch), &tx).await;
    }
    key(&mut app, KeyCode::F(7), &tx).await;
    key(&mut app, KeyCode::F(8), &tx).await;
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    key(&mut app, KeyCode::Enter, &tx).await;
    assert!(app.candidate_edit.is_none());
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(app.input.cursor(), 3);
    let edited = result(&app, &request);
    assert_eq!(edited["text"], "修改后的回答");
    assert_eq!(edited["message_id"], message);
    assert_eq!(edited["state"], "awaiting_approval");
    assert_eq!(
        app.bridge.apply(request.clone()).unwrap()["text"],
        "修改后的回答"
    );
    assert_eq!(result(&app, &other)["text"], "原候选");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 32)).unwrap();
    app.runner.running = true;
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    app.runner.running = false;
    let visible: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .filter(|symbol| *symbol != " ")
        .collect();
    assert!(visible.contains("修改后的回答"));
    assert!(visible.contains("人工草稿甲🙂乙"));
    app.advance_bridge(tx.clone());
    assert!(!log.exists());
    assert_ne!(app.bridge.mark(&message), Some(bridge::Mark::Confirmed));
    candidate_menu(&mut app, true, &tx).await;
    assert!(app.bridge.decide("a", "one", true).is_err());
    app.advance_bridge(tx.clone());
    tokio::time::timeout(Duration::from_secs(2), async {
        while result(&app, &request)["state"] != "confirmed" {
            let event = rx.recv().await.unwrap();
            app.handle_ui_event(event);
        }
    })
    .await
    .unwrap();
    let records: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["text"], "✦ 修改后的回答");
    assert_eq!(result(&app, &other)["state"], "awaiting_approval");
    assert_eq!(app.bridge.apply(request).unwrap()["state"], "confirmed");
    assert!(app.bridge.take_ready().is_none());
    assert_eq!(app.input, "人工草稿甲🙂乙");
}

#[tokio::test]
async fn cancel_and_validation_errors_keep_candidate_and_manual_draft_separate() {
    let temp = tempfile::tempdir().unwrap();
    let (mut app, request, _) = setup(temp.path());
    let (tx, _rx) = mpsc::channel(8);
    candidate_menu(&mut app, false, &tx).await;
    for invalid in [String::new(), "界".repeat(1366), "含\n控制字符".into()] {
        app.input.replace(invalid.clone());
        key(&mut app, KeyCode::Enter, &tx).await;
        assert!(app.candidate_edit.is_some());
        assert_eq!(app.input.to_string(), invalid);
        assert_eq!(result(&app, &request)["text"], "原候选");
        assert!(app.notice.contains("保存失败"));
    }
    key(&mut app, KeyCode::Esc, &tx).await;
    assert_eq!(app.input, "人工草稿甲🙂乙");
    assert_eq!(app.input.cursor(), 3);
    assert_eq!(result(&app, &request)["text"], "原候选");
    candidate_menu(&mut app, false, &tx).await;
    app.input.replace("/quit".into());
    key(&mut app, KeyCode::Enter, &tx).await;
    assert!(!app.quit_requested);
    assert_eq!(result(&app, &request)["text"], "/quit");
    assert_eq!(result(&app, &request)["state"], "awaiting_approval");
    assert!(app.bridge.take_ready().is_none());
    assert_eq!(app.bridge.status()["sending_enabled"], true);
}

#[tokio::test]
async fn pause_preserves_editable_candidates_and_requires_fresh_explicit_approval() {
    for route in ["ctrl-p", "pause"] {
        let temp = tempfile::tempdir().unwrap();
        let (mut app, request, message) = setup(temp.path());
        let (tx, mut rx) = mpsc::channel(32);
        let log = temp.path().join("pause-deliveries.jsonl");
        app.local_transport = Some(Arc::new(LocalTransport {
            outcome: AtomicU8::new(0),
            log: log.clone(),
            tx: tx.clone(),
        }));
        let other = candidate(&app, "b", "one", &message);
        let other_before = result(&app, &other);
        let mut old_direct = request.clone();
        if let bridge::Request::Reply {
            request_id,
            candidate,
            ..
        } = &mut old_direct
        {
            *request_id = "old-direct".into();
            *candidate = false;
        }
        app.bridge.apply(old_direct.clone()).unwrap();
        candidate_menu(&mut app, false, &tx).await;
        app.input.replace("暂停前已保存".into());
        key(&mut app, KeyCode::Enter, &tx).await;
        let before = result(&app, &request);
        if route == "ctrl-p" {
            candidate_menu(&mut app, false, &tx).await;
            app.input.replace("暂停中保存".into());
            app.handle_key(
                KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                tx.clone(),
            )
            .await
            .unwrap();
            assert_eq!(app.input, "暂停中保存");
        } else {
            app.command("/pause", tx.clone()).await.unwrap();
            candidate_menu(&mut app, false, &tx).await;
            app.input.replace("暂停中保存".into());
        }
        assert_eq!(result(&app, &request), before, "{route}");
        assert_eq!(result(&app, &other), other_before);
        assert_eq!(app.bridge.candidates().len(), 2);
        assert_eq!(result(&app, &old_direct)["state"], "cancelled");
        key(&mut app, KeyCode::Enter, &tx).await;
        assert!(app.candidate_edit.is_none());
        assert_eq!(app.input, "人工草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
        let saved = result(&app, &request);
        assert_eq!(saved["text"], "暂停中保存");
        assert_eq!(saved["state"], "awaiting_approval");
        for field in ["session", "caller", "request_id", "message_id"] {
            assert_eq!(saved[field], before[field]);
        }
        assert_eq!(app.bridge.status()["sending_enabled"], false);
        assert_ne!(app.bridge.mark(&message), Some(bridge::Mark::Confirmed));
        assert_eq!(app.bridge.apply(request.clone()).unwrap(), saved);
        assert_eq!(result(&app, &request), saved);
        candidate_menu(&mut app, false, &tx).await;
        app.input.replace("取消的编辑".into());
        key(&mut app, KeyCode::Esc, &tx).await;
        assert_eq!(result(&app, &request), saved);
        assert_eq!(app.input, "人工草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
        let mut paused_direct = old_direct.clone();
        if let bridge::Request::Reply { request_id, .. } = &mut paused_direct {
            *request_id = "paused-direct".into();
        }
        assert_eq!(
            app.bridge.apply(paused_direct.clone()).unwrap()["state"],
            "rejected"
        );
        app.advance_bridge(tx.clone());
        assert!(!log.exists());
        // Use the real manual Enter/queue/LocalTransport while Agent sending is paused.
        key(&mut app, KeyCode::Esc, &tx).await;
        key(&mut app, KeyCode::Enter, &tx).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = rx.recv().await.unwrap();
                let done = matches!(event, UiEvent::DeliveryCompleted);
                app.handle_ui_event(event);
                if done {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let read_log = || -> Vec<Value> {
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        };
        assert_eq!(
            read_log()
                .iter()
                .map(|r| r["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["人工草稿甲🙂乙"]
        );
        app.bridge.permission(true).unwrap();
        assert!(app.bridge.take_ready().is_none());
        assert_eq!(app.bridge.apply(old_direct).unwrap()["state"], "cancelled");
        assert_eq!(
            app.bridge.apply(paused_direct).unwrap()["state"],
            "rejected"
        );
        assert_eq!(app.bridge.apply(request.clone()).unwrap(), saved);
        app.advance_bridge(tx.clone());
        assert_eq!(read_log().len(), 1);
        candidate_menu(&mut app, true, &tx).await;
        assert!(app.bridge.decide("a", "one", true).is_err());
        app.advance_bridge(tx.clone());
        tokio::time::timeout(Duration::from_secs(2), async {
            while result(&app, &request)["state"] != "confirmed" {
                app.handle_ui_event(rx.recv().await.unwrap());
            }
        })
        .await
        .unwrap();
        assert_eq!(
            read_log()
                .iter()
                .map(|r| r["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["人工草稿甲🙂乙", "✦ 暂停中保存"]
        );
        assert_eq!(result(&app, &other), other_before);
        assert_eq!(app.bridge.apply(request).unwrap()["state"], "confirmed");
        assert!(app.bridge.take_ready().is_none());
    }
}

#[tokio::test]
async fn stale_editor_cannot_revive_terminal_candidate_or_cross_into_new_scene() {
    for transition in [
        "approve",
        "discard",
        "end",
        "new",
        "sending",
        "confirmed",
        "uncertain",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let (mut app, request, _) = setup(temp.path());
        let (tx, _rx) = mpsc::channel(8);
        candidate_menu(&mut app, false, &tx).await;
        app.input.replace("迟到编辑".into());
        match transition {
            "approve" => app.bridge.decide("a", "one", true).unwrap(),
            "discard" => app.bridge.decide("a", "one", false).unwrap(),

            "end" => app.bridge.end(),
            "new" => {
                app.bridge.new_session();
                let event = DanmuEvent::new(DanmuEventKind::Danmu, "下一场");
                let id = event.id.clone();
                app.ingest_event(event);
                app.bridge.permission(true).unwrap();
                candidate(&app, "a", "one", &id);
            }
            _ => {
                app.bridge.decide("a", "one", true).unwrap();
                let job = app.bridge.take_ready().unwrap();
                match transition {
                    "confirmed" => app.bridge.complete(&job.reply, Outcome::Confirmed.into()),
                    "uncertain" => app.bridge.complete(&job.reply, Outcome::Uncertain.into()),
                    _ => {}
                }
            }
        }
        let before = app.bridge.status();
        let original = (transition != "new").then(|| result(&app, &request));
        key(&mut app, KeyCode::Enter, &tx).await;
        assert!(app.candidate_edit.is_some(), "{transition}");
        assert_eq!(app.input, "迟到编辑");
        assert_eq!(app.bridge.status(), before);
        if let Some(original) = original {
            assert_eq!(result(&app, &request), original);
        }
        assert!(app.bridge.candidates().iter().all(|r| r.text == "原候选"));
        key(&mut app, KeyCode::Esc, &tx).await;
        assert_eq!(app.input, "人工草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
    }
}

#[tokio::test]
async fn native_reply_review_edits_and_shared_queue_keep_original_target_and_ai_marker() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = replay::app(temp.path(), DanmuSession::new("local"));
    let (tx, mut rx) = mpsc::channel(32);
    let log = temp.path().join("deliveries.jsonl");
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(0),
        log: log.clone(),
        tx: tx.clone(),
    }));
    app.bridge.claim_driver("owner").unwrap();
    let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "原问题");
    event.author_id = Some("42".into());
    event.username = Some("原观众".into());
    let message = event.id.clone();
    app.ingest_event(event);
    let request = bridge::Request::Reply {
        session: app.bridge.session_id(),
        caller: bridge::RUNNER_CALLER.into(),
        request_id: "native".into(),
        message_id: message,
        text: "✦ ✦ 初始回答".into(),
        candidate: true,
    };
    app.bridge
        .apply_owned_reply("owner", request.clone(), true)
        .unwrap();
    app.input = "人工草稿".into();
    app.advance_bridge(tx.clone());
    assert!(!log.exists());
    assert!(
        app.bridge
            .decide(bridge::RUNNER_CALLER, "native", true)
            .is_err()
    );
    let preview = app.selected_candidate_text();
    assert!(preview.contains("原观众") && preview.contains("UID 42"));
    assert!(preview.contains("✦ 初始回答") && !preview.contains("✦ ✦"));
    app.begin_candidate_edit().unwrap();
    app.bridge.permission(true).unwrap();
    let mut other = DanmuEvent::new(DanmuEventKind::Danmu, "另一条问题");
    other.author_id = Some("99".into());
    other.username = Some("其他观众".into());
    let other_id = other.id.clone();
    app.ingest_event(other);
    candidate(&app, "external", "other", &other_id);
    app.next_candidate();
    app.runner.settings.mention_sender = false;
    app.input = "修改回答".repeat(12).into();
    app.save_candidate_edit();
    assert!(app.candidate_edit.is_none());
    assert_eq!(app.input, "人工草稿");
    let body = "修改回答".repeat(12);
    let expected = assistant_segments(&body);
    assert!(expected.len() > 1);
    for text in &expected {
        assert!(text.starts_with("✦ "));
        assert!(text.graphemes(true).count() <= crate::bilibili::SEND_SEGMENT_LIMIT);
    }
    assert_eq!(
        expected
            .iter()
            .map(|text| text.strip_prefix("✦ ").unwrap())
            .collect::<String>(),
        body
    );
    assert_eq!(result(&app, &request)["reply_to"]["user_id"], "42");
    app.bridge
        .decide(bridge::RUNNER_CALLER, "native", true)
        .unwrap();
    app.advance_bridge(tx.clone());
    app.enqueue(vec!["人工发送".into()], None, tx.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut completed = 0;
        while completed < 2 {
            let event = rx.recv().await.unwrap();
            if matches!(event, UiEvent::DeliveryCompleted) {
                completed += 1;
            }
            app.handle_ui_event(event);
        }
    })
    .await
    .unwrap();
    let rows: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), expected.len() + 1);
    let automated: Vec<_> = rows.iter().filter(|r| r["reply_to"] == "42").collect();
    assert_eq!(automated.len(), expected.len());
    for (row, text) in automated.iter().zip(expected) {
        assert_eq!(row["text"], text);
    }
    let manual = rows.iter().find(|r| r["text"] == "人工发送").unwrap();
    assert!(manual["reply_to"].is_null());
    assert_eq!(result(&app, &request)["state"], "confirmed");
    assert_eq!(app.input, "人工草稿");
}

#[test]
fn assistant_marker_reserves_grapheme_capacity_and_deduplicates_model_prefixes() {
    let body = "e\u{301}".repeat(39);
    let segments = assistant_segments(&format!("✦ ✦ {body}"));
    assert_eq!(
        segments,
        vec![format!("✦ {}", "e\u{301}".repeat(38)), "✦ e\u{301}".into()]
    );
    assert!(
        segments
            .iter()
            .all(|s| s.graphemes(true).count() <= crate::bilibili::SEND_SEGMENT_LIMIT)
    );
    assert_eq!(assistant_segments("✦"), vec!["✦"]);
}

#[cfg(unix)]
fn repair_app(
    root: &Path,
    mode: u8,
    automatic: bool,
    known_word: bool,
) -> (
    TerminalApp,
    mpsc::Sender<UiEvent>,
    mpsc::Receiver<UiEvent>,
    PathBuf,
) {
    use std::os::unix::fs::PermissionsExt;
    let binary = root.join("repair_fixture.py");
    let fixture = include_str!("../../tests/fixtures/acp_agent.py").replace(
            "\"text\":\"受控候选\"",
            "\"text\": (\"合规改写正文\" if \"repair\" in payload else config.get(\"initial_text\", \"原始候选\"))"
        );
    let fixture = fixture.replace("payload = json.loads(req[\"params\"][\"prompt\"][0][\"text\"])", "payload = json.loads(req[\"params\"][\"prompt\"][0][\"text\"])\n    if \"repair\" in payload: time.sleep(.15)");
    std::fs::write(&binary, fixture).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let initial = if mode == 6 {
        format!("{}{}", "甲".repeat(38), "乙".repeat(20))
    } else {
        "原始候选".into()
    };
    std::fs::write(
        root.join("fixture.json"),
        serde_json::json!({"mode":"normal", "initial_text":initial}).to_string(),
    )
    .unwrap();
    let mut app = replay::app(root, DanmuSession::new("local"));
    app.bridge.available(true);
    app.runner.settings.binary = binary;
    app.runner.settings.automatic = automatic;
    app.runner.settings.mention_sender = true;
    if known_word {
        app.runner.settings.blocked_words = vec!["原始".into()];
    }
    let (tx, rx) = mpsc::channel(64);
    let log = root.join("deliveries.jsonl");
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(mode),
        log: log.clone(),
        tx: tx.clone(),
    }));
    app.runner.start(&app.bridge, false, false).unwrap();
    if automatic {
        app.bridge.permission(true).unwrap();
    }

    (app, tx, rx, log)
}

#[cfg(unix)]
#[tokio::test]
async fn owned_repair_rounds_deliver_once_with_frozen_review_target_and_segment_boundaries() {
    for (mode, automatic, known_word) in [
        (5, true, false),
        (5, false, false),
        (1, true, false),
        (6, true, false),
        (3, true, false),
        (0, true, true),
        (4, true, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut app, tx, mut rx, log) = repair_app(root.path(), mode, automatic, known_word);
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "需要实时外部资料的合理问题");
        event.author_id = Some("42".into());
        event.username = Some("原作者".into());
        app.ingest_event(event);
        let mut approved = false;
        let mut review_seen = false;
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                app.advance_bridge(tx.clone());
                while let Ok(event) = rx.try_recv() {
                    app.handle_ui_event(event);
                }
                let candidates = app.bridge.candidates();
                if !automatic && let Some(candidate) = candidates.first() {
                    if candidate.retry_of.is_some() {
                        let sent = std::fs::read_to_string(&log).unwrap().lines().count();
                        assert_eq!(sent, 1, "改写未经二次批准不得发送");
                        assert!(candidate.approval_required);
                        assert_eq!(candidate.reply_to.as_ref().unwrap().user_id, "42");
                        review_seen = true;
                    }
                    app.bridge.permission(true).unwrap();
                    app.bridge
                        .decide(&candidate.caller, &candidate.request_id, true)
                        .unwrap();
                    approved = true;
                    app.runner.settings.mention_sender = false;
                }
                let results = app.bridge.recent_results();
                if matches!(mode, 1 | 3)
                    && results
                        .iter()
                        .any(|r| r.state == bridge::Execution::Uncertain)
                {
                    break;
                }
                if results.iter().any(|r| {
                    r.retry_of.is_some()
                        && matches!(
                            r.state,
                            bridge::Execution::Confirmed
                                | bridge::Execution::Rejected
                                | bridge::Execution::Uncertain
                        )
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let results = app.bridge.recent_results();
        let original = results.iter().find(|r| r.retry_of.is_none()).unwrap();
        let query = bridge::Request::Result {
            session: original.session.clone(),
            caller: original.caller.clone(),
            request_id: original.request_id.clone(),
        };
        let original_record = app.bridge.apply(query.clone()).unwrap();
        let prompts: Vec<Value> = std::fs::read_to_string(root.path().join("wire.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .filter(|r| r["direction"] == "in" && r["value"]["method"] == "session/prompt")
            .collect();
        assert_eq!(prompts.len(), if matches!(mode, 1 | 3) { 1 } else { 2 });
        if !matches!(mode, 1 | 3) {
            let repaired = results.iter().find(|r| r.retry_of.is_some()).unwrap();
            assert_eq!(
                repaired.retry_of.as_deref(),
                Some(original.request_id.as_str())
            );
            assert_eq!(repaired.reply_to.as_ref().unwrap().user_id, "42");
            assert_eq!(
                repaired.state,
                match mode {
                    4 => bridge::Execution::Rejected,
                    _ => bridge::Execution::Confirmed,
                }
            );
            if !automatic {
                assert!(approved && review_seen);
            }
            let input: Value = serde_json::from_str(
                prompts[1]["value"]["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(input["repair"]["original_message"]["authorId"], "42");
            if mode == 6 {
                assert_eq!(
                    input["repair"]["confirmed_segments_do_not_repeat"],
                    serde_json::json!([format!("✦ {}", "甲".repeat(38))])
                );
            }
        }
        let rows: Vec<Value> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(
            rows.len(),
            if matches!(mode, 1 | 3) || known_word {
                1
            } else if mode == 6 {
                3
            } else {
                2
            }
        );
        assert!(rows.iter().all(|r| r["reply_to"] == "42"));
        if mode == 6 {
            assert_eq!(
                rows.iter()
                    .filter(|r| r["text"].as_str().unwrap().contains('甲'))
                    .count(),
                1
            );
        }
        if known_word {
            assert!(
                !rows
                    .iter()
                    .any(|r| r["text"].as_str().unwrap().contains("原始"))
            );
        }
        for _ in 0..8 {
            app.advance_bridge(tx.clone());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            app.bridge.apply(query).unwrap(),
            original_record,
            "原失败终态不能被改写或成功覆盖"
        );
        app.runner.shutdown(&app.bridge).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn owned_repair_inflight_output_cannot_survive_pause_scene_change_or_late_echo() {
    for transition in ["pause", "scene", "late_echo", "revoke"] {
        let root = tempfile::tempdir().unwrap();
        let (mut app, tx, mut rx, log) = repair_app(root.path(), 5, true, false);
        app.ingest_event(DanmuEvent::new(DanmuEventKind::Danmu, "合理问题"));
        let mut original = None;
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                app.advance_bridge(tx.clone());
                while let Ok(event) = rx.try_recv() {
                    app.handle_ui_event(event);
                }
                if original.is_none() {
                    original = app
                        .bridge
                        .recent_results()
                        .into_iter()
                        .find(|r| r.retry_of.is_none());
                }
                let prompts = std::fs::read_to_string(root.path().join("wire.jsonl"))
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|s| serde_json::from_str::<Value>(s).ok())
                    .filter(|r| r["direction"] == "in" && r["value"]["method"] == "session/prompt")
                    .count();
                if prompts == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        match transition {
            "pause" => app.runner.pause(&app.bridge),
            "scene" => app.bridge.new_session(),
            "late_echo" => app.bridge.late_echo(original.as_ref().unwrap()),
            "revoke" => {
                app.bridge.permission(false).unwrap();
                app.bridge.permission(true).unwrap();
            }
            _ => unreachable!(),
        }
        for _ in 0..120 {
            app.advance_bridge(tx.clone());
            while let Ok(event) = rx.try_recv() {
                app.handle_ui_event(event);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            std::fs::read_to_string(log).unwrap().lines().count(),
            1,
            "{transition}: 旧修复不得发送"
        );
        assert!(app.bridge.candidates().is_empty());
        app.runner.shutdown(&app.bridge).await;
    }
}
fn automatic_account_fixture(root: &Path, uid: &str) -> AccountStatus {
    let status = AccountStatus::SignedIn {
        display_name: format!("用户{uid}"),
        user_id: uid.into(),
    };
    let credential = serde_json::json!({
        "cookieHeader": format!("SESSDATA=secret-{uid}; bili_jct=csrf-{uid}; DedeUserID={uid}"),
        "csrf": format!("csrf-{uid}"),
        "identity": status,
    });
    crate::storage::write_private_atomic(
        &root.join("account.json"),
        &serde_json::to_vec(&credential).unwrap(),
    )
    .unwrap();
    status
}

fn automatic_app(root: &Path, room_id: &str, uid: &str, local: bool) -> TerminalApp {
    let status = automatic_account_fixture(root, uid);
    let mut app = replay::app(root, DanmuSession::new(room_id));
    app.config.room_id = room_id.into();
    app.bridge = Bridge::new(local);
    app.bridge.available(true);
    app.account_status = status;
    app.room = Some(RoomSnapshot {
        room_id: room_id.into(),
        broadcaster_id: "42".into(),
        broadcaster_name: "测试主播".into(),
        title: "授权恢复测试场次".into(),
        area: "知识".into(),
        live_started_at: Some(Utc::now()),
        live_status: RoomLiveStatus::Live,
    });
    app
}

fn restore_current_automatic(app: &mut TerminalApp) -> anyhow::Result<()> {
    let session = app.bridge.session_id();
    let generation = app.assistant_accounts.generation();
    app.restore_automatic(&session, generation)
}

#[test]
fn automatic_permission_reloads_only_for_the_same_room_and_sending_uid() {
    let same = tempfile::tempdir().unwrap();
    let mut authorized = automatic_app(same.path(), "100", "11", false);
    authorized.set_automatic_permission(true).unwrap();
    assert!(authorized.bridge.sending_enabled());
    drop(authorized);

    let mut reloaded = automatic_app(same.path(), "100", "11", false);
    assert!(!reloaded.bridge.sending_enabled());
    restore_current_automatic(&mut reloaded).unwrap();
    assert!(reloaded.bridge.sending_enabled());

    let different_room = tempfile::tempdir().unwrap();
    let mut authorized = automatic_app(different_room.path(), "100", "11", false);
    authorized.set_automatic_permission(true).unwrap();
    drop(authorized);
    let mut reloaded = automatic_app(different_room.path(), "200", "11", false);
    let _ = restore_current_automatic(&mut reloaded);
    assert!(!reloaded.bridge.sending_enabled());

    let different_account = tempfile::tempdir().unwrap();
    let mut authorized = automatic_app(different_account.path(), "100", "11", false);
    authorized.set_automatic_permission(true).unwrap();
    drop(authorized);
    let mut reloaded = automatic_app(different_account.path(), "100", "22", false);
    let _ = restore_current_automatic(&mut reloaded);
    assert!(!reloaded.bridge.sending_enabled());
}

#[test]
fn missing_credentials_cannot_restore_a_saved_automatic_grant() {
    let root = tempfile::tempdir().unwrap();
    let mut authorized = automatic_app(root.path(), "100", "11", false);
    authorized.set_automatic_permission(true).unwrap();
    drop(authorized);
    std::fs::remove_file(root.path().join("account.json")).unwrap();
    let mut reloaded = replay::app(root.path(), DanmuSession::new("100"));
    reloaded.bridge = Bridge::new(false);
    reloaded.bridge.available(true);
    let _ = restore_current_automatic(&mut reloaded);
    assert!(!reloaded.bridge.sending_enabled());
}

#[test]
fn automatic_restore_rejects_late_identity_generation_and_scene() {
    let generation_root = tempfile::tempdir().unwrap();
    let mut authorized = automatic_app(generation_root.path(), "100", "11", false);
    authorized.set_automatic_permission(true).unwrap();
    drop(authorized);

    let mut reloaded = automatic_app(generation_root.path(), "100", "11", false);
    let session = reloaded.bridge.session_id();
    let stale_generation = reloaded.assistant_accounts.generation();
    reloaded.assistant_accounts.main_changed().unwrap();
    reloaded
        .assistant_accounts
        .remember_automatic("100", &reloaded.account_status)
        .unwrap();
    let _ = reloaded.restore_automatic(&session, stale_generation);
    assert!(!reloaded.bridge.sending_enabled());

    let scene_root = tempfile::tempdir().unwrap();
    let mut authorized = automatic_app(scene_root.path(), "100", "11", false);
    authorized.set_automatic_permission(true).unwrap();
    drop(authorized);
    let mut reloaded = automatic_app(scene_root.path(), "100", "11", false);
    let stale_session = reloaded.bridge.session_id();
    let generation = reloaded.assistant_accounts.generation();
    reloaded.bridge.new_session();
    reloaded.bridge.available(true);
    let _ = reloaded.restore_automatic(&stale_session, generation);
    assert!(!reloaded.bridge.sending_enabled());
}

#[test]
fn explicit_disable_survives_reload_and_removes_the_restore_grant() {
    let root = tempfile::tempdir().unwrap();
    let mut authorized = automatic_app(root.path(), "100", "11", false);
    authorized.set_automatic_permission(true).unwrap();
    authorized.set_automatic_permission(false).unwrap();
    assert!(!authorized.bridge.sending_enabled());
    drop(authorized);

    let mut reloaded = automatic_app(root.path(), "100", "11", false);
    let _ = restore_current_automatic(&mut reloaded);
    assert!(!reloaded.bridge.sending_enabled());
}

#[test]
fn local_permission_never_becomes_a_persistent_production_grant() {
    let root = tempfile::tempdir().unwrap();
    let mut local = automatic_app(root.path(), "100", "11", true);
    local.set_automatic_permission(true).unwrap();
    assert!(local.bridge.sending_enabled());
    drop(local);

    let mut production = automatic_app(root.path(), "100", "11", false);
    let _ = restore_current_automatic(&mut production);
    assert!(!production.bridge.sending_enabled());
}

#[test]
fn automatic_permission_rejects_unverified_identity_before_changing_settings() {
    let missing = tempfile::tempdir().unwrap();
    let mut signed_out = replay::app(missing.path(), DanmuSession::new("100"));
    signed_out.config.room_id = "100".into();
    signed_out.bridge = Bridge::new(false);
    signed_out.bridge.available(true);
    assert!(signed_out.set_automatic_permission(true).is_err());
    assert!(!signed_out.bridge.sending_enabled());
    assert!(!signed_out.runner.settings.automatic);

    let mismatched = tempfile::tempdir().unwrap();
    let mut app = automatic_app(mismatched.path(), "100", "11", false);
    app.account_status = AccountStatus::SignedIn {
        display_name: "错误缓存身份".into(),
        user_id: "22".into(),
    };
    assert!(app.set_automatic_permission(true).is_err());
    assert!(!app.bridge.sending_enabled());
    assert!(!app.runner.settings.automatic);
}

#[tokio::test]
async fn invalid_identity_command_keeps_the_room_and_manual_sending_alive() {
    for search in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut app = automatic_app(root.path(), "100", "11", false);
        app.set_automatic_permission(true).unwrap();
        app.resume_pending = true;
        app.assistant_accounts
            .send_identity(&app.account_status)
            .invalidate()
            .unwrap();
        let session = app.bridge.session_id();
        let (tx, _rx) = mpsc::channel(32);
        let log = root.path().join("manual.jsonl");
        app.local_transport = Some(Arc::new(LocalTransport {
            outcome: AtomicU8::new(0),
            log: log.clone(),
            tx: tx.clone(),
        }));
        if search {
            app.input = "保留🙂草稿".into();
            app.input.set_cursor(2);
            app.open_commands();
            app.handle_paste("/ai auto on");
        } else {
            app.input = "/ai auto on".into();
        }
        let result = app
            .handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                tx.clone(),
            )
            .await;
        assert!(
            matches!(result, Ok(false)),
            "身份失效不得逃逸到主循环：{result:?}"
        );
        assert!(app.notice.contains("助手身份已更改或登录态不可用"));
        assert!(!app.bridge.sending_enabled());
        assert!(!app.resume_pending);
        assert!(!app.assistant_accounts.has_automatic_grant());
        assert_eq!(app.bridge.session_id(), session);
        assert!(app.bridge.is_active());
        if search {
            assert_eq!(app.input, "保留🙂草稿");
            assert_eq!(app.input.cursor(), 2);
        }
        for _ in 0..3 {
            if app.assistant_panel.is_none() {
                break;
            }
            key(&mut app, KeyCode::Esc, &tx).await;
        }
        app.input = "主账号仍可发送".into();
        key(&mut app, KeyCode::Enter, &tx).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !log.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let sent: Value =
            serde_json::from_str(std::fs::read_to_string(&log).unwrap().trim()).unwrap();
        assert_eq!(sent["text"], "主账号仍可发送");
        assert!(!app.bridge.sending_enabled());
        app.runner.shutdown(&app.bridge).await;
    }
}

#[tokio::test]
async fn command_recovery_does_not_hide_current_journal_write_failures() {
    let root = tempfile::tempdir().unwrap();
    let mut app = replay::app(root.path(), DanmuSession::new("local"));
    let blocked = root.path().join("not-a-directory");
    std::fs::write(&blocked, "occupied").unwrap();
    app.journal = SessionJournal::new(blocked.clone());
    app.input = "/help".into();
    let (tx, _rx) = mpsc::channel(8);
    let error = app
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
        .await
        .unwrap_err();
    assert!(
        error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some())
    );
    assert_eq!(std::fs::read_to_string(blocked).unwrap(), "occupied");
}

#[test]
fn pause_revokes_and_forgets_automatic_permission_without_erasing_preference() {
    let root = tempfile::tempdir().unwrap();
    let mut app = automatic_app(root.path(), "100", "11", false);
    app.set_automatic_permission(true).unwrap();
    app.pause_assistant();
    assert!(!app.bridge.sending_enabled());
    assert!(
        app.runner.settings.automatic,
        "pause keeps the user's mode preference"
    );
    assert!(
        !app.runner.settings.resume_on_start,
        "pause clears resume intent"
    );
    drop(app);

    let mut reloaded = automatic_app(root.path(), "100", "11", false);
    assert!(restore_current_automatic(&mut reloaded).is_err());
    assert!(!reloaded.bridge.sending_enabled());
}

#[cfg(unix)]
#[tokio::test]
async fn persisted_ungranted_resume_keeps_input_live_and_starts_only_after_command() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture");
    std::fs::create_dir(&fixture).unwrap();
    let program = fixture.join("acp-agent");
    std::fs::write(
        &program,
        include_bytes!("../../tests/fixtures/acp_agent.py"),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(fixture.join("fixture.json"), r#"{"mode":"normal"}"#).unwrap();
    let settings = crate::runner::settings::Settings {
        host: crate::runner::settings::Host::Omp,
        binary: program,
        automatic: true,
        resume_on_start: true,
        ..Default::default()
    };
    settings.save(&root.path().join("assistant.json")).unwrap();
    let mut app = replay::app(root.path(), DanmuSession::new("local"));
    let (tx, _rx) = mpsc::channel(128);
    app.local_transport = Some(Arc::new(LocalTransport {
        outcome: AtomicU8::new(0),
        log: root.path().join("deliveries.jsonl"),
        tx: tx.clone(),
    }));
    for (width, height) in [(16, 6), (32, 16), (80, 24), (120, 36)] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        app.advance_bridge(tx.clone());
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.handle_key(
            KeyEvent::new(KeyCode::Char('甲'), KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
    }
    assert_eq!(app.input, "甲甲甲甲");
    assert!(!app.runner.running);
    assert!(!app.bridge.sending_enabled());
    assert!(!fixture.join("wire.jsonl").exists());
    app.command("/ai start", tx.clone()).await.unwrap();
    for _ in 0..100 {
        app.advance_bridge(tx.clone());
        if app.runner.report.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(app.runner.running && app.runner.report.connected);
    assert!(!app.bridge.sending_enabled());
    assert_eq!(app.input, "甲甲甲甲");
    let wire = std::fs::read_to_string(fixture.join("wire.jsonl")).unwrap();
    assert_eq!(
        wire.lines()
            .filter(|line| {
                let row: Value = serde_json::from_str(line).unwrap();
                row["direction"] == "in" && row["value"]["method"] == "session/new"
            })
            .count(),
        1
    );
    app.runner.shutdown(&app.bridge).await;
}
