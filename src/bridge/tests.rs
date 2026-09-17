use super::*;
use crate::{
    delivery::{SendQueue, Transport},
    domain::DanmuEventKind,
};
fn fixture() -> (Bridge, String, String) {
    let b = Bridge::new(true);
    let e = DanmuEvent::new(DanmuEventKind::Danmu, "问题");
    let id = e.id.clone();
    b.ingest(e);
    let s = b.status()["session"].as_str().unwrap().to_owned();
    (b, s, id)
}
fn reply(s: &str, m: &str, c: &str, r: &str, candidate: bool) -> Request {
    Request::Reply {
        session: s.into(),
        caller: c.into(),
        request_id: r.into(),
        message_id: m.into(),
        text: "回答".into(),
        candidate,
    }
}
#[test]
fn batch_approval_is_atomic_and_does_not_authorize_later_candidates() {
    let (b, session, id) = fixture();
    b.claim_driver("owner").unwrap();
    for request in ["first", "second"] {
        b.apply_owned_reply(
            "owner",
            reply(&session, &id, RUNNER_CALLER, request, true),
            false,
        )
        .unwrap();
    }
    let stale = b.candidates();
    b.edit_candidate(&session, RUNNER_CALLER, "second", "已修改的候选")
        .unwrap();
    assert!(b.approve_candidates(&stale).is_err());
    assert!(b.take_ready().is_none());
    assert_eq!(b.candidate_count(), 2);
    b.approve_candidates(&b.candidates()).unwrap();
    b.apply_owned_reply(
        "owner",
        reply(&session, &id, RUNNER_CALLER, "later", true),
        false,
    )
    .unwrap();
    assert_eq!(b.take_ready().unwrap().reply.request_id, "first");
    assert_eq!(b.take_ready().unwrap().reply.request_id, "second");
    assert!(b.take_ready().is_none());
    assert_eq!(b.candidates()[0].request_id, "later");
    assert!(!b.sending_enabled());
}

