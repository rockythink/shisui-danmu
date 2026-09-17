use super::*;
use crate::obs::ObsConfiguration;
use std::path::Path;
/// All account/OBS clients point at a fresh private temporary directory; none are run.
pub(super) fn app(root: &Path, session: DanmuSession) -> TerminalApp {
    let themes = crate::theme::ThemeCatalog::load(root.join("themes.json")).unwrap();
    let theme_name = themes.selected().to_owned();
    let account = AccountClient::new(root.join("account.json")).unwrap();
    let mut runner = crate::runner::Runner::load(&root.join("config.toml"));
    runner.settings.workspace = Some(root.join("workspace"));
    let journal = SessionJournal::new(root.join("sessions"));
    journal.start(&session).unwrap();
    runner.import_history(&session.room_id, &journal).unwrap();
    TerminalApp {
        config: TerminalConfig {
            config_path: root.join("config.toml"),
            instance: None,
            history_idle_seconds: 0,
            room_id: "1".into(),
            single_line: true,
            chat_layout: false,
            show_time: false,
            show_name: true,
            palette: Palette::default(),
            theme_name,
            themes,
        },
        bridge: Bridge::new(true),
        local_transport: None,
        live_scope: None,
        candidate_selected: None,
        candidate_edit: None,
        review_frame: None,
        runner,
        assistant_panel: None,
        settings_operation: None,
        profile: agent::ProfileState::default(),
        client: BilibiliClient::new(root.join("account.json")).unwrap(),
        assistant_accounts: accounts::AssistantAccounts::load(&account, &root.join("config.toml"))
            .unwrap(),
        account,
        manual_send_queue: SendQueue::default(),
        independent_send_queue: SendQueue::default(),
        obs: ObsController::new(ObsConfiguration::default(), root.join("obs.json")),
        journal,
        session,
        room: None,
        room_updated_at: None,
        connection: "已连接 1".into(),
        watched: None,
        likes: None,
        online_viewers: None,
        input: EditorInput::default(),
        slash_selection: 0,
        command_search_draft: None,
        secret_draft: None,
        help: None,
        selected: 0,
        scroll_offset: 0,
        activity: activity::ActivityNotices::default(),
        last_user_activity: Instant::now(),
        notice: String::new(),
        notice_deadline: None,
        notice_is_delivery: false,
        notice_level: NoticeLevel::Info,
        delivery_status: DeliveryStatus::Idle,
        delivery_status_deadline: None,
        layout_chat: false,
        show_name: true,
        show_time: false,
        account_status: AccountStatus::SignedOut,
        stop_flow: None,
        login_qr: None,
        main_login: None,
        secret_mode: false,
        selection_active: false,
        quit_requested: false,
        unread_live_count: 0,
        obs_status: None,
        microphone_level: None,
        obs_error: None,
        obs_checked_at: None,
        pending_deliveries: VecDeque::new(),
        confirmed_deliveries: VecDeque::new(),
        last_realtime_at: None,
        last_live_danmu_at: None,
        animation_tick: 0,
        resume_pending: false,
    }
}
pub async fn run(path: &Path) -> Result<()> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 1024 * 1024,
        "回放最大1MiB"
    );
    let events: Vec<DanmuEvent> = serde_json::from_slice(&std::fs::read(path)?)?;
    let root = tempfile::tempdir()?;
    let mut app = app(root.path(), DanmuSession::new("local"));
    for event in events {
        app.ingest_event(event);
    }
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.draw(|frame| draw(frame, &mut app))?;
    println!("{:?}", terminal.backend().buffer());
    Ok(())
}
