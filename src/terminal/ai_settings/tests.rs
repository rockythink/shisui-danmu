use super::*;
use clap::Parser;

#[tokio::test]
async fn settings_restore_choices_preserve_unrelated_toml_and_reject_unsupported_effort() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        "room_id = '1'\nshow_name = false\n[unrelated]\nvalue = 'keep'\n",
    )
    .unwrap();
    let mut app = replay::app(temp.path(), DanmuSession::new("1"));
    let (tx, _) = mpsc::channel(8);
    app.ai_settings.account.signed_in = true;
    app.ai_settings.models = serde_json::from_value(serde_json::json!([
        {"model":"one","displayName":"One","inputModalities":["text"],"supportedReasoningEfforts":[{"reasoningEffort":"minimal","description":"minimal"}],"defaultReasoningEffort":"minimal"},
        {"model":"two","displayName":"Two","inputModalities":["text"],"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"medium"}],"defaultReasoningEffort":"medium"}
    ])).unwrap();
    app.ai_command(&["model", "one"], tx.clone()).await.unwrap();
    app.ai_command(&["enable"], tx.clone()).await.unwrap();
    assert_eq!(app.config.autoreply.mode, ReplyMode::Suggest);
    assert!(
        app.ai_command(&["effort", "high"], tx.clone())
            .await
            .is_err()
    );
    assert_eq!(
        app.config.autoreply.codex.effort.as_deref(),
        Some("minimal")
    );
    app.ai_command(&["model", "two"], tx.clone()).await.unwrap();
    assert_eq!(app.config.autoreply.codex.effort.as_deref(), Some("medium"));
    app.ai_command(&["disable"], tx.clone()).await.unwrap();
    let cli = crate::config::Cli::try_parse_from(["danmu"]).unwrap();
    let config = TerminalConfig::load(
        &cli,
        temp.path().join("config.toml"),
        temp.path().join("themes.json"),
    )
    .unwrap();
    assert!(!config.autoreply.enabled);
    assert_eq!(config.autoreply.codex.model.as_deref(), Some("two"));
    assert_eq!(config.autoreply.codex.effort.as_deref(), Some("medium"));
    assert!(!config.show_name);
    let saved: toml::Value =
        toml::from_str(&std::fs::read_to_string(temp.path().join("config.toml")).unwrap()).unwrap();
    assert_eq!(saved["unrelated"]["value"].as_str(), Some("keep"));
    app.command("/ai effort invalid", tx).await.unwrap();
    assert_eq!(app.config.autoreply.codex.effort.as_deref(), Some("medium"));
}

#[tokio::test]
async fn safety_pause_persists_without_overwriting_a_new_explicit_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = replay::app(temp.path(), DanmuSession::new("1"));
    app.autoreply = Some(autoreply::Handle::start(
        app.config.autoreply.clone(),
        app.journal.clone(),
        app.session.id.clone(),
    ));
    app.autoreply.as_mut().unwrap().mode(ReplyMode::Paused);
    tokio::time::sleep(Duration::from_millis(50)).await;
    app.persist_ai_safety_stop().await;
    assert_eq!(app.config.autoreply.mode, ReplyMode::Paused);
    let (tx, _) = mpsc::channel(8);
    app.ai_command(&["suggest"], tx).await.unwrap();
    app.persist_ai_safety_stop().await;
    assert_eq!(app.config.autoreply.mode, ReplyMode::Suggest);
}