#[test]
fn caller_scoped_idempotency_allows_multiple_replies_to_one_message() {
    let (b, s, m) = fixture();
    b.permission(true).unwrap();
    let req = reply(&s, &m, "one", "1", false);
    b.apply(req.clone()).unwrap();
    b.apply(req).unwrap();
    b.apply(reply(&s, &m, "two", "1", false)).unwrap();
    let a = b.take_ready().unwrap();
    let c = b.take_ready().unwrap();
    assert_ne!(a.reply.caller, c.reply.caller);
    assert!(b.take_ready().is_none());
    let mut conflict = reply(&s, &m, "one", "1", false);
    if let Request::Reply { text, .. } = &mut conflict {
        *text = "不同正文".into();
    }
    assert!(b.apply(conflict).is_err());
}
#[test]
fn permission_preserves_candidates_but_revokes_old_dispatch_and_keeps_real_results() {
    let (b, s, m) = fixture();
    b.permission(true).unwrap();
    b.apply(reply(&s, &m, "a", "flight", false)).unwrap();
    let flight = b.take_ready().unwrap();
    b.permission(true).unwrap();
    let candidate = reply(&s, &m, "a", "pending", true);
    b.apply(candidate.clone()).unwrap();
    let old_direct = reply(&s, &m, "a", "old-direct", false);
    b.apply(old_direct.clone()).unwrap();
    b.permission(false).unwrap();
    assert!(!flight.permit.valid());
    assert_eq!(b.candidates()[0].request_id, "pending");
    assert_eq!(b.apply(candidate).unwrap()["state"], "awaiting_approval");
    assert_eq!(b.apply(old_direct.clone()).unwrap()["state"], "cancelled");
    b.complete(&flight.reply, Outcome::Confirmed.into());
    assert_eq!(b.mark(&m), Some(Mark::Confirmed));
    b.permission(true).unwrap();
    assert!(b.take_ready().is_none());
    assert_eq!(b.apply(old_direct).unwrap()["state"], "cancelled");
    b.end();
    assert!(b.candidates().is_empty());
    assert!(b.edit_candidate(&s, "a", "pending", "迟到编辑").is_err());
    b.new_session();
    assert!(b.apply(reply(&s, &m, "a", "new", false)).is_err());
    b.complete(&flight.reply, Outcome::Confirmed.into());
    assert_eq!(b.mark(&m), None);
    assert_eq!(b.status()["sending_enabled"], false);
}
#[test]
fn reading_never_marks_processing_and_sender_cannot_report_success() {
    let (b, s, m) = fixture();
    b.apply(Request::Messages {
        session: s.clone(),
        cursor: 0,
        limit: 1,
        wait_ms: 0,
    })
    .unwrap();
    assert_eq!(b.mark(&m), None);
    assert!(serde_json::from_value::<Request>(json!({"op":"report","session":s,"caller":"a","request_id":"x","message_id":m,"state":"confirmed"})).is_err());
}
#[tokio::test]
async fn increment_order_gap_and_wait_follow_arrival_not_event_timestamp() {
    let b = Bridge::new(true);
    let s = b.status()["session"].as_str().unwrap().to_string();
    for i in 0..HISTORY + 2 {
        let mut e = DanmuEvent::new(DanmuEventKind::Danmu, i.to_string());
        e.timestamp = Utc::now() - Duration::seconds(i as i64);
        b.ingest(e);
    }
    let v = b
        .call(Request::Messages {
            session: s.clone(),
            cursor: 0,
            limit: 2,
            wait_ms: 0,
        })
        .await
        .unwrap();
    assert_eq!(v["gap"], true);
    assert_eq!(v["messages"][0]["content"], "2");
    assert_eq!(v["cursor"], 4);
    let copy = b.clone();
    let session = s.clone();
    let waiter = tokio::spawn(async move {
        copy.call(Request::Messages {
            session,
            cursor: (HISTORY + 2) as u64,
            limit: 1,
            wait_ms: 1000,
        })
        .await
        .unwrap()
    });
    tokio::task::yield_now().await;
    b.ingest(DanmuEvent::new(DanmuEventKind::Danmu, "唤醒"));
    assert_eq!(waiter.await.unwrap()["messages"][0]["content"], "唤醒");
    let v = b
        .call(Request::Messages {
            session: s,
            cursor: (HISTORY + 3) as u64,
            limit: 1,
            wait_ms: 1,
        })
        .await
        .unwrap();
    assert_eq!(v["messages"], json!([]));
}
struct Confirm;
impl Transport for Confirm {
    async fn send_confirm(&self, _: &str, _: Option<&str>) -> Delivery {
        Outcome::Confirmed.into()
    }
}
struct Unknown;
impl Transport for Unknown {
    async fn send_confirm(&self, _: &str, _: Option<&str>) -> Delivery {
        Outcome::Uncertain.into()
    }
}
#[tokio::test]
async fn agent_pause_and_unknown_do_not_block_manual_and_unknown_never_turns_green() {
    let (b, s, m) = fixture();
    b.permission(true).unwrap();
    let req = reply(&s, &m, "a", "r", false);
    b.apply(req.clone()).unwrap();
    let job = b.take_ready().unwrap();
    let waiting = reply(&s, &m, "b", "waiting", true);
    b.apply(waiting.clone()).unwrap();
    let queue = SendQueue::default();
    let text = vec!["同一回复".into()];
    let outcome = queue.send(&Unknown, &text, Some(&job.permit), None).await;
    b.complete(&job.reply, outcome);
    assert_eq!(b.mark(&m), Some(Mark::Failed));
    assert_eq!(b.status()["sending_enabled"], false);
    assert_eq!(b.apply(req).unwrap()["state"], "uncertain");
    assert_eq!(b.apply(waiting).unwrap()["state"], "awaiting_approval");
    assert!(b.take_ready().is_none());
    assert_eq!(
        queue.send(&Confirm, &text, None, None).await.outcome,
        Outcome::Confirmed
    );
    b.permission(true).unwrap();
    b.apply(reply(&s, &m, "a", "new", false)).unwrap();
    let job = b.take_ready().unwrap();
    b.permission(false).unwrap();
    assert_eq!(
        queue
            .send(&Confirm, &text, Some(&job.permit), None)
            .await
            .outcome,
        Outcome::Cancelled
    );
    assert_eq!(
        queue.send(&Confirm, &text, None, None).await.outcome,
        Outcome::Confirmed
    );
}
#[test]
fn reconnect_requires_fresh_permission_and_does_not_replay() {
    let (b, s, m) = fixture();
    b.permission(true).unwrap();
    b.apply(reply(&s, &m, "a", "r", false)).unwrap();
    let waiting = reply(&s, &m, "b", "waiting", true);
    b.apply(waiting.clone()).unwrap();
    b.available(false);
    assert_eq!(
        b.apply(waiting.clone()).unwrap()["state"],
        "awaiting_approval"
    );
    b.available(true);
    assert!(b.take_ready().is_none());
    b.permission(true).unwrap();
    assert!(b.take_ready().is_none());
    assert_eq!(
        b.apply(reply(&s, &m, "a", "r", false)).unwrap()["state"],
        "cancelled"
    );
}

