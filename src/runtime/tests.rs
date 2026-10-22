use super::*;

fn options(root: &Path, mode: RuntimeMode) -> RuntimeOptions {
    let storage = StoragePaths {
        support_dir: root.to_path_buf(),
        config_file: root.join("config.toml"),
        account_session: root.join("account.json"),
        obs_configuration: root.join("obs.json"),
        sessions_dir: root.join("sessions"),
        themes_file: root.join("themes.json"),
    };
    RuntimeOptions {
        obs_passwords: Arc::new(crate::obs::FileObsPasswordStore::new(
            storage.obs_configuration.clone(),
            false,
        )),
        storage,
        workspace_default: root.join("workspace"),
        mode,
        external_instance: None,
    }
}
fn test_state(temp: &tempfile::TempDir, session: DanmuSession) -> RuntimeState {
    let mut state = RuntimeState::prepare(options(temp.path(), RuntimeMode::Local)).unwrap();
    state.session = session;
    state.journal.start(&state.session).unwrap();
    state
        .runner
        .import_history(&state.session.room_id, &state.journal)
        .unwrap();
    state.connection = "已连接 1".into();
    state
}
#[tokio::test]
pub(crate) async fn post_waits_for_echo_matcher_and_fast_echo_confirms_once() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let (post_seen_tx, mut post_seen_rx) = oneshot::channel();
    let (respond_tx, respond_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        let mut post_seen_tx = Some(post_seen_tx);
        let mut respond_rx = Some(respond_rx);
        for body in [
            r#"{"code":0,"data":{"isLogin":true,"uname":"用户42","mid":42}}"#,
            r#"{"code":0,"data":{"mode_info":{"extra":{"content":"极速回声","send_from_me":true}}}}"#,
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            if request.starts_with("POST ") {
                post_seen_tx.take().unwrap().send(()).unwrap();
                respond_rx.take().unwrap().await.unwrap();
            }
            requests.push(request);
            socket.write_all(response(body).as_bytes()).await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        requests
    });

    let temp = crate::test_support::private_tempdir().unwrap();
    let account_path = temp.path().join("account.json");
    crate::storage::write_private_atomic(
        &account_path,
        &serde_json::to_vec(&serde_json::json!({
            "cookieHeader":"SESSDATA=fake; bili_jct=fake; DedeUserID=42",
            "csrf":"fake",
            "identity":{"SignedIn":{"display_name":"用户42", "user_id":"42"}}
        }))
        .unwrap(),
    )
    .unwrap();
    let account = AccountClient::new(account_path)
        .unwrap()
        .with_test_endpoint(endpoint);
    let (tx, mut ui_rx) = mpsc::channel(8);
    let transport = RuntimeTransport {
        account: accounts::SendIdentity::manual(account),
        client: BilibiliClient::new(temp.path().join("live-session.json")).unwrap(),
        room: "1".into(),
        tx,
        job: None,
        bridge: bridge::Bridge::new(true),
        queue: SendQueue::default(),
        manual_permit: None,
    };
    let send = tokio::spawn(async move { transport.send_confirm("极速回声", None).await });
    let started = tokio::time::timeout(Duration::from_secs(1), ui_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut post_seen_rx)
            .await
            .is_err(),
        "核心尚未注册回声匹配器时不得开始POST"
    );

    let mut app = test_state(&temp, DanmuSession::new("1"));
    app.handle_ui_event(started);
    post_seen_rx.await.unwrap();
    let mut echo = DanmuEvent::new(DanmuEventKind::Danmu, "极速回声");
    echo.timestamp = Utc::now();
    echo.username = Some("用户42".into());
    echo.author_id = Some("42".into());
    app.ingest_event(echo);
    respond_tx.send(()).unwrap();

    let delivery = send.await.unwrap();
    assert_eq!(delivery.outcome, Outcome::Confirmed);
    assert_eq!(
        delivery.diagnosis.cause,
        crate::delivery::Cause::EchoConfirmed
    );
    let requests = server.await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST "))
            .count(),
        1
    );
}

