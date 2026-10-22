// SPDX-License-Identifier: MPL-2.0
use super::*;
use shisui_danmu::{domain, runtime as rt};

#[test]
fn nullable_identity_and_pre_epoch_timestamp_remain_exact() {
    let event = domain::DanmuEvent {
        id: "fixture-event".into(),
        kind: domain::DanmuEventKind::Superchat,
        timestamp: chrono::DateTime::from_timestamp_millis(-1234).unwrap(),
        username: None,
        author_id: None,
        content: "中文与👩🏽‍💻".into(),
        reply_to: None,
        origin: domain::DanmuEventOrigin::Archived,
        platform_event_id: None,
        emotes: Vec::new(),
        gift: None,
    };
    let result = EventRecord::from(rt::EventRecord {
        message_key: u64::MAX,
        event,
        assistant_state: None,
    });
    assert_eq!(result.message_key, u64::MAX);
    assert_eq!(result.event.timestamp, -1234);
    assert!(result.event.username.is_none());
    assert!(result.event.author_id.is_none());
    assert!(result.event.reply_to.is_none());
    assert!(result.event.platform_event_id.is_none());
    assert!(result.event.gift.is_none());
    assert_eq!(result.event.origin, EventOrigin::Archived);
}

#[test]
fn all_format_four_settings_survive_roundtrip_without_credential_fields() {
    let settings = rt::settings::AssistantSettings {
        format: 4,
        api: rt::settings::ApiSettings {
            protocol: rt::settings::ApiProtocol::AnthropicMessages,
            base_url: "http://127.0.0.1:12345/v1".into(),
            model: "fixture-model".into(),
            max_output_tokens: 4321,
            openai_token_limit: rt::settings::OpenaiTokenLimit::MaxTokens,
            chatgpt_account: Some("fixture-account".into()),
        },
        name: "合成助手".into(),
        preferences: "仅合成配置".into(),
        use_profile: false,
        use_project: true,
        workspace: Some(std::path::PathBuf::from("/isolated/assistant-workspace")),
        automatic: true,
        resume_on_start: true,
        reply_activity: rt::settings::ReplyActivity::Active,
        dynamic_host_priority: false,
        dynamic_muted_support: true,
        thank_gifts: true,
        thank_likes: false,
        thank_follows: true,
        thank_shares: false,
        mention_sender: true,
        repair_blocked: false,
        web_search: true,
        blocked_words: vec!["合成屏蔽词".into()],
        skills: vec!["fixture-skill".into()],
        persona: rt::settings::Persona::Broadcaster,
    };
    let projected = DesktopAssistantSettings::try_from(settings.clone()).unwrap();
    let restored = rt::settings::AssistantSettings::try_from(projected).unwrap();
    assert_eq!(restored, settings);
}

#[test]
fn reviewed_candidate_preserves_the_frozen_identity_and_revision() {
    let expected = rt::ReviewedCandidate {
        session: "session".into(),
        caller: "caller".into(),
        request_id: "request".into(),
        revision: u64::MAX - 1,
        identity_generation: u64::MAX,
    };
    let restored: rt::ReviewedCandidate = ReviewedCandidate::from(expected.clone()).into();
    assert_eq!(restored, expected);
}

#[test]
fn resync_keeps_watermark_without_replaying_snapshot_or_notices() {
    let batch = DesktopBatch::from(rt::RuntimeBatch {
        sequence: 2049,
        new_message_count: 312,
        snapshot: None,
        changes: vec![rt::RuntimeChange::ResyncRequired],
    });
    assert_eq!(batch.sequence, 2049);
    assert_eq!(batch.new_message_count, 312);
    assert!(batch.snapshot.is_none());
    assert!(matches!(
        batch.changes.as_slice(),
        [DesktopChange::ResyncRequired]
    ));
}

