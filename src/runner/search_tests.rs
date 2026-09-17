use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

// Keep these Python peers from exhausting the production test-only RPC deadline.
static SEARCH_PEERS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn peer(host: Host, enabled: bool, scenario: &str) -> (tempfile::TempDir, Handle) {
    let fixture = tempfile::tempdir().unwrap();
    let binary = fixture.path().join("search-peer.py");
    std::fs::write(&binary, include_str!("../../tests/fixtures/acp_search.py")).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        fixture.path().join("search-fixture.json"),
        json!({"host": if host == Host::Omp { "omp" } else { "gemini" }, "scenario": scenario})
            .to_string(),
    )
    .unwrap();
    let settings = Settings {
        host,
        binary,
        web_search: enabled,
        ..Settings::default()
    };
    let mut handle = Handle::spawn(
        settings,
        Workspace::isolated(Arc::new(tempfile::tempdir().unwrap())),
        None,
    );
    match next(&mut handle).await {
        Event::Ready => {}
        Event::Closed(result) => panic!("搜索fixture未就绪：{result:?}"),
        _ => panic!("搜索fixture意外启动事件"),
    }
    (fixture, handle)
}

async fn next(handle: &mut Handle) -> Event {
    tokio::time::timeout(Duration::from_secs(4), handle.events.recv())
        .await
        .unwrap()
        .expect("搜索fixture事件流提前关闭")
}

async fn ask(handle: &mut Handle, round: &str) -> Result<Vec<Candidate>, String> {
    handle
        .actions
        .send(Action::Prompt(Prompt {
            round_id: round.into(),
            text: json!({"round_id": round}).to_string(),
            message_ids: vec!["viewer".into()].into(),
            cancel_epoch: handle.epoch(),
        }))
        .await
        .unwrap();
    loop {
        match next(handle).await {
            Event::Started { .. } => {}
            Event::Answer { result, .. } => {
                return result.and_then(|answer| match answer {
                    Answer::Candidates(candidates) => Ok(candidates),
                    Answer::NoMessage => Ok(Vec::new()),
                    Answer::Rejected(reason) => Err(reason),
                });
            }
            Event::Closed(result) => panic!("搜索fixture没有返回轮次结果：{result:?}"),
            _ => panic!("搜索fixture意外轮次事件"),
        }
    }
}