#[test]
fn reports_and_permission_changes_cannot_hide_actual_delivery_outcomes() {
    let (b, s, m) = fixture();
    b.permission(true).unwrap();
    b.apply(reply(&s, &m, "a", "unknown", false)).unwrap();
    let job = b.take_ready().unwrap();
    b.complete(&job.reply, Outcome::Uncertain.into());
    b.apply(Request::Report {
        session: s.clone(),
        caller: "a".into(),
        request_id: "finished".into(),
        message_id: m.clone(),
        state: ReportState::Finished,
    })
    .unwrap();
    assert_eq!(b.mark(&m), Some(Mark::Failed));
    b.permission(true).unwrap();
    b.apply(reply(&s, &m, "other", "success", false)).unwrap();
    let job = b.take_ready().unwrap();
    b.complete(&job.reply, Outcome::Confirmed.into());
    b.apply(reply(&s, &m, "other", "candidate", true)).unwrap();
    b.permission(false).unwrap();
    assert_eq!(b.mark(&m), Some(Mark::Confirmed));
}
#[tokio::test]
async fn legitimate_large_batch_round_trips_without_using_request_frame_limit() {
    let root = tempfile::tempdir().unwrap();
    let b = Bridge::new(true);
    let s = b.status()["session"].as_str().unwrap().to_owned();
    for n in 0..100 {
        b.ingest(DanmuEvent::new(
            DanmuEventKind::Danmu,
            format!("{n}:{}", "内容".repeat(200)),
        ));
    }
    let instance = root.path().join("private");
    let server = wire::serve(&instance, b).await.unwrap();
    let value = wire::call(
        &instance,
        Request::Messages {
            session: s,
            cursor: 0,
            limit: 100,
            wait_ms: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        value["messages"][99]["content"],
        format!("99:{}", "内容".repeat(200))
    );
    assert_eq!(value["cursor"], 100);
    drop(server);
    assert!(!instance.join("instance.json").exists());
}

#[test]
fn native_reply_target_is_owned_and_frozen_before_review() {
    let b = Bridge::new(true);
    b.claim_driver("owner").unwrap();
    let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "问题");
    event.author_id = Some("42".into());
    event.username = Some("原作者".into());
    event.reply_to = Some("原作者正在回复的其他人".into());
    let id = event.id.clone();
    b.ingest(event.clone());
    let session = b.session_id();
    let req = reply(&session, &id, RUNNER_CALLER, "target", true);
    assert!(b.apply_owned_reply("wrong", req.clone(), true).is_err());
    assert!(b.apply_owned_reply("owner", Request::Status, true).is_err());
    assert!(
        b.apply_owned_reply("owner", reply(&session, &id, "other", "x", true), true)
            .is_err()
    );
    assert!(b.apply_owned("owner", req.clone()).is_err());
    let original = b.apply_owned_reply("owner", req.clone(), true).unwrap();
    assert_eq!(
        original["reply_to"],
        json!({"user_id":"42", "username":"原作者"})
    );
    assert!(b.decide(RUNNER_CALLER, "target", true).is_err());
    assert!(b.take_ready().is_none());
    b.edit_candidate(&session, RUNNER_CALLER, "target", "编辑后正文")
        .unwrap();
    event.author_id = Some("99".into());
    event.username = Some("变化昵称".into());
    b.ingest(event);
    for _ in 0..HISTORY {
        b.ingest(DanmuEvent::new(DanmuEventKind::Danmu, "后续消息"));
    }
    let retried = b.apply_owned_reply("owner", req, false).unwrap();
    assert_eq!(retried["reply_to"], original["reply_to"]);
    assert_eq!(retried["text"], "编辑后正文");
    b.permission(true).unwrap();
    b.decide(RUNNER_CALLER, "target", true).unwrap();
    let job = b.take_ready().unwrap();
    assert_eq!(job.reply.reply_to.unwrap().user_id, "42");
    b.new_session();
    assert!(!job.permit.valid());
    assert!(b.take_ready().is_none());
}