#[test]
pub(crate) fn late_echo_keeps_its_identity_and_expires_at_the_delivery_window() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut app = test_state(&temp, DanmuSession::new("1"));
    let submitted = Utc::now();
    let delivery = PendingDelivery::new("late reply".into(), "host".into(), "42".into(), submitted);
    let mut boundary = DanmuEvent::new(DanmuEventKind::Danmu, "late reply");
    boundary.author_id = Some("42".into());
    boundary.timestamp = submitted + chrono::Duration::seconds(90);
    assert!(delivery.matches(&boundary));
    boundary.timestamp += chrono::Duration::milliseconds(1);
    assert!(!delivery.matches(&boundary));
    let id = delivery.id.clone();
    let (tx, mut rx) = oneshot::channel();
    let (registered_tx, _registered_rx) = oneshot::channel();
    app.handle_ui_event(CoreEvent::DeliveryStarted {
        delivery,
        confirmation: tx,
        registered: registered_tx,
    });
    app.handle_ui_event(CoreEvent::DeliveryEchoMissing {
        delivery_ids: vec![id.clone()],
    });
    assert_eq!(app.delivery_status, DeliveryStatus::Uncertain);
    let mut impostor = DanmuEvent::new(DanmuEventKind::Danmu, "late reply");
    impostor.author_id = Some("43".into());
    impostor.username = Some("host".into());
    impostor.timestamp = submitted + chrono::Duration::seconds(30);
    app.ingest_event(impostor);
    assert!(rx.try_recv().is_err());
    let mut echo = DanmuEvent::new(DanmuEventKind::Danmu, "late reply");
    echo.author_id = Some("42".into());
    echo.timestamp = submitted + chrono::Duration::seconds(30);
    app.ingest_event(echo);
    assert!(rx.try_recv().is_ok());
    app.handle_ui_event(CoreEvent::DeliveryCompleted);
    assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
    app.handle_ui_event(CoreEvent::DeliveryEchoMissing {
        delivery_ids: vec![id],
    });
    assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
}
#[test]
pub(crate) fn sent_message_accepts_masked_live_echo_and_restores_broadcaster_identity() {
    let submitted_at = Utc::now();
    let pending = PendingDelivery::new(
        "我发的消息".into(),
        "拾穗数据".into(),
        "42".into(),
        submitted_at,
    );
    let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "我发的消息");
    event.timestamp = submitted_at + chrono::Duration::seconds(1);
    event.username = Some("拾***据".into());
    event.author_id = Some("0".into());

    assert!(pending.matches(&event));
    pending.canonicalize(&mut event);
    assert_eq!(event.username.as_deref(), Some("拾穗数据"));
    assert_eq!(event.author_id.as_deref(), Some("42"));
}