async fn finish(mut handle: Handle) {
    handle.stop();
    tokio::time::timeout(Duration::from_secs(4), &mut handle.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn search_enabled_only_admits_host_search_and_discards_pre_tool_commentary() {
    let _peer = SEARCH_PEERS.lock().await;
    for host in [Host::Omp, Host::Gemini] {
        let (_fixture, mut handle) = peer(host, true, "normal").await;
        for round in ["first", "second"] {
            let answer = ask(&mut handle, round).await.unwrap();
            assert_eq!(answer.len(), 1);
            assert_eq!(answer[0].message_id, "viewer");
            assert_eq!(answer[0].text, "暂无可核实的实时资料");
        }
        finish(handle).await;
    }
}

#[tokio::test]
async fn disabled_search_rejects_even_the_known_native_search_shape() {
    let _peer = SEARCH_PEERS.lock().await;
    for host in [Host::Omp, Host::Gemini] {
        let (_fixture, mut handle) = peer(host, false, "normal").await;
        assert!(ask(&mut handle, "disabled").await.is_err());
        finish(handle).await;
    }
}

#[tokio::test]
async fn search_does_not_admit_other_tools_or_kind_only_impersonation() {
    let _peer = SEARCH_PEERS.lock().await;
    for host in [Host::Omp, Host::Gemini] {
        for scenario in ["other_tool", "spoof_kind", "file_location", "file_result"] {
            let (_fixture, mut handle) = peer(host, true, scenario).await;
            assert!(
                ask(&mut handle, scenario).await.is_err(),
                "{host:?}/{scenario}"
            );
            finish(handle).await;
        }
    }
}

#[tokio::test]
async fn search_updates_require_current_admission_identity_and_live_status() {
    let _peer = SEARCH_PEERS.lock().await;
    for scenario in [
        "unknown_update",
        "identity_change",
        "input_change",
        "repeated_terminal",
    ] {
        let (_fixture, mut handle) = peer(Host::Omp, true, scenario).await;
        assert!(ask(&mut handle, scenario).await.is_err(), "{scenario}");
        finish(handle).await;
    }
}

#[tokio::test]
async fn prior_round_search_ids_cannot_be_reused() {
    let _peer = SEARCH_PEERS.lock().await;
    let (_fixture, mut handle) = peer(Host::Omp, true, "cross_round").await;
    assert_eq!(
        ask(&mut handle, "first").await.unwrap()[0].message_id,
        "viewer"
    );
    assert!(ask(&mut handle, "second").await.is_err());
    finish(handle).await;
}

#[tokio::test]
async fn search_cannot_finish_a_round_or_emit_candidate_before_tool_completion() {
    let _peer = SEARCH_PEERS.lock().await;
    for scenario in ["unfinished", "premature_text"] {
        let (_fixture, mut handle) = peer(Host::Omp, true, scenario).await;
        assert!(ask(&mut handle, scenario).await.is_err(), "{scenario}");
        finish(handle).await;
    }
}

#[tokio::test]
async fn only_search_boundaries_allow_new_assistant_message_and_never_replay_old_ids() {
    let _peer = SEARCH_PEERS.lock().await;
    for scenario in ["plain_messages", "reused_message"] {
        let (_fixture, mut handle) = peer(Host::Omp, true, scenario).await;
        assert!(ask(&mut handle, scenario).await.is_err(), "{scenario}");
        finish(handle).await;
    }
}

#[tokio::test]
async fn search_errors_can_return_an_honest_unavailable_answer_without_claiming_evidence() {
    let _peer = SEARCH_PEERS.lock().await;
    for scenario in ["failed", "completed_error"] {
        let (_fixture, mut handle) = peer(Host::Omp, true, scenario).await;
        assert_eq!(
            ask(&mut handle, scenario).await.unwrap()[0].text,
            "暂无可核实的实时资料"
        );
        finish(handle).await;
    }
}

#[tokio::test]
async fn search_permission_requests_still_cancel_instead_of_escalating() {
    let _peer = SEARCH_PEERS.lock().await;
    let (fixture, mut handle) = peer(Host::Gemini, true, "permission").await;
    assert!(ask(&mut handle, "permission").await.is_err());
    finish(handle).await;
    assert_eq!(
        std::fs::read_to_string(fixture.path().join("permission-outcome")).unwrap(),
        "cancelled"
    );
}

#[test]
fn managed_gemini_policy_blocks_private_override_without_touching_it() {
    let root = tempfile::tempdir().unwrap();
    hosts::check_gemini_system_policy(root.path()).unwrap();
    let policies = root.path().join("policies");
    std::fs::create_dir(&policies).unwrap();
    let managed = policies.join("managed.toml");
    let original = "[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 999\n";
    std::fs::write(&managed, original).unwrap();
    assert!(hosts::check_gemini_system_policy(root.path()).is_err());
    assert_eq!(std::fs::read_to_string(managed).unwrap(), original);
    let settings_root = tempfile::tempdir().unwrap();
    std::fs::write(settings_root.path().join("settings.json"), "{}").unwrap();
    assert!(hosts::check_gemini_system_policy(settings_root.path()).is_err());
}

#[test]
fn search_launch_permission_cannot_change_in_an_existing_workspace_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings {
        host: Host::Omp,
        binary: "/bin/echo".into(),
        ..Settings::default()
    };
    hosts::command(&settings, root.path()).unwrap();
    let changed = Settings {
        web_search: true,
        ..settings
    };
    assert!(hosts::command(&changed, root.path()).is_err());
}