#[test]
fn native_reply_never_invents_a_missing_or_invalid_author() {
    let b = Bridge::new(true);
    b.claim_driver("owner").unwrap();
    let session = b.session_id();
    for (index, author) in [
        None,
        Some(""),
        Some("0"),
        Some("-1"),
        Some("+42"),
        Some(" 42"),
        Some("alice"),
        Some("18446744073709551616"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "问题");
        event.author_id = author.map(str::to_owned);
        event.username = Some("不能用昵称猜UID".into());
        let id = event.id.clone();
        b.ingest(event);
        let key = index.to_string();
        let req = reply(&session, &id, RUNNER_CALLER, &key, true);
        let value = b.apply_owned_reply("owner", req, true).unwrap();
        assert!(value.get("reply_to").is_none(), "{author:?}");
        assert_eq!(value["state"], "awaiting_approval");
        b.decide(RUNNER_CALLER, &key, false).unwrap();
    }
    b.permission(true).unwrap();
    assert!(b.take_ready().is_none());
    let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "有UID无昵称");
    event.author_id = Some("42".into());
    let id = event.id.clone();
    b.ingest(event);
    let disabled = b
        .apply_owned_reply(
            "owner",
            reply(&session, &id, RUNNER_CALLER, "off", true),
            false,
        )
        .unwrap();
    assert!(disabled.get("reply_to").is_none());
    let enabled = b
        .apply_owned_reply(
            "owner",
            reply(&session, &id, RUNNER_CALLER, "on", true),
            true,
        )
        .unwrap();
    assert_eq!(enabled["reply_to"], json!({"user_id":"42", "username":""}));
}

fn repair_fixture() -> (Bridge, Reply, Value) {
    let (b, session, id) = fixture();
    b.claim_driver("repair-owner").unwrap();
    b.owned_policy("repair-owner", true, &[]);
    b.permission(true).unwrap();
    b.apply_owned_reply(
        "repair-owner",
        reply(&session, &id, RUNNER_CALLER, "original", false),
        true,
    )
    .unwrap();
    let job = b.take_ready().unwrap();
    let mut delivery = Delivery::new(
        Outcome::Rejected,
        crate::delivery::Cause::ContentRejected,
        "平台明确内容拒绝",
    );
    delivery.diagnosis.response_received = true;
    delivery.confirmed_segments = vec!["✦ 已确认前段".into()];
    delivery.unconfirmed_segments = vec!["✦ 失败后段".into()];
    b.complete(&job.reply, delivery);
    let terminal = b
        .apply(Request::Result {
            session,
            caller: RUNNER_CALLER.into(),
            request_id: "original".into(),
        })
        .unwrap();
    (b, job.reply, terminal)
}
#[test]
fn repair_keeps_original_result_and_never_revives_revoked_expired_or_cross_scene_authority() {
    for transition in [
        "revoke",
        "new_scene",
        "end",
        "expired",
        "late_echo",
        "release",
    ] {
        let (b, original, terminal) = repair_fixture();
        let mut repair = b.take_repair("repair-owner").unwrap();
        match transition {
            "revoke" => {
                b.permission(false).unwrap();
                b.permission(true).unwrap();
            }
            "new_scene" => b.new_session(),
            "end" => b.end(),
            "expired" => repair.permit = Permit::new(Utc::now() - Duration::seconds(1)),
            "late_echo" => b.late_echo(&original),
            "release" => b.release_driver("repair-owner"),
            _ => unreachable!(),
        }
        assert!(
            b.apply_repair("repair-owner", &repair, "正常改写".into(), true)
                .is_err(),
            "{transition}"
        );
        assert!(b.take_ready().is_none());
        if transition != "new_scene" {
            assert_eq!(
                b.apply(Request::Result {
                    session: original.session,
                    caller: original.caller,
                    request_id: original.request_id
                })
                .unwrap(),
                terminal
            );
        }
    }
}
#[test]
fn repair_is_one_shot_and_late_echo_cancels_unsent_but_reports_inflight_duplicate_risk() {
    for in_flight in [false, true] {
        let (b, original, terminal) = repair_fixture();
        let repair = b.take_repair("repair-owner").unwrap();
        assert!(b.take_repair("repair-owner").is_none());
        assert!(
            b.apply_repair("repair-owner", &repair, "失败后段".into(), true)
                .is_err()
        );
        assert!(
            b.apply_repair("repair-owner", &repair, "已确认前段，正常后段".into(), true)
                .is_err()
        );
        let value = b
            .apply_repair("repair-owner", &repair, "正常改写".into(), true)
            .unwrap();
        assert!(
            b.apply_repair("repair-owner", &repair, "再次改写".into(), true)
                .is_err()
        );
        let job = if in_flight {
            Some(b.take_ready().unwrap())
        } else {
            None
        };
        b.late_echo(&original);
        if let Some(job) = job {
            b.complete(&job.reply, Outcome::Confirmed.into());
        }
        assert!(b.take_ready().is_none());
        let result = b
            .apply(Request::Result {
                session: original.session.clone(),
                caller: original.caller.clone(),
                request_id: value["request_id"].as_str().unwrap().into(),
            })
            .unwrap();
        assert_eq!(
            result["state"],
            if in_flight { "confirmed" } else { "cancelled" }
        );
        if in_flight {
            assert_eq!(result["duplicate_risk"], true);
        }
        assert_eq!(
            b.apply(Request::Result {
                session: original.session,
                caller: original.caller,
                request_id: original.request_id
            })
            .unwrap(),
            terminal
        );
    }
}
#[test]
fn external_caller_never_acquires_owned_repair_and_invisible_evasion_is_rejected() {
    let (b, session, id) = fixture();
    b.permission(true).unwrap();
    b.apply(reply(&session, &id, "external", "external", false))
        .unwrap();
    let job = b.take_ready().unwrap();
    let mut delivery = Delivery::new(
        Outcome::Uncertain,
        crate::delivery::Cause::EchoMissing,
        "无回显",
    );
    delivery.unconfirmed_segments = vec!["正文".into()];
    b.complete(&job.reply, delivery);
    b.claim_driver("repair-owner").unwrap();
    assert!(b.take_repair("repair-owner").is_none());
    assert!(Bridge::validate_reply_text("不\u{200b}拆字").is_err());
}