#[test]
fn boundary_rejects_invalid_tokens_times_and_relative_paths() {
    assert!(matches!(
        parse_uuid("not-an-operation"),
        Err(CoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        parse_time(i64::MAX),
        Err(CoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        absolute_path("relative/path".into(), "fixture"),
        Err(CoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        absolute_path("/isolated/../other".into(), "fixture"),
        Err(CoreError::InvalidInput { .. })
    ));
    assert_eq!(parse_time(-1234).unwrap().timestamp_millis(), -1234);
}

#[test]
fn error_mapping_never_formats_a_private_io_cause_or_chain() {
    let error = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "fixture-private-secret",
    ))
    .context("fixture-private-context");
    let error = CoreError::from_runtime(error);
    assert!(matches!(error, CoreError::Storage { .. }));
    let message = error.to_string();
    assert!(!message.contains("fixture-private-secret"));
    assert!(!message.contains("fixture-private-context"));
    assert!(message.contains("PermissionDenied"));
}

#[test]
fn failed_operation_is_not_projected_as_an_accepted_or_completed_receipt() {
    let result = DesktopCommandResult::from(rt::CommandResult {
        operation_id: uuid::Uuid::nil(),
        state: rt::OperationState::Failed {
            reason: "合成写盘失败".into(),
        },
        value: rt::OperationValue::None,
        effects: Vec::new(),
    });
    assert!(
        matches!(result.state, DesktopOperationState::Failed { reason } if reason == "合成写盘失败")
    );
    assert!(matches!(result.value, DesktopOperationValue::None));
    assert!(result.effects.is_empty());
}

#[test]
fn unexpected_foreign_callback_error_is_storage_not_panic_or_secret_text() {
    let error = CoreError::from(uniffi::UnexpectedUniFFICallbackError::new("fixture-secret"));
    assert!(matches!(error, CoreError::Storage { .. }));
    assert!(!error.to_string().contains("fixture-secret"));
}

#[test]
fn explicit_runtime_categories_survive_anyhow_context_without_text_guessing() {
    use rt::error::{RuntimeFailure, RuntimeFailureKind};
    let cases = [
        (
            RuntimeFailureKind::InvalidInput,
            CoreError::InvalidInput {
                message: "safe".into(),
            },
        ),
        (
            RuntimeFailureKind::Busy,
            CoreError::Busy {
                message: "safe".into(),
            },
        ),
        (
            RuntimeFailureKind::Stale,
            CoreError::Stale {
                message: "safe".into(),
            },
        ),
        (
            RuntimeFailureKind::PermissionDenied,
            CoreError::PermissionDenied {
                message: "safe".into(),
            },
        ),
        (
            RuntimeFailureKind::Storage,
            CoreError::Storage {
                message: "safe".into(),
            },
        ),
        (
            RuntimeFailureKind::Transport,
            CoreError::Transport {
                message: "safe".into(),
            },
        ),
        (
            RuntimeFailureKind::Protocol,
            CoreError::Protocol {
                message: "safe".into(),
            },
        ),
        (
            RuntimeFailureKind::Closed,
            CoreError::Closed {
                message: "safe".into(),
            },
        ),
    ];
    for (kind, expected) in cases {
        let cause = RuntimeFailure::new(kind, "safe");
        let error = anyhow::Error::new(cause).context("private outer detail");
        assert_eq!(CoreError::from_runtime(error), expected);
    }
}

#[tokio::test]
async fn exclusive_runtime_returns_busy_and_shutdown_releases_it_before_object_drop() {
    struct DenyPasswordAccess;
    impl DesktopObsPasswordStore for DenyPasswordAccess {
        fn read(&self, _: String, _: u16) -> Result<Option<String>, CoreError> {
            Err(CoreError::PermissionDenied {
                message: "open must not access credentials".into(),
            })
        }
        fn write(&self, _: String, _: u16, _: String) -> Result<(), CoreError> {
            Err(CoreError::PermissionDenied {
                message: "open must not write credentials".into(),
            })
        }
    }
    struct OwnedRoot(PathBuf);
    impl Drop for OwnedRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root =
        OwnedRoot(std::env::temp_dir().join(format!("danmu-ffi-lock-{}", uuid::Uuid::new_v4())));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root.0).unwrap();
    let options = || DesktopOptions {
        support_root: root.0.to_str().unwrap().into(),
        config_path: root.0.join("config.toml").to_str().unwrap().into(),
        account_path: root.0.join("account.json").to_str().unwrap().into(),
        obs_config_path: root.0.join("obs.json").to_str().unwrap().into(),
        sessions_root: root.0.join("sessions").to_str().unwrap().into(),
        workspace_default: root.0.join("workspace").to_str().unwrap().into(),
        external_instance: None,
        mode: DesktopMode::Local,
    };
    let first = DesktopRuntime::open(options(), Arc::new(DenyPasswordAccess))
        .await
        .unwrap();
    let duplicate = DesktopRuntime::open(options(), Arc::new(DenyPasswordAccess)).await;
    assert!(matches!(duplicate, Err(CoreError::Busy { .. })));
    first.shutdown().await.unwrap();
    first.shutdown().await.unwrap();
    let next = DesktopRuntime::open(options(), Arc::new(DenyPasswordAccess))
        .await
        .unwrap();
    next.shutdown().await.unwrap();
}
