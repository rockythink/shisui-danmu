use super::*;
use std::path::Path;
pub(crate) fn paths(root: &Path) -> crate::storage::StoragePaths {
    crate::storage::StoragePaths {
        support_dir: root.to_path_buf(),
        config_file: root.join("config.toml"),
        account_session: root.join("account.json"),
        obs_configuration: root.join("obs.json"),
        sessions_dir: root.join("sessions"),
        themes_file: root.join("themes.json"),
    }
}
#[cfg(test)]
pub(crate) fn app(root: &Path, session: DanmuSession) -> RuntimeState {
    let paths = paths(root);
    let mut state = RuntimeState::prepare(crate::runtime::RuntimeOptions {
        obs_passwords: std::sync::Arc::new(crate::obs::FileObsPasswordStore::new(
            paths.obs_configuration.clone(),
            false,
        )),
        storage: paths,
        workspace_default: root.join("workspace"),
        mode: crate::runtime::RuntimeMode::Local,
        external_instance: None,
    })
    .unwrap();
    state.session = session;
    state.obs_allowed = true;
    state.journal.start(&state.session).unwrap();
    state
        .runner
        .import_history(&state.session.room_id, &state.journal)
        .unwrap();
    state.connection = "已连接 1".into();
    state
}
pub async fn run(path: &Path) -> Result<()> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 1024 * 1024,
        "回放最大1MiB"
    );
    let events: Vec<DanmuEvent> = serde_json::from_slice(&std::fs::read(path)?)?;
    let root = tempfile::tempdir()?;
    let paths = paths(root.path());
    let handle = crate::runtime::open(crate::runtime::RuntimeOptions {
        obs_passwords: std::sync::Arc::new(crate::obs::FileObsPasswordStore::new(
            paths.obs_configuration.clone(),
            false,
        )),
        storage: paths,
        workspace_default: root.path().join("workspace"),
        mode: crate::runtime::RuntimeMode::Replay,
        external_instance: None,
    })
    .await?;
    let rendered = handle
        .presentation(move |state| {
            Box::pin(async move {
                for event in events {
                    state.ingest_event(event);
                }
                let backend = ratatui::backend::TestBackend::new(100, 30);
                let mut terminal = ratatui::Terminal::new(backend)?;
                terminal.draw(|frame| draw(frame, state))?;
                Ok::<_, anyhow::Error>(format!("{:?}", terminal.backend().buffer()))
            })
        })
        .await?;
    let shutdown = handle.shutdown().await;
    println!("{}", rendered?);
    shutdown
}