#[test]
fn reviewed_snapshot_authorizes_only_that_version_and_not_automatic_sends() {
    let (b, session, id) = fixture();
    b.permission(true).unwrap();
    b.apply(reply(&session, &id, "review", "first", true))
        .unwrap();
    b.apply(reply(&session, &id, "review", "second", true))
        .unwrap();
    b.permission(false).unwrap();
    let shown = b.candidates()[0].clone();
    b.edit_candidate(&session, "review", "first", "修改过的正文")
        .unwrap();
    b.edit_candidate(&session, "review", "first", &shown.text)
        .unwrap();
    assert!(
        b.approve_candidates(std::slice::from_ref(&shown)).is_err(),
        "改回相同正文仍须重新显示"
    );
    let shown = b.candidates()[0].clone();
    b.approve_candidates(std::slice::from_ref(&shown)).unwrap();
    assert!(b.approve_candidates(std::slice::from_ref(&shown)).is_err());
    let job = b.take_ready().unwrap();
    assert_eq!(job.reply.request_id, "first");
    assert!(b.job_authorized(&job.reply));
    assert!(!b.sending_enabled());
    assert!(b.take_ready().is_none());
    assert_eq!(
        b.apply(reply(&session, &id, "external", "auto", false))
            .unwrap()["state"],
        "rejected"
    );
    b.identity_changed();
    assert!(!job.permit.valid());
    b.complete(&job.reply, Outcome::Confirmed.into());
    assert_eq!(b.mark(&id), Some(Mark::Confirmed));
}

#[test]
fn sender_switch_preserves_reviewed_draft_but_requires_fresh_approval() {
    let (b, session, id) = fixture();
    b.permission(true).unwrap();
    b.apply(reply(&session, &id, "review", "draft", true))
        .unwrap();
    b.permission(false).unwrap();
    let shown = b.candidates()[0].clone();
    b.approve_candidates(std::slice::from_ref(&shown)).unwrap();
    b.identity_changed();
    assert!(b.take_ready().is_none());
    assert_eq!(b.candidates()[0].text, shown.text);
    assert!(b.approve_candidates(std::slice::from_ref(&shown)).is_err());
    b.approve_candidates(std::slice::from_ref(&b.candidates()[0]))
        .unwrap();
    assert!(b.take_ready().is_some());
    assert!(!b.sending_enabled());
}

