use super::*;
use crate::obs::ObsConfiguration;
use ratatui::backend::TestBackend;
use serde::Deserialize;
use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    SettingsCommand {
        value: String,
    },
    Event {
        text: String,
        author: String,
        name: String,
        id: String,
        #[serde(default)]
        history: bool,
    },
    Mode {
        value: String,
    },
    Draft {
        text: String,
    },
    Popup {
        open: bool,
    },
    Live {
        value: bool,
    },
    Session {
        id: String,
    },
    Wait {
        millis: u64,
    },
    Approve,
    Discard,
    Transport {
        outcome: String,
    },
    Assert {
        state: String,
    },
}
struct LocalTransport {
    outcome: Outcome,
    sent: AtomicUsize,
}
impl Transport for LocalTransport {
    async fn send_confirm(&self, text: &str) -> Outcome {
        self.sent.fetch_add(1, Ordering::Relaxed);
        println!(
            "LOCAL_TRANSPORT {} {text}",
            if self.outcome == Outcome::Confirmed {
                "accepted+echo"
            } else {
                "no-confirmation"
            }
        );
        self.outcome
    }
}

/// All account/OBS clients point at a fresh private temporary directory; none are run.
pub(super) fn app(root: &Path, session: DanmuSession) -> TerminalApp {
    let themes = crate::theme::ThemeCatalog::load(root.join("themes.json")).unwrap();
    let theme_name = themes.selected().to_owned();
    TerminalApp {
        config: TerminalConfig {
            config_path: root.join("config.toml"),
            autoreply: autoreply::Config::default(),
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
        autoreply: None,
        ai_settings: ai_settings::Settings::default(),
        reply_panel: false,
        client: BilibiliClient::new(root.join("account.json")).unwrap(),
        account: AccountClient::new(root.join("account.json")).unwrap(),
        send_queue: SendQueue::default(),
        obs: ObsController::new(ObsConfiguration::default(), root.join("obs.json")),
        journal: SessionJournal::new(root.join("sessions")),
        session,
        room: None,
        room_updated_at: None,
        connection: "已连接 1".into(),
        watched: None,
        likes: None,
        online_viewers: None,
        input: EditorInput::default(),
        slash_selection: 0,
        selected: 0,
        scroll_offset: 0,
        activity: activity::ActivityNotices::default(),
        last_user_activity: Instant::now(),
        notice: String::new(),
        notice_deadline: None,
        delivery_status: DeliveryStatus::Idle,
        delivery_status_deadline: None,
        layout_chat: false,
        show_name: true,
        show_time: false,
        account_status: AccountStatus::SignedOut,
        stop_flow: None,
        login_qr: None,
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
        live_danmu_count: 0,
        last_live_danmu_at: None,
        animation_tick: 0,
    }
}
pub async fn run(path: &Path) -> Result<()> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 1024 * 1024,
        "回放文件最大1MiB"
    );
    let steps: Vec<Step> = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(steps.len() <= 1000, "回放最多1000步");
    let temp = tempfile::tempdir()?;
    let mut app = app(temp.path(), DanmuSession::new("1"));
    app.reply_panel = true;
    app.config.autoreply.enabled = true;
    app.config.autoreply.global_interval_seconds = 1;
    app.config.autoreply.person_interval_seconds = 1;
    app.autoreply = Some(autoreply::Handle::start(
        app.config.autoreply.clone(),
        app.journal.clone(),
        app.session.id.clone(),
    ));
    let mut gate = autoreply::Gate {
        session: "local-replay".into(),
        live: true,
        busy: false,
        broadcaster: "host".into(),
    };
    app.autoreply.as_mut().unwrap().set_gate(gate.clone());
    let mut transport = LocalTransport {
        outcome: Outcome::Confirmed,
        sent: AtomicUsize::new(0),
    };
    let (tx, mut rx) = mpsc::channel(32);
    tokio::time::sleep(Duration::from_millis(100)).await;
    for step in steps {
        match step {
            Step::SettingsCommand { value } => {
                anyhow::ensure!(
                    [
                        "settings",
                        "disable",
                        "provider api",
                        "provider chatgpt",
                        "suggest",
                        "approve-mode",
                        "auto"
                    ]
                    .contains(&value.as_str()),
                    "回放不允许登录/网络/生成命令"
                );
                app.command(&format!("/ai {value}"), tx.clone()).await?;
            }
            Step::Event {
                text,
                author,
                name,
                id,
                history,
            } => {
                let mut event = DanmuEvent::new(DanmuEventKind::Danmu, text);
                event.author_id = Some(author);
                event.username = Some(name);
                event.platform_event_id = Some(id);
                if history {
                    event.origin = DanmuEventOrigin::History;
                }
                app.ingest_event(event);
            }
            Step::Mode { value } => {
                anyhow::ensure!(
                    ["suggest", "approve-mode", "auto", "pause", "resume-send"]
                        .contains(&value.as_str()),
                    "不允许的回放命令"
                );
                app.command(&format!("/ai {value}"), tx.clone()).await?;
            }
            Step::Draft { text } => {
                app.input.replace(text);
                gate.busy = !app.input.is_empty() || app.login_qr.is_some();
            }
            Step::Popup { open } => {
                app.login_qr = open.then(Vec::new);
                gate.busy = open || !app.input.is_empty();
            }
            Step::Live { value } => gate.live = value,
            Step::Session { id } => gate.session = id,
            Step::Wait { millis } => {
                anyhow::ensure!(millis <= 5000, "单步等待最多5秒");
                tokio::time::sleep(Duration::from_millis(millis)).await;
            }
            Step::Approve => {
                app.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE), tx.clone())
                    .await?;
            }
            Step::Discard => {
                app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE), tx.clone())
                    .await?;
            }
            Step::Transport { outcome } => {
                transport.outcome = match outcome.as_str() {
                    "confirmed" => Outcome::Confirmed,
                    "uncertain" => Outcome::Uncertain,
                    "rejected" => Outcome::Rejected,
                    _ => anyhow::bail!("未知本地传输结果"),
                }
            }
            Step::Assert { state } => {
                let view = app.autoreply.as_ref().unwrap().view.borrow();
                match state.as_str() {
                    "disabled" => anyhow::ensure!(!view.enabled, "期望总开关关闭"),
                    "settings_open" => anyhow::ensure!(app.ai_settings.open, "期望设置界面"),
                    "candidate" => {
                        anyhow::ensure!(view.candidate.is_some(), "期望候选：{}", view.reason)
                    }
                    "no_candidate" => anyhow::ensure!(view.candidate.is_none(), "不应有候选"),
                    "zero_calls" => anyhow::ensure!(view.usage.requests == 0, "无事件应零调用"),
                    "paused" => anyhow::ensure!(view.mode == ReplyMode::Paused, "期望暂停"),
                    "sent" => {
                        anyhow::ensure!(view.reason.contains("Sent"), "期望已发：{}", view.reason)
                    }
                    _ => anyhow::bail!("未知断言"),
                }
                println!("PASS {state}");
            }
        }
        app.autoreply.as_mut().unwrap().set_gate(gate.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
        while let Ok(candidate) = app.autoreply.as_mut().unwrap().ready.try_recv() {
            let outcome = app
                .send_queue
                .send(&transport, &candidate.segments, Some(&candidate.permit))
                .await;
            let state = match outcome {
                Outcome::Confirmed => ReplyState::Sent,
                Outcome::Cancelled => ReplyState::Cancelled,
                _ => ReplyState::Uncertain,
            };
            app.autoreply
                .as_mut()
                .unwrap()
                .delivery(candidate.key, state);
        }
        while let Ok(event) = rx.try_recv() {
            app.handle_ui_event(event);
        }
    }
    let mut terminal = Terminal::new(TestBackend::new(120, 26))?;
    terminal.draw(|frame| draw(frame, &mut app))?;
    for y in 0..26 {
        let mut row = String::new();
        let mut x = 0;
        while x < 120 {
            let symbol = terminal.backend().buffer()[(x, y)].symbol();
            row.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        println!("{}", row.trim_end());
    }
    println!(
        "REPLAY ONLY: no account/OBS/provider/network calls; local segments={}",
        transport.sent.load(Ordering::Relaxed)
    );
    Ok(())
}