#[test]
pub(crate) fn canonical_history_reconciles_masked_live_event_without_a_duplicate() {
    let timestamp = Utc::now();
    let mut live = DanmuEvent::new(DanmuEventKind::Danmu, "同一条弹幕");
    live.timestamp = timestamp;
    live.username = Some("拾***据".into());
    live.author_id = Some("0".into());
    let mut events = vec![live];

    let mut history = DanmuEvent::new(DanmuEventKind::Danmu, "同一条弹幕");
    history.timestamp = timestamp + chrono::Duration::seconds(1);
    history.username = Some("拾穗数据".into());
    history.author_id = Some("42".into());
    history.origin = crate::domain::DanmuEventOrigin::History;

    assert!(reconcile_cross_origin_event(&mut events, &history));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].username.as_deref(), Some("拾穗数据"));
    assert_eq!(events[0].author_id.as_deref(), Some("42"));
}
#[test]
pub(crate) fn masked_live_echo_after_history_confirmation_is_not_inserted_twice() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut app = test_state(&temp, DanmuSession::new("1"));
    let submitted_at = Utc::now();
    let delivery = PendingDelivery::new(
        "古天乐，神殿侠侣，应该看过吧".into(),
        "停车拾穗".into(),
        "42".into(),
        submitted_at,
    );
    let (confirmation_tx, mut confirmation_rx) = oneshot::channel();
    app.handle_ui_event(CoreEvent::DeliveryStarted {
        delivery,
        confirmation: confirmation_tx,
        registered: oneshot::channel().0,
    });

    let mut history = DanmuEvent::new(DanmuEventKind::Danmu, "古天乐，神殿侠侣，应该看过吧");
    history.timestamp = submitted_at + chrono::Duration::seconds(1);
    history.username = Some("停车拾穗".into());
    history.author_id = Some("42".into());
    history.origin = DanmuEventOrigin::History;
    app.ingest_event(history);

    let mut live = DanmuEvent::new(DanmuEventKind::Danmu, "古天乐，神殿侠侣，应该看过吧");
    live.timestamp = submitted_at + chrono::Duration::seconds(8);
    live.username = Some("停***".into());
    live.author_id = Some("0".into());
    app.ingest_event(live);

    assert_eq!(confirmation_rx.try_recv(), Ok(()));
    assert_eq!(app.confirmed_deliveries.len(), 1);
    assert_eq!(app.session.recent_events.len(), 1);
    assert_eq!(
        app.session.recent_events[0].username.as_deref(),
        Some("停车拾穗")
    );
}
#[test]
pub(crate) fn app_shows_its_own_live_echo_immediately() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut app = test_state(&temp, DanmuSession::new("1"));
    let submitted_at = Utc::now();
    let delivery = PendingDelivery::new(
        "主播消息".into(),
        "拾穗数据".into(),
        "42".into(),
        submitted_at,
    );
    let (confirmation_tx, mut confirmation_rx) = oneshot::channel();
    app.handle_ui_event(CoreEvent::DeliveryStarted {
        delivery,
        confirmation: confirmation_tx,
        registered: oneshot::channel().0,
    });
    assert_eq!(app.delivery_status, DeliveryStatus::Sending);
    app.handle_ui_event(CoreEvent::DeliveryAccepted);
    assert_eq!(app.delivery_status, DeliveryStatus::AwaitingEcho);

    let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "主播消息");
    event.timestamp = submitted_at + chrono::Duration::seconds(1);
    event.username = Some("拾***据".into());
    event.author_id = Some("0".into());
    app.handle_client_event(BilibiliClientEvent::Danmu(event));

    assert_eq!(confirmation_rx.try_recv(), Ok(()));
    assert_eq!(app.session.recent_events.len(), 1);
    assert_eq!(
        app.session.recent_events[0].username.as_deref(),
        Some("拾穗数据")
    );
    assert!(app.last_live_danmu_at.is_some());
    assert_eq!(app.delivery_status, DeliveryStatus::Verifying);
    app.handle_ui_event(CoreEvent::DeliveryCompleted);
    assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
    assert!(app.notice.is_empty());
}