#[tokio::test]
async fn missing_echo_never_retries_or_repairs_and_manual_sending_remains_available() {
    struct MissingEcho;
    impl Transport for MissingEcho {
        async fn send_confirm(&self, _: &str, _: Option<&str>) -> Delivery {
            let mut result = Delivery::new(
                Outcome::Uncertain,
                crate::delivery::Cause::EchoMissing,
                "没有真实回显",
            );
            result.diagnosis.response_received = true;
            result.diagnosis.platform_code = Some(0);
            result
        }
    }
    let (b, session, id) = fixture();
    b.claim_driver("owner").unwrap();
    b.owned_policy("owner", true, &[]);
    b.permission(true).unwrap();
    let request = reply(&session, &id, RUNNER_CALLER, "uncertain", false);
    b.apply_owned_reply("owner", request.clone(), false)
        .unwrap();
    let job = b.take_ready().unwrap();
    let queue = SendQueue::default();
    let delivery = queue
        .send(
            &MissingEcho,
            &["原段".into(), "后段".into()],
            Some(&job.permit),
            None,
        )
        .await;
    assert_eq!(delivery.unconfirmed_segments, ["原段", "后段"]);
    b.complete(&job.reply, delivery);
    assert!(b.take_repair("owner").is_none());
    assert!(b.take_ready().is_none());
    assert_eq!(
        b.apply_owned_reply("owner", request, false).unwrap()["state"],
        "uncertain"
    );
    assert!(b.sending_enabled());
    let event = DanmuEvent::new(DanmuEventKind::Danmu, "后续新问题");
    let next = reply(&session, &event.id, RUNNER_CALLER, "following", false);
    b.ingest(event);
    b.apply_owned_reply("owner", next.clone(), false).unwrap();
    let following = b.take_ready().unwrap();
    assert_eq!(following.reply.request_id, "following");
    let delivered = queue
        .send(&Confirm, &["新回复".into()], Some(&following.permit), None)
        .await;
    b.complete(&following.reply, delivered);
    assert_eq!(
        b.apply_owned_reply("owner", next, false).unwrap()["state"],
        "confirmed"
    );
    assert_eq!(
        queue
            .send(&Confirm, &["人工回复".into()], None, None)
            .await
            .outcome,
        Outcome::Confirmed
    );
    b.late_echo(&job.reply);
    // Only the first segment arrived late; the never-sent tail is not delivery proof.
    assert_eq!(b.mark(&id), Some(Mark::Failed));
}

fn routing_policy() -> routing::Policy {
    routing::Policy {
        author: Some("self-account".into()),
        name: "主播".into(),
        thank_gifts: true,
        thank_likes: true,
        thank_follows: true,
        thank_shares: true,
    }
}

fn routed_event(id: &str, kind: DanmuEventKind, author: &str, content: &str) -> DanmuEvent {
    let mut event = DanmuEvent::new(kind, content);
    event.id = id.into();
    event.author_id = Some(author.into());
    event.username = Some(format!("user-{author}"));
    event
}

#[test]
fn routing_lease_freezes_owned_reports_and_replies_beyond_raw_history() {
    let b = Bridge::new(true);
    b.claim_driver("owner").unwrap();
    b.start_routing("owner", routing_policy(), false).unwrap();
    let original = routed_event("leased-source", DanmuEventKind::Danmu, "42", "最早的问题");
    let session = b.session_id();
    b.ingest(original);
    let models = b.routing_models("owner").unwrap();
    assert_eq!(
        models
            .iter()
            .map(|message| message.event.id.as_str())
            .collect::<Vec<_>>(),
        ["leased-source"]
    );
    b.consume_routing_models("owner", &["leased-source".into()])
        .unwrap();
    assert!(
        b.consume_routing_models("owner", &["leased-source".into()])
            .is_err()
    );

    for index in 0..600 {
        b.ingest(routed_event(
            &format!("later-{index}"),
            DanmuEventKind::Danmu,
            "84",
            "后续消息",
        ));
    }
    let outsider_report = Request::Report {
        session: session.clone(),
        caller: "outsider".into(),
        request_id: "outsider-report".into(),
        message_id: "leased-source".into(),
        state: ReportState::Processing,
    };
    assert!(b.apply(outsider_report).is_err());
    assert!(
        b.apply(reply(
            &session,
            "leased-source",
            "outsider",
            "outsider-reply",
            true,
        ))
        .is_err()
    );

    b.apply_owned(
        "owner",
        Request::Report {
            session: session.clone(),
            caller: RUNNER_CALLER.into(),
            request_id: "owned-report".into(),
            message_id: "leased-source".into(),
            state: ReportState::Processing,
        },
    )
    .unwrap();
    let response = b
        .apply_owned_reply(
            "owner",
            reply(
                &session,
                "leased-source",
                RUNNER_CALLER,
                "owned-reply",
                true,
            ),
            true,
        )
        .unwrap();
    assert_eq!(
        response["reply_to"],
        json!({"user_id":"42", "username":"user-42"})
    );
    assert_eq!(response["state"], "awaiting_approval");
    assert_eq!(b.routing_snapshot()["leased"], json!(["leased-source"]));
    // Reuse of an ID in the rolling raw window cannot retarget a leased question.
    b.ingest(routed_event(
        "leased-source",
        DanmuEventKind::Danmu,
        "99",
        "复用ID的新内容",
    ));
    let reused = b
        .apply_owned_reply(
            "owner",
            reply(
                &session,
                "leased-source",
                RUNNER_CALLER,
                "after-reuse",
                true,
            ),
            true,
        )
        .unwrap();
    assert_eq!(
        reused["reply_to"],
        json!({"user_id":"42", "username":"user-42"})
    );
    for index in 0..512 {
        b.ingest(routed_event(
            &format!("reuse-tail-{index}"),
            DanmuEventKind::Like,
            "84",
            "点赞",
        ));
    }

    b.expire_routing_flight("owner").unwrap();
    assert_eq!(b.routing_snapshot()["queue"]["expired"], 1);
    b.finish_routing_models("owner");
    assert_eq!(b.routing_snapshot()["leased"], json!([]));
    // A terminal generation report may arrive after the model lease is released. Its owned
    // reply snapshot remains authoritative, while outsiders still cannot guess the old ID.
    b.apply_owned(
        "owner",
        Request::Report {
            session: session.clone(),
            caller: RUNNER_CALLER.into(),
            request_id: "owned-terminal-report".into(),
            message_id: "leased-source".into(),
            state: ReportState::Finished,
        },
    )
    .unwrap();
    assert!(
        b.apply(reply(
            &session,
            "leased-source",
            "outsider",
            "outsider-after-finish",
            true,
        ))
        .is_err()
    );
}