#[test]
pub(crate) fn segmented_send_tracks_multiple_unconfirmed_echoes_without_overwriting() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut app = test_state(&temp, DanmuSession::new("1"));
    let submitted_at = Utc::now();
    let first = PendingDelivery::new(
        "第一段".into(),
        "拾穗数据".into(),
        "42".into(),
        submitted_at,
    );
    let second = PendingDelivery::new(
        "第二段".into(),
        "拾穗数据".into(),
        "42".into(),
        submitted_at + chrono::Duration::seconds(2),
    );
    let (first_tx, mut first_rx) = oneshot::channel();
    let (second_tx, mut second_rx) = oneshot::channel();
    app.handle_ui_event(CoreEvent::DeliveryStarted {
        delivery: first,
        confirmation: first_tx,
        registered: oneshot::channel().0,
    });
    app.handle_ui_event(CoreEvent::DeliveryStarted {
        delivery: second,
        confirmation: second_tx,
        registered: oneshot::channel().0,
    });

    assert_eq!(app.pending_deliveries.len(), 2);
    let mut second_echo = DanmuEvent::new(DanmuEventKind::Danmu, "第二段");
    second_echo.timestamp = submitted_at + chrono::Duration::seconds(3);
    second_echo.username = Some("拾***据".into());
    second_echo.author_id = Some("0".into());
    app.ingest_event(second_echo);

    assert_eq!(second_rx.try_recv(), Ok(()));
    assert!(matches!(
        first_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(app.pending_deliveries.len(), 1);
    assert_eq!(app.delivery_status, DeliveryStatus::Verifying);

    let mut first_echo = DanmuEvent::new(DanmuEventKind::Danmu, "第一段");
    first_echo.timestamp = submitted_at + chrono::Duration::seconds(4);
    first_echo.username = Some("拾***据".into());
    first_echo.author_id = Some("0".into());
    app.ingest_event(first_echo);

    assert_eq!(first_rx.try_recv(), Ok(()));
    assert!(app.pending_deliveries.is_empty());
    app.handle_ui_event(CoreEvent::DeliveryCompleted);
    assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
}

#[tokio::test]
async fn runtime_confirms_fast_echo_without_a_terminal_or_observer() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let temp = crate::test_support::private_tempdir().unwrap();
    let account_path = temp.path().join("account.json");
    crate::storage::write_private_atomic(
        &account_path,
        &serde_json::to_vec(&serde_json::json!({
            "cookieHeader":"SESSDATA=fixture; bili_jct=fixture; DedeUserID=42",
            "csrf":"fixture", "identity":{"SignedIn":{"display_name":"用户42","user_id":"42"}}
        }))
        .unwrap(),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let mut state = RuntimeState::prepare(options(temp.path(), RuntimeMode::Live)).unwrap();
    state.account = state.account.with_test_endpoint(endpoint);
    state.session = DanmuSession::new("123");
    state.journal.start(&state.session).unwrap();
    let handle = spawn_owner(state);
    let (posted, post_seen) = oneshot::channel();
    let (respond, response_allowed) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut posted = Some(posted);
        let mut response_allowed = Some(response_allowed);
        let mut posts = 0;
        for body in [
            r#"{"code":0,"data":{"isLogin":true,"uname":"用户42","mid":42}}"#,
            r#"{"code":0,"data":{"mode_info":{"extra":{"content":"极速回声","send_from_me":true}}}}"#,
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            if request.starts_with(b"POST ") {
                posts += 1;
                posted.take().unwrap().send(()).unwrap();
                response_allowed.take().unwrap().await.unwrap();
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        posts
    });
    let accepted = handle
        .execute(RuntimeCommand::SendManual {
            text: "极速回声".into(),
            reply_to_message_id: None,
            draft_revision: 7,
        })
        .await
        .unwrap();
    assert_eq!(accepted.state, OperationState::Accepted);
    tokio::time::timeout(Duration::from_secs(2), post_seen)
        .await
        .unwrap()
        .unwrap();
    let mut echo = DanmuEvent::new(DanmuEventKind::Danmu, "极速回声");
    echo.author_id = Some("42".into());
    echo.username = Some("用户42".into());
    handle.inject_events(vec![echo], true).await.unwrap();
    respond.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let QueryResult::Diagnostics(diagnostics) =
                handle.query(RuntimeQuery::Diagnostics).await.unwrap()
            else {
                panic!("diagnostics result");
            };
            if diagnostics.operations.iter().any(|(id, state)| {
                *id == accepted.operation_id && *state == OperationState::Completed
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(server.await.unwrap(), 1);
    let QueryResult::Snapshot(snapshot) = handle.query(RuntimeQuery::Snapshot).await.unwrap()
    else {
        panic!("snapshot result");
    };
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.new_message_count, 1);
    // Attaching the first observer happens only after the transport/archival completion.
    let observer = handle.observe().await.unwrap();
    assert_eq!(
        observer
            .next_batch()
            .await
            .unwrap()
            .unwrap()
            .snapshot
            .unwrap()
            .events
            .len(),
        1
    );
    let (first, second) = tokio::join!(handle.shutdown(), handle.shutdown());
    first.unwrap();
    second.unwrap();
}

#[tokio::test]
async fn observation_resync_preserves_collection_and_session_cumulative_count() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let handle = open(options(temp.path(), RuntimeMode::Local))
        .await
        .unwrap();
    let slow = handle.observe().await.unwrap();
    let initial = slow.next_batch().await.unwrap().unwrap();
    assert!(initial.snapshot.unwrap().events.is_empty());
    for batch in 0..24 {
        let events = (0..64)
            .map(|offset| {
                let mut event =
                    DanmuEvent::new(DanmuEventKind::Danmu, format!("合成消息 {batch}-{offset}"));
                event.id = format!("bounded-{batch}-{offset}");
                event.author_id = Some("42".into());
                event
            })
            .collect();
        handle.inject_events(events, true).await.unwrap();
        // Force an atomic publication without consuming the deliberately paused observer.
        drop(handle.observe().await.unwrap());
    }
    let lagged = slow.next_batch().await.unwrap().unwrap();
    assert!(matches!(
        lagged.changes.as_slice(),
        [RuntimeChange::ResyncRequired]
    ));
    let fresh = handle.observe().await.unwrap();
    let snapshot = fresh.next_batch().await.unwrap().unwrap().snapshot.unwrap();
    assert_eq!(snapshot.new_message_count, 24 * 64);
    assert!(snapshot.events.len() <= 512);
    let session_id = snapshot.state.session_id;
    let QueryResult::Session(archived) = handle
        .query(RuntimeQuery::LoadSession { session_id })
        .await
        .unwrap()
    else {
        panic!("session result");
    };
    assert!(!archived.recent_events.is_empty());
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_viewer_messages_count_as_unread() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let handle = open(options(temp.path(), RuntimeMode::Local))
        .await
        .unwrap();
    let events = [
        DanmuEventKind::Enter,
        DanmuEventKind::Like,
        DanmuEventKind::Gift,
        DanmuEventKind::Follow,
        DanmuEventKind::Danmu,
        DanmuEventKind::Superchat,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, kind)| {
        let mut event = DanmuEvent::new(kind, format!("事件 {index}"));
        event.id = format!("unread-{index}");
        event
    })
    .collect();
    handle.inject_events(events, true).await.unwrap();
    let QueryResult::Snapshot(snapshot) = handle.query(RuntimeQuery::Snapshot).await.unwrap()
    else {
        panic!("snapshot result");
    };
    assert_eq!(snapshot.new_message_count, 2);
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn fixture_deduplication_and_gift_updates_keep_display_keys_and_unread_total() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let handle = open(options(temp.path(), RuntimeMode::Local))
        .await
        .unwrap();
    let fixture: Vec<DanmuEvent> =
        serde_json::from_str(include_str!("../../tests/fixtures/desktop-events.json")).unwrap();
    handle.inject_events(fixture.clone(), true).await.unwrap();
    let QueryResult::Snapshot(first) = handle.query(RuntimeQuery::Snapshot).await.unwrap() else {
        panic!("snapshot result");
    };
    assert_eq!(first.new_message_count, 3);
    let first_keys: HashMap<_, _> = first
        .events
        .iter()
        .map(|row| (row.event.id.clone(), row.message_key))
        .collect();
    handle.inject_events(fixture, true).await.unwrap();
    let QueryResult::Snapshot(second) = handle.query(RuntimeQuery::Snapshot).await.unwrap() else {
        panic!("snapshot result");
    };
    assert_eq!(second.new_message_count, 3);
    assert_eq!(
        second
            .events
            .iter()
            .map(|row| (row.event.id.clone(), row.message_key))
            .collect::<HashMap<_, _>>(),
        first_keys
    );
    assert_eq!(
        second
            .events
            .iter()
            .filter(|row| row.event.kind == DanmuEventKind::Gift)
            .count(),
        1
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn replay_has_no_model_platform_or_external_endpoint_commands() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let handle = open(options(temp.path(), RuntimeMode::Replay))
        .await
        .unwrap();
    for command in [
        RuntimeCommand::Connect {
            room_id: "123".into(),
        },
        RuntimeCommand::SendManual {
            text: "拒绝发送".into(),
            reply_to_message_id: None,
            draft_revision: 0,
        },
        RuntimeCommand::Assistant(AssistantCommand::RefreshModels),
        RuntimeCommand::SetExternalEndpoint {
            root: Some(temp.path().join("endpoint")),
        },
    ] {
        assert!(handle.execute(command).await.is_err());
    }
    let fixture: Vec<DanmuEvent> =
        serde_json::from_str(include_str!("../../tests/fixtures/desktop-events.json")).unwrap();
    handle.inject_events(fixture, true).await.unwrap();
    let QueryResult::Snapshot(snapshot) = handle.query(RuntimeQuery::Snapshot).await.unwrap()
    else {
        panic!("snapshot result");
    };
    assert_eq!(snapshot.new_message_count, 0);
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn workspace_default_is_explicit_and_storage_failure_never_becomes_a_default_runtime() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut configuration = options(temp.path(), RuntimeMode::Local);
    configuration.workspace_default = temp.path().join("GuiWorkspace");
    let expected = configuration.workspace_default.clone();
    let handle = open(configuration).await.unwrap();
    let QueryResult::Settings(settings) = handle.query(RuntimeQuery::Settings).await.unwrap()
    else {
        panic!("settings result");
    };
    assert_eq!(settings.resolved_workspace, expected);
    handle.shutdown().await.unwrap();
    crate::storage::write_private_atomic(&temp.path().join("account.json"), b"{broken").unwrap();
    assert!(
        open(options(temp.path(), RuntimeMode::Local))
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(temp.path().join("account.json")).unwrap(),
        b"{broken"
    );
}

#[tokio::test]
async fn restored_rows_with_reused_platform_ids_have_independent_stable_display_keys() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut first = DanmuEvent::new(DanmuEventKind::Danmu, "旧版第一条消息");
    first.id = String::new();
    first.timestamp = DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut second = first.clone();
    second.content = "旧版第二条消息".into();
    let mut session = DanmuSession::new("123");
    session.preload_history([first, second]);
    let mut state = test_state(&temp, session);
    let before = state.snapshot();
    assert_ne!(before.events[0].message_key, before.events[1].message_key);
    state.session.recent_events[0].username = Some("恢复后的完整名字".into());
    state.session.recent_events[0].origin = DanmuEventOrigin::History;
    let after = state.snapshot();
    assert_eq!(after.events[0].message_key, before.events[0].message_key);
    assert_eq!(after.events[1].message_key, before.events[1].message_key);
}

#[tokio::test]
async fn saved_obs_configuration_reopens_without_permission_repair() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let options = options(temp.path(), RuntimeMode::Local);
    let configuration = crate::obs::ObsConfiguration {
        host: "127.0.0.1".into(),
        port: 4466,
        microphone_input_name: "合成配置麦克风".into(),
        default_live_scene: "合成场景".into(),
    };
    configuration
        .save(&options.storage.obs_configuration)
        .unwrap();
    let handle = open(options).await.unwrap();
    let QueryResult::Settings(saved) = handle.query(RuntimeQuery::Settings).await.unwrap() else {
        panic!("settings result");
    };
    assert_eq!(saved.obs, configuration);
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_connect_completion_is_terminal_and_disconnect_does_not_reclassify_it() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let handle = open(options(temp.path(), RuntimeMode::Local))
        .await
        .unwrap();
    let connected = handle
        .execute(RuntimeCommand::Connect {
            room_id: "123".into(),
        })
        .await
        .unwrap();
    assert_eq!(connected.state, OperationState::Completed);
    handle.execute(RuntimeCommand::Disconnect).await.unwrap();
    let QueryResult::Diagnostics(diagnostics) =
        handle.query(RuntimeQuery::Diagnostics).await.unwrap()
    else {
        panic!("diagnostics result");
    };
    assert!(diagnostics.operations.iter().any(|(id, state)| *id == connected.operation_id && *state == OperationState::Completed));
    handle.shutdown().await.unwrap();
}
#[test]
fn startup_history_creates_a_fresh_session_without_live_metrics() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let journal = SessionJournal::new(temp.path().join("sessions"));
    let mut archived = DanmuSession::new("42");
    let event = DanmuEvent::new(DanmuEventKind::Danmu, "上次直播的弹幕");
    archived.ingest(event.clone());
    journal.start(&archived).unwrap();
    archived.end(Utc::now(), DanmuSessionEndReason::Completed);
    journal.end(&archived).unwrap();
    let source = temp
        .path()
        .join("sessions")
        .join(&archived.id)
        .join("journal.jsonl");
    let original = std::fs::read(&source).unwrap();
    let loaded = RuntimeState::recover_session(&journal, "42").unwrap();

    assert_ne!(loaded.id, archived.id);
    assert_eq!(loaded.status, crate::domain::DanmuSessionStatus::Active);
    assert_eq!(loaded.metrics, crate::domain::SessionMetrics::default());
    assert_eq!(loaded.recent_events.len(), 1);
    assert_eq!(loaded.recent_events[0].id, event.id);
    assert_eq!(loaded.recent_events[0].origin, DanmuEventOrigin::Archived);
    journal.start(&loaded).unwrap();
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[tokio::test]
async fn explicit_pause_is_not_a_fatal_notification_and_does_not_erase_an_existing_failure() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut state = test_state(&temp, DanmuSession::new("1"));
    state.pause_assistant();
    assert!(state.state_record().assistant_failure.is_none());
    let failure = state.fail_assistant("受控传输故障；已暂停".into());
    assert_eq!(
        state.state_record().assistant_failure.as_deref(),
        Some(failure.as_str())
    );
    state.pause_assistant();
    state.stop_assistant();
    assert_eq!(
        state.state_record().assistant_failure.as_deref(),
        Some(failure.as_str())
    );
}