#[test]
fn routing_templates_require_grant_or_review_and_never_enter_repair() {
    let review = Bridge::new(true);
    review.claim_driver("review-owner").unwrap();
    review
        .start_routing("review-owner", routing_policy(), false)
        .unwrap();
    review.ingest(routed_event(
        "review-follow",
        DanmuEventKind::Danmu,
        "110",
        "复用ID的旧弹幕",
    ));
    review
        .consume_routing_models("review-owner", &["review-follow".into()])
        .unwrap();
    for index in 0..512 {
        review.ingest(routed_event(
            &format!("review-filler-{index}"),
            DanmuEventKind::Danmu,
            "110",
            "填充历史",
        ));
    }
    review.ingest(routed_event(
        "review-follow",
        DanmuEventKind::Follow,
        "11",
        "关注了主播",
    ));

    let automatic = Bridge::new(true);
    automatic.claim_driver("automatic-owner").unwrap();
    automatic
        .start_routing("automatic-owner", routing_policy(), false)
        .unwrap();
    automatic.ingest(routed_event(
        "automatic-follow",
        DanmuEventKind::Follow,
        "12",
        "关注了主播",
    ));
    let fallback = Bridge::new(true);
    fallback.claim_driver("fallback-owner").unwrap();
    fallback
        .start_routing("fallback-owner", routing_policy(), false)
        .unwrap();
    let mut fallback_event = routed_event(
        "fallback-follow",
        DanmuEventKind::Follow,
        "invalid-uid",
        "关注了主播",
    );
    fallback_event.username = Some("昵\n称\u{200b}\u{2028}甲".into());
    fallback.ingest(fallback_event);

    let anonymous = Bridge::new(true);
    anonymous.claim_driver("anonymous-owner").unwrap();
    anonymous
        .start_routing("anonymous-owner", routing_policy(), false)
        .unwrap();
    let mut anonymous_event = routed_event(
        "anonymous-follow",
        DanmuEventKind::Follow,
        "invalid-uid",
        "关注了主播",
    );
    anonymous_event.author_id = None;
    anonymous_event.username = None;
    anonymous.ingest(anonymous_event);

    let saturated = Bridge::new(true);
    saturated.claim_driver("saturated-owner").unwrap();
    saturated
        .start_routing("saturated-owner", routing_policy(), false)
        .unwrap();
    let model_ids = (0..8)
        .map(|index| format!("model-{index}"))
        .collect::<Vec<_>>();
    for id in &model_ids {
        saturated.ingest(routed_event(id, DanmuEventKind::Danmu, "13", id));
    }
    saturated
        .consume_routing_models("saturated-owner", &model_ids)
        .unwrap();
    saturated.ingest(routed_event(
        "saturated-follow",
        DanmuEventKind::Follow,
        "14",
        "关注了主播",
    ));
    for index in 0..600 {
        saturated.ingest(routed_event(
            &format!("saturated-later-{index}"),
            DanmuEventKind::Danmu,
            "15",
            "后续消息",
        ));
    }

    std::thread::sleep(std::time::Duration::from_millis(5_050));

    assert!(
        !automatic
            .submit_routing_template("automatic-owner", true)
            .unwrap()
    );
    assert!(!automatic.sending_enabled());
    automatic.permission(true).unwrap();
    assert!(
        automatic
            .submit_routing_template("automatic-owner", true)
            .unwrap()
    );
    let automatic_job = automatic.take_ready().unwrap();
    assert!(!automatic_job.reply.approval_required);
    assert!(
        automatic_job.reply.reply_to
            == Some(ReplyTarget {
                user_id: "12".into(),
                username: "user-12".into(),
            })
    );

    assert!(
        saturated
            .submit_routing_template("saturated-owner", false)
            .unwrap()
    );
    assert_eq!(
        saturated.routing_snapshot()["leased"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(saturated.candidates().len(), 1);

    assert!(
        review
            .submit_routing_template("review-owner", false)
            .unwrap()
    );
    assert!(!review.sending_enabled());
    assert_eq!(review.routing_backlog(), 1);
    assert!(
        !review
            .submit_routing_template("review-owner", false)
            .unwrap()
    );
    let candidate = review.candidates().pop().unwrap();
    assert!(candidate.approval_required);
    assert!(
        candidate.reply_to
            == Some(ReplyTarget {
                user_id: "11".into(),
                username: "user-11".into(),
            })
    );

    review
        .approve_candidates(std::slice::from_ref(&candidate))
        .unwrap();
    let reviewed_job = review.take_ready().unwrap();
    assert!(
        fallback
            .submit_routing_template("fallback-owner", false)
            .unwrap()
    );
    let fallback_candidate = fallback.candidates().pop().unwrap();
    assert!(fallback_candidate.reply_to.is_none());
    assert!(fallback_candidate.text.starts_with("@昵称甲 "));
    fallback
        .approve_candidates(std::slice::from_ref(&fallback_candidate))
        .unwrap();
    let fallback_job = fallback.take_ready().unwrap();

    assert!(
        !anonymous
            .submit_routing_template("anonymous-owner", false)
            .unwrap()
    );
    assert!(anonymous.candidates().is_empty());

    for (bridge, driver, job) in [
        (&automatic, "automatic-owner", automatic_job),
        (&review, "review-owner", reviewed_job),
        (&fallback, "fallback-owner", fallback_job),
    ] {
        let mut rejected = Delivery::new(
            Outcome::Rejected,
            crate::delivery::Cause::ContentRejected,
            "模板被内容审核拒绝",
        );
        rejected.unconfirmed_segments = vec![job.reply.text.clone()];
        bridge.complete(&job.reply, rejected);
        assert!(bridge.take_repair(driver).is_none());
    }
}

#[test]
fn routing_ownership_scene_and_account_guards_invalidate_private_state() {
    let b = Bridge::new(true);
    b.claim_driver("owner").unwrap();
    b.start_routing("owner", routing_policy(), false).unwrap();
    b.ingest(routed_event("foreign", DanmuEventKind::Danmu, "42", "问题"));
    b.ingest(routed_event(
        "self",
        DanmuEventKind::Danmu,
        "self-account",
        "自己的消息",
    ));
    assert_eq!(b.routing_models("owner").unwrap().len(), 1);
    assert!(b.routing_models("outsider").is_err());
    assert!(b.update_routing("outsider", routing_policy()).is_err());
    let before = b.routing_snapshot();
    b.stop_routing("outsider");
    assert_eq!(b.routing_snapshot(), before);

    b.consume_routing_models("owner", &["foreign".into()])
        .unwrap();
    b.release_driver("owner");
    assert_eq!(b.routing_snapshot()["active"], false);
    assert_eq!(b.routing_snapshot()["leased"], json!([]));
    assert!(b.start_routing("owner", routing_policy(), true).is_err());

    b.claim_driver("next-owner").unwrap();
    b.start_routing("next-owner", routing_policy(), true)
        .unwrap();
    assert_eq!(b.routing_models("next-owner").unwrap().len(), 1);
    b.end();
    assert_eq!(b.routing_snapshot()["active"], false);
    assert!(b.routing_models("next-owner").is_err());
}