#[tokio::test]
async fn worker_marks_update_observers_without_mutating_content_keys_or_unread() {
    let temp = crate::test_support::private_tempdir().unwrap();
    let mut state = test_state(&temp, DanmuSession::new("1"));
    state.ingestion_live = true;
    let event = DanmuEvent::new(DanmuEventKind::Danmu, "请处理这条公开问题");
    state.ingest_event(event.clone());
    let (sender, mut receiver) = tokio::sync::broadcast::channel(64);
    let (watermark, _) = watch::channel((0, 0));
    let mut sequence = 0;
    let mut cache = None;
    publish_changes(&mut state, &sender, &watermark, &mut sequence, &mut cache);
    let mut key = None;
    while let Ok(change) = receiver.try_recv() {
        if let RuntimeChange::Upsert(record) = change.change {
            key = Some(record.message_key);
        }
    }
    let key = key.expect("initial row");
    let count = state.new_message_count;
    for (index, report, expected) in [
        (0, bridge::ReportState::Processing, bridge::Mark::Processing),
        (1, bridge::ReportState::Finished, bridge::Mark::Finished),
        (2, bridge::ReportState::Failed, bridge::Mark::Failed),
    ] {
        state
            .bridge
            .apply(bridge::Request::Report {
                session: state.bridge.status()["session"].as_str().unwrap().into(),
                caller: "observer-regression".into(),
                request_id: format!("report-{index}"),
                message_id: event.id.clone(),
                state: report,
            })
            .unwrap();
        publish_changes(&mut state, &sender, &watermark, &mut sequence, &mut cache);
        let mut row = None;
        while let Ok(change) = receiver.try_recv() {
            if let RuntimeChange::Upsert(record) = change.change {
                row = Some(record);
            }
        }
        let row = row.expect("mark-only update must reach observer");
        assert_eq!(row.message_key, key);
        assert_eq!(row.event, event);
        assert_eq!(row.assistant_state, Some(expected));
        assert_eq!(state.new_message_count, count);
    }
    publish_changes(&mut state, &sender, &watermark, &mut sequence, &mut cache);
    assert!(
        receiver.try_recv().is_err(),
        "unchanged marks do not produce duplicate updates"
    );
}
