mod input;
mod meter;
mod qr;

pub use qr::qr_lines;

use crate::{
    bilibili::{
        AccountClient, AccountStatus, BilibiliClient, BilibiliClientEvent, LoginPoll,
        RoomLiveStatus, RoomSnapshot, cross_origin_duplicate, kind_label, masked_name_matches,
        normalized_message, segment_message, usable_author_id,
    },
    config::TerminalConfig,
    domain::{DanmuEvent, DanmuEventKind, DanmuEventOrigin, DanmuSession, DanmuSessionEndReason},
    obs::{MicrophoneLevel, MicrophoneState, ObsController, ObsStatus},
    persistence::SessionJournal,
    theme::Palette,
};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, Utc};
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use input::EditorInput;
use meter::microphone_meter;
use qr::{compact_qr_lines, draw_qr};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use std::{
    collections::VecDeque,
    io::{self, Stdout},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
const SEND_QUEUE_INTERVAL: Duration = Duration::from_secs(2);
const DELIVERY_NOTICE_LIFETIME: Duration = Duration::from_secs(15);

#[derive(Debug)]
enum UiEvent {
    Notice {
        message: String,
        level: NoticeLevel,
    },
    LoginQr(Vec<String>),
    LoginDone(AccountStatus),
    ObsStatus(std::result::Result<ObsStatus, String>),
    RoomSnapshot(RoomSnapshot),
    OnlineViewers(std::result::Result<Option<u64>, String>),
    Likes(std::result::Result<Option<u64>, String>),
    DeliveryStarted {
        delivery: PendingDelivery,
        confirmation: oneshot::Sender<()>,
    },
    DeliveryHistory {
        events: Vec<DanmuEvent>,
    },
    DeliveryFailed {
        delivery_ids: Vec<String>,
        content: String,
        message: String,
    },
    DeliveryTimedOut {
        delivery_ids: Vec<String>,
    },
    DeliveryRejected {
        content: String,
        message: String,
    },
    DeliveryCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryStatus {
    Idle,
    Sending,
    AwaitingEcho,
    Verifying,
    Delivered,
    Uncertain,
    Failed,
}

#[derive(Debug, Clone)]
struct PendingDelivery {
    id: String,
    content: String,
    broadcaster_name: String,
    broadcaster_id: String,
    submitted_at: DateTime<Utc>,
}

impl PendingDelivery {
    fn new(
        content: String,
        broadcaster_name: String,
        broadcaster_id: String,
        submitted_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            content: content.trim().to_owned(),
            broadcaster_name: broadcaster_name.trim().to_owned(),
            broadcaster_id: broadcaster_id.trim().to_owned(),
            submitted_at,
        }
    }

    fn matches(&self, event: &DanmuEvent) -> bool {
        if event.kind != DanmuEventKind::Danmu
            || normalized_message(&event.content) != normalized_message(&self.content)
            || event.timestamp < self.submitted_at - chrono::Duration::seconds(3)
            || event.timestamp > self.submitted_at + chrono::Duration::seconds(15)
        {
            return false;
        }
        match usable_author_id(event.author_id.as_deref()) {
            Some(author_id) => author_id == self.broadcaster_id,
            None => event.username.as_deref().is_some_and(|username| {
                username.eq_ignore_ascii_case(&self.broadcaster_name)
                    || masked_name_matches(username, &self.broadcaster_name)
            }),
        }
    }

    fn canonicalize(&self, event: &mut DanmuEvent) {
        event.username = Some(self.broadcaster_name.clone());
        event.author_id = Some(self.broadcaster_id.clone());
    }
}

#[derive(Debug)]
struct ActiveDelivery {
    delivery: PendingDelivery,
    confirmation: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeLevel {
    Info,
    Success,
    Warning,
    Error,
    Progress,
}

impl NoticeLevel {
    fn lifetime(self) -> Option<Duration> {
        match self {
            Self::Info | Self::Success => Some(Duration::from_secs(3)),
            Self::Warning | Self::Error | Self::Progress => None,
        }
    }
}

impl UiEvent {
    fn success(message: impl Into<String>) -> Self {
        Self::Notice {
            message: message.into(),
            level: NoticeLevel::Success,
        }
    }

    fn warning(message: impl Into<String>) -> Self {
        Self::Notice {
            message: message.into(),
            level: NoticeLevel::Warning,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self::Notice {
            message: message.into(),
            level: NoticeLevel::Error,
        }
    }
}

fn operation_notice(result: Result<String>) -> UiEvent {
    match result {
        Ok(message) => UiEvent::success(message),
        Err(error) => UiEvent::error(error.to_string()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandSpec {
    completion: &'static str,
    usage: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Copy)]
enum SlashSuggestion<'a> {
    Command(&'static CommandSpec),
    Theme {
        id: &'a str,
        label: &'a str,
        current: bool,
    },
    ReloadThemes,
}

impl SlashSuggestion<'_> {
    fn completion(self) -> String {
        match self {
            Self::Command(spec) => spec.completion.to_owned(),
            Self::Theme { id, .. } => format!("/theme {id}"),
            Self::ReloadThemes => "/theme reload".into(),
        }
    }

    fn usage(self) -> String {
        match self {
            Self::Command(spec) => spec.usage.to_owned(),
            Self::Theme { id, current, .. } => {
                format!("{} {id}", if current { "●" } else { " " })
            }
            Self::ReloadThemes => "↻ reload".into(),
        }
    }

    fn description(self) -> String {
        match self {
            Self::Command(spec) => spec.description.to_owned(),
            Self::Theme { label, current, .. } if current => format!("{label} · 当前"),
            Self::Theme { label, .. } => label.to_owned(),
            Self::ReloadThemes => "重新读取 themes.json".into(),
        }
    }

    fn opens_submenu(self) -> bool {
        matches!(self, Self::Command(spec) if spec.completion == "/theme")
    }
}

enum SlashKeyAction {
    Ignored,
    Handled,
    Submit(String),
}

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        completion: "/help",
        usage: "/help",
        description: "显示命令面板操作提示",
    },
    CommandSpec {
        completion: "/login",
        usage: "/login",
        description: "登录 B 站账号",
    },
    CommandSpec {
        completion: "/logout",
        usage: "/logout",
        description: "清除独立登录态",
    },
    CommandSpec {
        completion: "/layout",
        usage: "/layout",
        description: "切换聊天与信息流布局",
    },
    CommandSpec {
        completion: "/theme",
        usage: "/theme",
        description: "打开主题选择列表",
    },
    CommandSpec {
        completion: "/names show",
        usage: "/names show",
        description: "显示用户名",
    },
    CommandSpec {
        completion: "/names hide",
        usage: "/names hide",
        description: "隐藏用户名",
    },
    CommandSpec {
        completion: "/time show",
        usage: "/time show",
        description: "显示消息时间",
    },
    CommandSpec {
        completion: "/time hide",
        usage: "/time hide",
        description: "隐藏消息时间",
    },
    CommandSpec {
        completion: "/feature",
        usage: "/feature",
        description: "设为重点消息",
    },
    CommandSpec {
        completion: "/archive ",
        usage: "/archive [关键词]",
        description: "搜索归档会话",
    },
    CommandSpec {
        completion: "/obs",
        usage: "/obs",
        description: "检查 OBS 连接并显示状态",
    },
    CommandSpec {
        completion: "/obs status",
        usage: "/obs status",
        description: "查看 OBS 状态",
    },
    CommandSpec {
        completion: "/obs connect",
        usage: "/obs connect",
        description: "检查 OBS 连接",
    },
    CommandSpec {
        completion: "/obs mute",
        usage: "/obs mute",
        description: "静音麦克风",
    },
    CommandSpec {
        completion: "/obs unmute",
        usage: "/obs unmute",
        description: "取消麦克风静音",
    },
    CommandSpec {
        completion: "/obs scene ",
        usage: "/obs scene [名称]",
        description: "列出或切换场景",
    },
    CommandSpec {
        completion: "/obs config mic ",
        usage: "/obs config mic [名称]",
        description: "列出或选择麦克风",
    },
    CommandSpec {
        completion: "/obs config password",
        usage: "/obs config password",
        description: "安全更新 OBS 密码",
    },
    CommandSpec {
        completion: "/obs start",
        usage: "/obs start",
        description: "开始推流",
    },
    CommandSpec {
        completion: "/obs stop",
        usage: "/obs stop",
        description: "请求停止推流",
    },
    CommandSpec {
        completion: "/obs confirm",
        usage: "/obs confirm",
        description: "确认停止推流",
    },
    CommandSpec {
        completion: "/obs cancel",
        usage: "/obs cancel",
        description: "取消停止推流",
    },
    CommandSpec {
        completion: "/quit",
        usage: "/quit",
        description: "退出弹幕台",
    },
];

fn slash_suggestions<'a>(
    input: &str,
    themes: &'a crate::theme::ThemeCatalog,
) -> Vec<SlashSuggestion<'a>> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    if let Some(rest) = input.strip_prefix("/theme")
        && (rest.is_empty() || rest.starts_with(' '))
    {
        let query = rest.trim();
        let selected = themes.selected();
        let mut entries = themes
            .entries()
            .filter(|(id, _)| id.starts_with(query))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(id, _)| usize::from(*id != selected));
        let mut suggestions = entries
            .into_iter()
            .map(|(id, label)| SlashSuggestion::Theme {
                id,
                label,
                current: id == selected,
            })
            .collect::<Vec<_>>();
        if "reload".starts_with(query) {
            suggestions.push(SlashSuggestion::ReloadThemes);
        }
        return suggestions;
    }
    COMMAND_SPECS
        .iter()
        .filter(|spec| spec.completion.starts_with(input))
        .map(SlashSuggestion::Command)
        .collect()
}

pub struct TerminalApp {
    config: TerminalConfig,
    client: BilibiliClient,
    account: AccountClient,
    send_queue: Arc<tokio::sync::Mutex<()>>,
    obs: ObsController,
    journal: SessionJournal,
    session: DanmuSession,
    room: Option<RoomSnapshot>,
    room_updated_at: Option<DateTime<Local>>,
    connection: String,
    watched: Option<u64>,
    likes: Option<u64>,
    online_viewers: Option<u64>,
    input: EditorInput,
    slash_selection: usize,
    selected: usize,
    scroll_offset: usize,
    notice: String,
    notice_deadline: Option<Instant>,
    delivery_status: DeliveryStatus,
    delivery_status_deadline: Option<Instant>,
    layout_chat: bool,
    show_name: bool,
    show_time: bool,
    account_status: AccountStatus,
    awaiting_stop_confirmation: bool,
    login_qr: Option<Vec<String>>,
    secret_mode: bool,
    selection_active: bool,
    page_event_count: usize,
    quit_requested: bool,
    unread_live_count: u64,
    obs_status: Option<ObsStatus>,
    microphone_level: Option<MicrophoneLevel>,
    obs_error: Option<String>,
    obs_checked_at: Option<DateTime<Local>>,
    pending_deliveries: VecDeque<ActiveDelivery>,
    confirmed_deliveries: VecDeque<PendingDelivery>,
    last_realtime_at: Option<DateTime<Local>>,
    live_danmu_count: u64,
    last_live_danmu_at: Option<DateTime<Local>>,
    animation_tick: u64,
}

impl TerminalApp {
    pub async fn run(
        config: TerminalConfig,
        client: BilibiliClient,
        account: AccountClient,
        obs: ObsController,
        journal: SessionJournal,
    ) -> Result<()> {
        let room_id = config.room_id.clone();
        let mut session = journal
            .recover_latest_interrupted()?
            .filter(|session| session.room_id == room_id)
            .unwrap_or_else(|| DanmuSession::new(&room_id));
        if session.status != crate::domain::DanmuSessionStatus::Active {
            session.resume();
        }
        journal.start(&session)?;
        let (room, initial_room_error) = match client.room_snapshot(&room_id).await {
            Ok(room) => (Some(room), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let account_status = account.status().await.unwrap_or(AccountStatus::SignedOut);
        let (online_viewers, likes, initial_online_error, initial_likes_error) =
            if matches!(account_status, AccountStatus::SignedIn { .. }) {
                match room.as_ref() {
                    Some(room) => {
                        let (online_result, likes_result) = tokio::join!(
                            account.current_online_viewers(&room.room_id, &room.broadcaster_id),
                            account.current_likes(&room.room_id),
                        );
                        let (online_viewers, online_error) = match online_result {
                            Ok(viewers) => (viewers, None),
                            Err(error) => (None, Some(error.to_string())),
                        };
                        let (likes, likes_error) = match likes_result {
                            Ok(likes) => (likes, None),
                            Err(error) => (None, Some(error.to_string())),
                        };
                        (online_viewers, likes, online_error, likes_error)
                    }
                    None => (None, None, None, None),
                }
            } else {
                (None, None, None, None)
            };
        let room_updated_at = room.as_ref().map(|_| Local::now());
        let mut app = Self {
            layout_chat: config.chat_layout,
            show_name: config.show_name,
            show_time: config.show_time,
            config,
            client: client.clone(),
            account,
            send_queue: Arc::new(tokio::sync::Mutex::new(())),
            obs,
            journal,
            session,
            room,
            connection: "连接中".into(),
            watched: None,
            likes,
            online_viewers,
            input: EditorInput::default(),
            room_updated_at,
            slash_selection: 0,
            selected: 0,
            scroll_offset: 0,
            notice: "Tab 切换布局；输入 /help 查看命令".into(),
            notice_deadline: Some(Instant::now() + Duration::from_secs(6)),
            delivery_status: DeliveryStatus::Idle,
            delivery_status_deadline: None,
            account_status,
            awaiting_stop_confirmation: false,
            login_qr: None,
            secret_mode: false,
            selection_active: false,
            page_event_count: 1,
            quit_requested: false,
            unread_live_count: 0,
            obs_status: None,
            microphone_level: None,
            pending_deliveries: VecDeque::new(),
            confirmed_deliveries: VecDeque::new(),
            last_realtime_at: None,
            live_danmu_count: 0,
            obs_error: None,
            obs_checked_at: None,
            last_live_danmu_at: None,
            animation_tick: 0,
        };
        if let Some(error) = initial_room_error {
            app.set_notice(format!("房间数据读取失败：{error}"), NoticeLevel::Error);
        } else if let Some(error) = initial_online_error {
            app.set_notice(format!("在线人数读取失败：{error}"), NoticeLevel::Error);
        } else if let Some(error) = initial_likes_error {
            app.set_notice(format!("点赞数读取失败：{error}"), NoticeLevel::Error);
        }
        let (client_tx, mut client_rx) = mpsc::channel(512);
        let (stop_tx, stop_rx) = watch::channel(false);
        let stream_room_id = room_id.clone();
        let stream_stop = stop_rx.clone();
        tokio::spawn(async move {
            client.run(stream_room_id, client_tx, stream_stop).await;
        });
        let (ui_tx, mut ui_rx) = mpsc::channel(32);
        let (meter_tx, mut meter_rx) = watch::channel(None);
        let meter_obs = app.obs.clone();
        let meter_stop = stop_rx.clone();
        tokio::spawn(async move {
            meter_obs
                .monitor_microphone_levels(meter_tx, meter_stop)
                .await;
        });
        let obs_client = app.obs.clone();
        let obs_tx = ui_tx.clone();
        tokio::spawn(async move {
            loop {
                let status = obs_client
                    .fetch_status()
                    .await
                    .map_err(|error| error.to_string());
                if obs_tx.send(UiEvent::ObsStatus(status)).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        let room_client = app.client.clone();
        let room_account = app.account.clone();
        let room_tx = ui_tx.clone();
        let refresh_room_id = room_id.clone();
        let mut last_room = app.room.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                match room_client
                    .room_snapshot_preserving(&refresh_room_id, last_room.as_ref())
                    .await
                {
                    Ok(snapshot) => {
                        last_room = Some(snapshot.clone());
                        let (online, likes) = tokio::join!(
                            room_account.current_online_viewers(
                                &snapshot.room_id,
                                &snapshot.broadcaster_id,
                            ),
                            room_account.current_likes(&snapshot.room_id),
                        );
                        let online = online.map_err(|error| error.to_string());
                        let likes = likes.map_err(|error| error.to_string());
                        if room_tx.send(UiEvent::RoomSnapshot(snapshot)).await.is_err()
                            || room_tx.send(UiEvent::OnlineViewers(online)).await.is_err()
                            || room_tx.send(UiEvent::Likes(likes)).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        if room_tx
                            .send(UiEvent::Notice {
                                message: format!("房间数据刷新失败，继续显示上次成功值：{error}"),
                                level: NoticeLevel::Error,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        let mut events = EventStream::new();
        let mut terminal = TerminalGuard::enter()?;
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut should_quit = false;

        while !should_quit {
            terminal.terminal.draw(|frame| draw(frame, &mut app))?;
            tokio::select! {
                _ = tick.tick() => {
                    app.animation_tick = app.animation_tick.wrapping_add(1);
                    app.expire_notice_at(Instant::now());
                },
                event = events.next() => if let Some(Ok(event)) = event {
                    match event {
                        Event::Key(key) => {
                            should_quit = app.handle_key(key, ui_tx.clone()).await?;
                        }
                        Event::Mouse(mouse) => app.handle_mouse(mouse),
                        _ => {}
                    }
                },
                message = client_rx.recv() => if let Some(message) = message { app.handle_client_event(message); },
                event = ui_rx.recv() => if let Some(event) = event { app.handle_ui_event(event); },
                changed = meter_rx.changed() => if changed.is_ok() {
                    app.microphone_level = *meter_rx.borrow_and_update();
                },
            }
        }
        let _ = stop_tx.send(true);
        app.session
            .end(Utc::now(), DanmuSessionEndReason::Completed);
        app.journal.end(&app.session)?;
        Ok(())
    }

    fn handle_client_event(&mut self, event: BilibiliClientEvent) {
        match event {
            BilibiliClientEvent::Connected { room_id } => {
                self.connection = format!("已连接 {room_id}");
                self.last_realtime_at = Some(Local::now());
            }
            BilibiliClientEvent::Disconnected { reason } => {
                self.connection = format!("实时连接中断 · {reason}");
            }
            BilibiliClientEvent::Heartbeat => {
                self.last_realtime_at = Some(Local::now());
            }
            BilibiliClientEvent::Watched(value) => {
                self.watched = Some(value);
                self.last_realtime_at = Some(Local::now());
            }
            BilibiliClientEvent::Likes(value) => {
                self.likes = Some(self.likes.map_or(value, |current| current.max(value)));
                self.last_realtime_at = Some(Local::now());
            }
            BilibiliClientEvent::UnhandledCommand { command } => {
                self.last_realtime_at = Some(Local::now());
                if let Err(error) = self.journal.unhandled_command(&self.session, &command) {
                    self.set_notice(
                        format!("记录未处理的 B 站命令失败：{error}"),
                        NoticeLevel::Error,
                    );
                }
            }
            BilibiliClientEvent::Error(error) => self.set_notice(error, NoticeLevel::Error),
            BilibiliClientEvent::Danmu(event) => {
                self.last_realtime_at = Some(Local::now());
                if event.origin == DanmuEventOrigin::Live && event.kind == DanmuEventKind::Danmu {
                    self.live_danmu_count = self.live_danmu_count.saturating_add(1);
                    self.last_live_danmu_at = Some(Local::now());
                }
                self.ingest_event(event);
            }
        }
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Notice { message, level } => {
                self.set_notice(message, level);
                self.login_qr = None;
            }
            UiEvent::LoginQr(lines) => {
                self.login_qr = Some(lines);
                self.set_notice("请使用哔哩哔哩客户端扫码登录", NoticeLevel::Progress);
            }
            UiEvent::LoginDone(status) => {
                self.account_status = status;
                self.login_qr = None;
                self.set_notice("B 站账号登录成功", NoticeLevel::Success);
            }
            UiEvent::ObsStatus(result) => {
                self.obs_checked_at = Some(Local::now());
                match result {
                    Ok(status) => {
                        self.obs_status = Some(status);
                        self.obs_error = None;
                    }
                    Err(error) => {
                        self.obs_status = None;
                        self.obs_error = Some(error);
                    }
                }
            }
            UiEvent::RoomSnapshot(snapshot) => {
                self.room = Some(snapshot);
                self.room_updated_at = Some(Local::now());
            }
            UiEvent::OnlineViewers(result) => match result {
                Ok(viewers) => self.online_viewers = viewers,
                Err(error) => {
                    self.set_notice(
                        format!("在线人数刷新失败，继续显示上次成功值：{error}"),
                        NoticeLevel::Error,
                    );
                }
            },
            UiEvent::Likes(result) => match result {
                Ok(Some(value)) => {
                    self.likes = Some(self.likes.map_or(value, |current| current.max(value)));
                }
                Ok(None) => {}
                Err(error) => {
                    self.set_notice(
                        format!("点赞数刷新失败，继续显示上次成功值：{error}"),
                        NoticeLevel::Error,
                    );
                }
            },
            UiEvent::DeliveryStarted {
                delivery,
                confirmation,
            } => {
                self.pending_deliveries.push_back(ActiveDelivery {
                    delivery,
                    confirmation: Some(confirmation),
                });
                self.set_delivery_status(DeliveryStatus::AwaitingEcho);
            }
            UiEvent::DeliveryHistory { events } => {
                for event in events {
                    self.ingest_event(event);
                }
                if !self.pending_deliveries.is_empty() {
                    self.set_delivery_status(DeliveryStatus::Verifying);
                }
            }
            UiEvent::DeliveryFailed {
                delivery_ids,
                content,
                message,
            } => {
                for delivery_id in delivery_ids {
                    self.clear_delivery(&delivery_id);
                }
                self.set_delivery_status(DeliveryStatus::Failed);
                self.set_delivery_notice(format!("{message}；内容：「{content}」"));
            }
            UiEvent::DeliveryTimedOut { delivery_ids } => {
                let contents = delivery_ids
                    .iter()
                    .filter_map(|delivery_id| {
                        self.pending_deliveries
                            .iter()
                            .find(|active| active.delivery.id == *delivery_id)
                            .map(|active| active.delivery.content.clone())
                    })
                    .collect::<Vec<_>>();
                for delivery_id in delivery_ids {
                    self.clear_delivery(&delivery_id);
                }
                if !contents.is_empty() {
                    self.set_delivery_status(DeliveryStatus::Uncertain);
                    self.set_delivery_notice(format!(
                        "未确认送达，已停止提醒；内容：{}",
                        contents
                            .iter()
                            .map(|content| format!("「{content}」"))
                            .collect::<Vec<_>>()
                            .join(" ｜ ")
                    ));
                }
            }
            UiEvent::DeliveryRejected { content, message } => {
                self.set_delivery_status(DeliveryStatus::Failed);
                self.set_delivery_notice(format!("{message}；内容：「{content}」"));
            }
            UiEvent::DeliveryCompleted => {
                if self.pending_deliveries.is_empty() {
                    self.set_delivery_status(DeliveryStatus::Delivered);
                } else {
                    self.set_delivery_status(DeliveryStatus::AwaitingEcho);
                }
            }
        }
    }

    fn ingest_event(&mut self, mut event: DanmuEvent) {
        let live_arrival =
            event.origin == DanmuEventOrigin::Live && event.kind == DanmuEventKind::Danmu;
        let confirmed = self
            .pending_deliveries
            .iter()
            .position(|active| active.delivery.matches(&event));
        self.confirmed_deliveries.retain(|delivery| {
            event.timestamp <= delivery.submitted_at + chrono::Duration::seconds(15)
        });
        if confirmed.is_none()
            && self
                .confirmed_deliveries
                .iter()
                .any(|delivery| delivery.matches(&event))
        {
            return;
        }
        if let Some(index) = confirmed {
            let mut active = self
                .pending_deliveries
                .remove(index)
                .expect("已检查待确认弹幕");
            active.delivery.canonicalize(&mut event);
            self.confirmed_deliveries.push_back(active.delivery.clone());
            while self.confirmed_deliveries.len() > 32 {
                self.confirmed_deliveries.pop_front();
            }
            if let Some(confirmation) = active.confirmation.take() {
                let _ = confirmation.send(());
            }
            let status = if self.pending_deliveries.is_empty() {
                DeliveryStatus::Delivered
            } else {
                DeliveryStatus::AwaitingEcho
            };
            self.set_delivery_status(status);
        }

        if reconcile_cross_origin_event(&mut self.session.recent_events, &event) {
            return;
        }
        let selected_anchor = self
            .selection_active
            .then(|| self.session.recent_events.get(self.selected))
            .flatten()
            .map(|event| event.id.clone());
        let scroll_anchor = (self.scroll_offset > 0)
            .then(|| self.session.recent_events.get(self.scroll_offset))
            .flatten()
            .map(|event| event.id.clone());
        if self.session.ingest(event.clone()) {
            let _ = self.journal.event(&self.session, &event);
            self.selected = selected_anchor
                .as_deref()
                .and_then(|id| {
                    self.session
                        .recent_events
                        .iter()
                        .position(|event| event.id == id)
                })
                .unwrap_or(0);
            self.scroll_offset = scroll_anchor
                .as_deref()
                .and_then(|id| {
                    self.session
                        .recent_events
                        .iter()
                        .position(|event| event.id == id)
                })
                .unwrap_or(0);
            if live_arrival && (self.selection_active || self.scroll_offset > 0) {
                self.unread_live_count = self.unread_live_count.saturating_add(1);
            } else if !self.selection_active && self.scroll_offset == 0 {
                self.unread_live_count = 0;
            }
        }
    }

    fn clear_delivery(&mut self, delivery_id: &str) -> bool {
        let Some(index) = self
            .pending_deliveries
            .iter()
            .position(|active| active.delivery.id == delivery_id)
        else {
            return false;
        };
        self.pending_deliveries.remove(index);
        true
    }

    fn set_notice(&mut self, message: impl Into<String>, level: NoticeLevel) {
        self.set_notice_at(message, level, Instant::now());
    }

    fn set_delivery_notice(&mut self, message: impl Into<String>) {
        self.notice = message.into();
        self.notice_deadline = Some(Instant::now() + DELIVERY_NOTICE_LIFETIME);
    }

    fn set_notice_at(&mut self, message: impl Into<String>, level: NoticeLevel, now: Instant) {
        self.notice = message.into();
        self.notice_deadline = level.lifetime().map(|lifetime| now + lifetime);
    }
    fn set_delivery_status(&mut self, status: DeliveryStatus) {
        self.set_delivery_status_at(status, Instant::now());
    }

    fn set_delivery_status_at(&mut self, status: DeliveryStatus, now: Instant) {
        self.delivery_status = status;
        self.delivery_status_deadline = match status {
            DeliveryStatus::Delivered => Some(now + Duration::from_secs(3)),
            DeliveryStatus::Uncertain | DeliveryStatus::Failed => {
                Some(now + DELIVERY_NOTICE_LIFETIME)
            }
            _ => None,
        };
    }

    fn expire_notice_at(&mut self, now: Instant) {
        if self.notice_deadline.is_some_and(|deadline| now >= deadline) {
            self.notice.clear();
            self.notice_deadline = None;
        }
        if self
            .delivery_status_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.delivery_status = DeliveryStatus::Idle;
            self.delivery_status_deadline = None;
        }
    }

    fn return_to_live(&mut self) {
        self.selection_active = false;
        self.selected = 0;
        self.scroll_offset = 0;
        self.unread_live_count = 0;
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::ScrollUp => self.scroll_page(true),
            MouseEventKind::ScrollDown => self.scroll_page(false),
            _ => {}
        }
    }

    fn scroll_page(&mut self, older: bool) {
        if self.session.recent_events.is_empty() {
            return;
        }
        self.selection_active = false;
        self.selected = 0;
        let page = self.page_event_count.max(1);
        if older {
            self.scroll_offset =
                (self.scroll_offset + page).min(self.session.recent_events.len().saturating_sub(1));
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(page);
            if self.scroll_offset == 0 {
                self.unread_live_count = 0;
            }
        }
    }

    fn handle_slash_key(&mut self, key: &KeyEvent) -> SlashKeyAction {
        if !self.input.starts_with('/') {
            return SlashKeyAction::Ignored;
        }

        let suggestions = slash_suggestions(&self.input, &self.config.themes);
        let move_up = key.code == KeyCode::Up
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p'));
        let move_down = key.code == KeyCode::Down
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('n'));

        if key.code == KeyCode::Esc {
            self.input.clear();
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        if suggestions.is_empty() {
            return SlashKeyAction::Ignored;
        }
        if move_up {
            self.slash_selection = self
                .slash_selection
                .checked_sub(1)
                .unwrap_or(suggestions.len() - 1);
            return SlashKeyAction::Handled;
        }
        if move_down {
            self.slash_selection = (self.slash_selection + 1) % suggestions.len();
            return SlashKeyAction::Handled;
        }
        let selected = self.slash_selection.min(suggestions.len() - 1);
        if key.code == KeyCode::Tab {
            self.input.replace(suggestions[selected].completion());
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        if key.code == KeyCode::Enter {
            let suggestion = suggestions[selected];
            let completion = suggestion.completion();
            let opens_submenu = suggestion.opens_submenu();
            drop(suggestions);
            self.slash_selection = 0;
            if opens_submenu {
                self.input.replace(completion);
                return SlashKeyAction::Handled;
            }
            self.input.clear();
            return SlashKeyAction::Submit(sanitize_input(completion));
        }
        SlashKeyAction::Ignored
    }

    async fn handle_key(&mut self, key: KeyEvent, tx: mpsc::Sender<UiEvent>) -> Result<bool> {
        if key.kind != crossterm::event::KeyEventKind::Press {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        match self.handle_slash_key(&key) {
            SlashKeyAction::Ignored => {}
            SlashKeyAction::Handled => return Ok(false),
            SlashKeyAction::Submit(command) => {
                self.submit(command, tx).await?;
                return Ok(self.quit_requested);
            }
        }
        let selection_navigation = key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Up | KeyCode::Down);
        if self.selection_active
            && !selection_navigation
            && !matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::End)
        {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => {
                    self.input.clear();
                    return Ok(false);
                }
                KeyCode::Char('a') => {
                    self.input.move_to_start();
                    return Ok(false);
                }
                KeyCode::Char('e') => {
                    self.input.move_to_end();
                    return Ok(false);
                }
                KeyCode::Char('w') => {
                    self.input.delete_previous_word();
                    self.slash_selection = 0;
                    return Ok(false);
                }
                KeyCode::Char('k') => {
                    self.input.delete_to_end();
                    self.slash_selection = 0;
                    return Ok(false);
                }
                KeyCode::Char('d') => {
                    self.input.delete_at_cursor();
                    self.slash_selection = 0;
                    return Ok(false);
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                if self.login_qr.take().is_some() {
                    return Ok(false);
                }
                if self.secret_mode {
                    self.secret_mode = false;
                    self.input.clear();
                    return Ok(false);
                }
                if self.selection_active || self.scroll_offset > 0 {
                    self.return_to_live();
                    return Ok(false);
                }
                return Ok(true);
            }
            KeyCode::Tab => {
                self.layout_chat = !self.layout_chat;
                self.set_notice(
                    format!(
                        "已切换为{}布局",
                        if self.layout_chat {
                            "聊天"
                        } else {
                            "信息流"
                        }
                    ),
                    NoticeLevel::Success,
                );
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_selection(true),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_selection(false)
            }
            KeyCode::Home => self.input.move_to_start(),
            KeyCode::End if self.selection_active || self.scroll_offset > 0 => {
                self.return_to_live()
            }
            KeyCode::End => self.input.move_to_end(),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.move_word_left()
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.move_word_right()
            }
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Backspace => {
                self.input.delete_before_cursor();
                self.slash_selection = 0;
            }
            KeyCode::Delete => {
                self.input.delete_at_cursor();
                self.slash_selection = 0;
            }
            KeyCode::Enter => {
                if self.selection_active {
                    let username = self
                        .session
                        .recent_events
                        .get(self.selected)
                        .and_then(|event| event.username.clone());
                    self.return_to_live();
                    if let Some(username) = username {
                        let needs_space = self
                            .input
                            .previous_grapheme()
                            .is_some_and(|value| !value.trim().is_empty());
                        self.insert_text(&format!(
                            "{}@{username} ",
                            if needs_space { " " } else { "" }
                        ));
                        self.set_notice(
                            format!("已选择 {username}，继续编辑后按 Enter 发送"),
                            NoticeLevel::Info,
                        );
                    }
                } else {
                    let command = sanitize_input(self.input.take());
                    if !command.is_empty() {
                        self.submit(command, tx).await?;
                    }
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(character)
            }
            _ => {}
        }
        Ok(self.quit_requested)
    }

    async fn submit(&mut self, input: String, tx: mpsc::Sender<UiEvent>) -> Result<()> {
        if self.secret_mode {
            self.secret_mode = false;
            ObsController::set_password(&input)?;
            self.set_notice("OBS WebSocket 密码已安全更新", NoticeLevel::Success);
            return Ok(());
        }
        if input.starts_with('/') {
            return self.command(&input, tx).await;
        }
        let segments = segment_message(&input, 20);
        if segments.is_empty() {
            return Ok(());
        }
        let submitted_content = input;
        let account = self.account.clone();
        let client = self.client.clone();
        let room = self.config.room_id.clone();
        let send_queue = self.send_queue.clone();
        tokio::spawn(async move {
            let queue_guard = send_queue.lock().await;
            let (broadcaster_name, broadcaster_id) = match account.status().await {
                Ok(AccountStatus::SignedIn {
                    display_name,
                    user_id,
                }) => (display_name, user_id),
                Ok(AccountStatus::SignedOut) => {
                    let _ = tx
                        .send(UiEvent::DeliveryRejected {
                            content: submitted_content.clone(),
                            message: "发送失败：还没有 B 站登录态".into(),
                        })
                        .await;
                    return;
                }
                Err(error) => {
                    let _ = tx
                        .send(UiEvent::DeliveryRejected {
                            content: submitted_content.clone(),
                            message: format!("读取 B 站登录态失败：{error}"),
                        })
                        .await;
                    return;
                }
            };
            let mut confirmations = Vec::with_capacity(segments.len());
            let mut delivery_ids = Vec::with_capacity(segments.len());

            for (index, segment) in segments.iter().enumerate() {
                let delivery = PendingDelivery::new(
                    segment.clone(),
                    broadcaster_name.clone(),
                    broadcaster_id.clone(),
                    Utc::now(),
                );
                let delivery_id = delivery.id.clone();
                delivery_ids.push(delivery_id.clone());
                let (confirmation_tx, confirmation_rx) = oneshot::channel();
                if tx
                    .send(UiEvent::DeliveryStarted {
                        delivery,
                        confirmation: confirmation_tx,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                if let Err(error) = account.send_danmu(segment, &room, None).await {
                    let _ = tx
                        .send(UiEvent::DeliveryFailed {
                            delivery_ids,
                            content: segment.clone(),
                            message: format!("第 {} 段发送失败：{error}", index + 1),
                        })
                        .await;
                    return;
                }
                confirmations.push((delivery_id, confirmation_rx));
                tokio::time::sleep(SEND_QUEUE_INTERVAL).await;
            }
            drop(queue_guard);

            let unresolved =
                wait_for_delivery_confirmations(&client, &room, &tx, confirmations).await;
            if unresolved.is_empty() {
                let _ = tx.send(UiEvent::DeliveryCompleted).await;
            } else {
                let _ = tx
                    .send(UiEvent::DeliveryTimedOut {
                        delivery_ids: unresolved,
                    })
                    .await;
            }
        });
        self.set_delivery_status(DeliveryStatus::Sending);
        Ok(())
    }

    async fn command(&mut self, raw: &str, tx: mpsc::Sender<UiEvent>) -> Result<()> {
        let parts = raw.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["/quit"] | ["/q"] => {
                self.quit_requested = true;
            }
            ["/help"] => {
                self.set_notice(
                    "输入 / 打开命令面板；↑/↓ 选择；Enter 执行；Tab 仅补全",
                    NoticeLevel::Info,
                );
            }
            ["/layout"] => {
                self.layout_chat = !self.layout_chat;
                self.set_notice("已切换布局", NoticeLevel::Success);
            }
            ["/theme"] => {
                let choices = self.config.themes.choices().join("、");
                let path = self.config.themes.path().display().to_string();
                self.set_notice(
                    format!(
                        "当前主题：{}；可选：{choices}；配置：{path}",
                        self.config.theme_name
                    ),
                    NoticeLevel::Info,
                );
            }
            ["/theme", "reload"] => match self.config.themes.reload() {
                Ok((theme_name, palette)) => {
                    self.config.theme_name = theme_name.clone();
                    self.config.palette = palette;
                    self.set_notice(
                        format!("已重新加载主题：{theme_name}"),
                        NoticeLevel::Success,
                    );
                }
                Err(error) => self.set_notice(error.to_string(), NoticeLevel::Error),
            },
            ["/theme", theme_name] => match self.config.themes.select(theme_name) {
                Ok((theme_name, palette)) => {
                    self.config.theme_name = theme_name.clone();
                    self.config.palette = palette;
                    self.set_notice(format!("已切换主题：{theme_name}"), NoticeLevel::Success);
                }
                Err(error) => self.set_notice(error.to_string(), NoticeLevel::Error),
            },
            ["/names", "show"] => {
                self.show_name = true;
                self.set_notice("已显示用户名", NoticeLevel::Success);
            }
            ["/names", "hide"] => {
                self.show_name = false;
                self.set_notice("已隐藏用户名", NoticeLevel::Success);
            }
            ["/time", "show"] => {
                self.show_time = true;
                self.set_notice("已显示时间", NoticeLevel::Success);
            }
            ["/time", "hide"] => {
                self.show_time = false;
                self.set_notice("已隐藏时间", NoticeLevel::Success);
            }
            ["/login"] => {
                self.set_notice("正在创建 B 站登录二维码…", NoticeLevel::Progress);
                self.start_login(tx);
            }
            ["/logout"] => match self.account.sign_out() {
                Ok(()) => {
                    self.account_status = AccountStatus::SignedOut;
                    self.set_notice("已清除 TUI 独立登录态", NoticeLevel::Success);
                }
                Err(error) => self.set_notice(error.to_string(), NoticeLevel::Error),
            },
            ["/feature"] => {
                self.with_selected(|session, id| session.feature(Some(id)), "已设为重点消息")
            }
            ["/archive"] => self.search_archive(""),
            ["/archive", rest @ ..] => self.search_archive(&rest.join(" ")),
            ["/obs"] | ["/obs", "status"] | ["/obs", "connect"] => {
                self.set_notice("正在检查 OBS 连接…", NoticeLevel::Progress);
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs.fetch_status().await.map(|status| {
                        format!(
                            "OBS 场景：{}；推流：{:?}；麦克风：{:?}{}",
                            status.current_scene,
                            status.stream,
                            status.microphone,
                            status
                                .compatibility_warning
                                .map(|value| format!("；{value}"))
                                .unwrap_or_default()
                        )
                    });
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "mute"] | ["/obs", "unmute"] => {
                self.set_notice(
                    if parts[1] == "mute" {
                        "正在静音麦克风…"
                    } else {
                        "正在取消麦克风静音…"
                    },
                    NoticeLevel::Progress,
                );
                let muted = parts[1] == "mute";
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs.set_microphone_muted(muted).await.map(|_| {
                        if muted {
                            "麦克风已静音"
                        } else {
                            "麦克风已取消静音"
                        }
                        .to_string()
                    });
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "scene"] => {
                self.set_notice("正在读取 OBS 场景…", NoticeLevel::Progress);
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs
                        .list_scenes()
                        .await
                        .map(|items| format!("OBS 场景：{}", items.join("、")));
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "scene", scene @ ..] if !scene.is_empty() => {
                let scene = scene.join(" ");
                self.set_notice(format!("正在切换到场景：{scene}"), NoticeLevel::Progress);
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs
                        .switch_scene(&scene)
                        .await
                        .map(|_| format!("已切换场景：{scene}"));
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "config", "mic"] => {
                self.set_notice("正在读取 OBS 输入…", NoticeLevel::Progress);
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs.list_inputs().await.map(|items| {
                        format!(
                            "OBS 输入：{}；使用 /obs config mic <名称>",
                            items.join("、")
                        )
                    });
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "config", "mic", name @ ..] if !name.is_empty() => {
                let name = name.join(" ");
                self.set_notice(format!("正在验证麦克风输入：{name}"), NoticeLevel::Progress);
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs
                        .set_microphone_name(name.clone())
                        .await
                        .map(|_| format!("麦克风输入已切换为：{name}"));
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "start"] => {
                self.set_notice("正在启动 OBS 推流…", NoticeLevel::Progress);
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs
                        .start_stream()
                        .await
                        .map(|_| "OBS 已开始推流".to_string());
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "stop"] => {
                self.awaiting_stop_confirmation = true;
                self.set_notice(
                    "停止推流有中断直播风险；再次输入 /obs confirm",
                    NoticeLevel::Warning,
                );
            }
            ["/obs", "confirm"] if self.awaiting_stop_confirmation => {
                self.awaiting_stop_confirmation = false;
                self.set_notice("正在停止 OBS 推流…", NoticeLevel::Progress);
                let obs = self.obs.clone();
                tokio::spawn(async move {
                    let result = obs
                        .stop_stream()
                        .await
                        .map(|_| "OBS 已停止推流".to_string());
                    let _ = tx.send(operation_notice(result)).await;
                });
            }
            ["/obs", "confirm"] => {
                self.set_notice("当前没有待确认的停止推流操作", NoticeLevel::Info);
            }
            ["/obs", "cancel"] => {
                self.awaiting_stop_confirmation = false;
                self.set_notice("已取消停止推流", NoticeLevel::Success);
            }
            ["/obs", "config", "password"] => {
                self.secret_mode = true;
                self.input.clear();
                self.set_notice(
                    "请输入 OBS WebSocket 密码并按 Enter；输入不会显示或进入历史",
                    NoticeLevel::Progress,
                );
            }
            _ => self.set_notice(
                format!("未知命令：{raw}；输入 /help 查看命令"),
                NoticeLevel::Error,
            ),
        }
        let _ = self.journal.snapshot(&self.session);
        Ok(())
    }

    fn start_login(&mut self, tx: mpsc::Sender<UiEvent>) {
        let account = self.account.clone();
        tokio::spawn(async move {
            let challenge = match account.login_challenge().await {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.send(UiEvent::error(error.to_string())).await;
                    return;
                }
            };
            let lines = compact_qr_lines(challenge.url.as_str())
                .unwrap_or_else(|_| vec![challenge.url.to_string()]);
            if tx.send(UiEvent::LoginQr(lines)).await.is_err() {
                return;
            }
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                match account.poll_login(&challenge.key).await {
                    Ok(LoginPoll::Waiting | LoginPoll::Scanned) => continue,
                    Ok(LoginPoll::Expired) => {
                        let _ = tx
                            .send(UiEvent::warning("登录二维码已过期，请重新输入 /login"))
                            .await;
                        return;
                    }
                    Ok(LoginPoll::SignedIn(status)) => {
                        let _ = tx.send(UiEvent::LoginDone(status)).await;
                        return;
                    }
                    Err(error) => {
                        let _ = tx.send(UiEvent::error(error.to_string())).await;
                        return;
                    }
                }
            }
        });
    }

    fn with_selected(&mut self, operation: impl FnOnce(&mut DanmuSession, &str), notice: &str) {
        if let Some(id) = self
            .session
            .recent_events
            .get(self.selected)
            .map(|event| event.id.clone())
        {
            operation(&mut self.session, &id);
            self.set_notice(notice, NoticeLevel::Success);
        } else {
            self.set_notice("当前没有可操作的消息", NoticeLevel::Info);
        }
    }

    fn move_selection(&mut self, older: bool) {
        let candidates = self
            .session
            .recent_events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.kind == DanmuEventKind::Danmu
                    && event
                        .username
                        .as_deref()
                        .is_some_and(|name| !name.trim().is_empty())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            self.return_to_live();
            return;
        }
        if !self.selection_active {
            self.selection_active = true;
            self.unread_live_count = 0;
            self.scroll_offset = 0;
            self.selected = candidates[0];
            return;
        }
        let position = candidates
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let next = if older {
            (position + 1).min(candidates.len() - 1)
        } else {
            position.saturating_sub(1)
        };
        self.selected = candidates[next];
    }

    fn search_archive(&mut self, query: &str) {
        match self.journal.search(query) {
            Ok(results) => {
                let message = if results.is_empty() {
                    "未找到归档会话".into()
                } else {
                    format!(
                        "找到 {} 个归档会话：{}",
                        results.len(),
                        results
                            .iter()
                            .take(3)
                            .map(|item| format!(
                                "{}@{}",
                                item.room_id,
                                item.started_at.with_timezone(&Local).format("%m-%d %H:%M")
                            ))
                            .collect::<Vec<_>>()
                            .join("、")
                    )
                };
                self.set_notice(message, NoticeLevel::Info);
            }
            Err(error) => self.set_notice(error.to_string(), NoticeLevel::Error),
        }
    }

    fn insert(&mut self, character: char) {
        let mut buffer = [0; 4];
        self.insert_text(character.encode_utf8(&mut buffer));
    }

    fn insert_text(&mut self, text: &str) {
        self.input.insert_text(text);
        self.slash_selection = 0;
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}
impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}
fn retain_unconfirmed(confirmations: &mut Vec<(String, oneshot::Receiver<()>)>) {
    confirmations.retain_mut(|(_, confirmation)| match confirmation.try_recv() {
        Ok(()) | Err(oneshot::error::TryRecvError::Closed) => false,
        Err(oneshot::error::TryRecvError::Empty) => true,
    });
}

async fn wait_for_delivery_confirmations(
    client: &BilibiliClient,
    room_id: &str,
    tx: &mpsc::Sender<UiEvent>,
    mut confirmations: Vec<(String, oneshot::Receiver<()>)>,
) -> Vec<String> {
    for attempt in 0..8 {
        retain_unconfirmed(&mut confirmations);
        if confirmations.is_empty() {
            return Vec::new();
        }
        let delay = if attempt == 0 {
            Duration::from_millis(350)
        } else {
            Duration::from_secs(2)
        };
        tokio::time::sleep(delay).await;
        retain_unconfirmed(&mut confirmations);
        if confirmations.is_empty() {
            return Vec::new();
        }
        if let Ok(events) = client.history(room_id).await
            && tx.send(UiEvent::DeliveryHistory { events }).await.is_err()
        {
            return Vec::new();
        }
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    retain_unconfirmed(&mut confirmations);
    confirmations
        .into_iter()
        .map(|(delivery_id, _)| delivery_id)
        .collect()
}

fn reconcile_cross_origin_event(events: &mut [DanmuEvent], incoming: &DanmuEvent) -> bool {
    let Some(existing) = events
        .iter_mut()
        .find(|event| cross_origin_duplicate(event, incoming))
    else {
        return false;
    };
    if incoming.origin == DanmuEventOrigin::History {
        existing.username = incoming.username.clone();
        existing.author_id = incoming.author_id.clone();
        existing.platform_event_id = incoming.platform_event_id.clone();
    } else if existing.emotes.is_empty() && !incoming.emotes.is_empty() {
        existing.emotes = incoming.emotes.clone();
    }
    true
}

fn display_events(events: &[DanmuEvent]) -> impl Iterator<Item = &DanmuEvent> {
    let now = Utc::now();
    let enter_cutoff = now - chrono::Duration::seconds(5);
    let like_cutoff = now - chrono::Duration::seconds(3);
    events.iter().rev().filter(move |event| match event.kind {
        DanmuEventKind::Enter => event.timestamp > enter_cutoff,
        DanmuEventKind::Like => event.timestamp > like_cutoff,
        _ => true,
    })
}

fn draw(frame: &mut ratatui::Frame, app: &mut TerminalApp) {
    let palette = app.config.palette;
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.background)),
        area,
    );
    if area.width < 32 || area.height < 7 {
        draw_compact(frame, area, app, palette);
        if let Some(lines) = &app.login_qr {
            draw_qr(frame, area, lines, palette);
        }
        return;
    }
    let status_lines = technical_status_lines(app, palette, area.width);
    let status_height = u16::try_from(status_lines.len())
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(4))
        .max(1);
    let notice_lines = application_notice_lines(app, palette, area.width);
    let available_after_body = area.height.saturating_sub(status_height + 2);
    let notice_height = u16::try_from(notice_lines.len())
        .unwrap_or(u16::MAX)
        .min(3)
        .min(available_after_body.saturating_sub(2));
    let input_height = input_area_height(app, area.width)
        .min(available_after_body.saturating_sub(notice_height))
        .max(2);
    let rows = Layout::vertical([
        Constraint::Length(status_height),
        Constraint::Min(2),
        Constraint::Length(notice_height),
        Constraint::Length(input_height),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(Text::from(status_lines)), rows[0]);
    draw_body(frame, rows[1], app, palette);
    if notice_height > 0 {
        frame.render_widget(Paragraph::new(Text::from(notice_lines)), rows[2]);
    }
    draw_input(frame, rows[3], app, palette);
    draw_command_palette(frame, rows[1], app, palette);

    if let Some(lines) = &app.login_qr {
        draw_qr(frame, area, lines, palette);
    }
}

fn draw_command_palette(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut TerminalApp,
    palette: Palette,
) {
    let suggestions = slash_suggestions(&app.input, &app.config.themes);
    if suggestions.is_empty() || area.height < 4 {
        return;
    }

    let visible = suggestions.len().min(5);
    app.slash_selection = app.slash_selection.min(suggestions.len() - 1);
    let offset = app
        .slash_selection
        .saturating_sub(visible - 1)
        .min(suggestions.len() - visible);
    let height = (visible as u16 + 2).min(area.height);
    let width = area.width.saturating_sub(4).min(72);
    let popup = Rect::new(
        area.x + 2,
        area.y + area.height.saturating_sub(height),
        width,
        height,
    );
    let items = suggestions
        .iter()
        .skip(offset)
        .take(visible)
        .map(|suggestion| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<24}", suggestion.usage()),
                    Style::default()
                        .fg(palette.info)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(suggestion.description(), Style::default().fg(palette.time)),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.slash_selection - offset));
    let title = if app.input.starts_with("/theme") {
        " 主题 · Enter 选择 · ↑/↓ 移动 "
    } else {
        " 命令 · Enter 执行 · Tab 补全 · ↑/↓ 移动 "
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .title(title),
        )
        .style(Style::default().bg(palette.background).fg(palette.content))
        .highlight_style(Style::default().bg(palette.frame).fg(palette.content))
        .highlight_symbol("› ");
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut state);
}

fn draw_compact(frame: &mut ratatui::Frame, area: Rect, app: &mut TerminalApp, palette: Palette) {
    if area.height < 3 {
        frame.render_widget(
            Paragraph::new(format!(
                "{} · 房间 {} · ❯ {}",
                connection_badge(app),
                app.config.room_id,
                visible_input(app)
            ))
            .style(Style::default().fg(palette.content)),
            area,
        );
        return;
    }
    let input_height = input_area_height(app, area.width)
        .min(area.height.saturating_sub(1))
        .max(2);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(input_height)]).split(area);
    let latest = display_events(&app.session.recent_events)
        .map(|event| Line::from(map_event_emotes(event)))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(latest).style(Style::default().fg(palette.content)),
        rows[0],
    );
    draw_input(frame, rows[1], app, palette);
}

fn draw_body(frame: &mut ratatui::Frame, area: Rect, app: &mut TerminalApp, palette: Palette) {
    let title = app
        .session
        .featured_event
        .as_ref()
        .map(|event| {
            if app.show_name {
                format!(
                    "NOW · {}：{}",
                    event.username.as_deref().unwrap_or("观众"),
                    map_event_emotes(event)
                )
            } else {
                format!("NOW · {}", map_event_emotes(event))
            }
        })
        .unwrap_or_else(|| "Ghost Stage".into());
    draw_events(frame, area, app, palette, &title);
}

fn rounded_block(title: &str, palette: Palette) -> Block<'static> {
    let title = if title.is_empty() {
        String::new()
    } else {
        format!(" {title} ")
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(palette.frame))
}

fn room_live_label(room: &RoomSnapshot) -> &'static str {
    match room.live_status {
        RoomLiveStatus::Offline => "○ OFFLINE",
        RoomLiveStatus::Live => "● LIVE",
        RoomLiveStatus::Rotating => "◉ ROTATING",
    }
}

const STATUS_RED: Color = Color::Rgb(225, 29, 72);
const STATUS_RED_DIM: Color = Color::Rgb(159, 18, 57);
const STATUS_ORANGE: Color = Color::Rgb(194, 65, 12);
const STATUS_GREEN: Color = Color::Rgb(21, 128, 61);
const STATUS_BLUE: Color = Color::Rgb(29, 78, 216);

fn status_span(content: String, background: Color, palette: Palette) -> Span<'static> {
    Span::styled(
        content,
        Style::default()
            .fg(contrast_foreground(background, palette))
            .bg(background)
            .add_modifier(Modifier::BOLD),
    )
}

fn fit_display_width(content: &str, width: usize) -> String {
    if UnicodeWidthStr::width(content) <= width {
        return content.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut fitted = String::new();
    let mut used = 0;
    for grapheme in content.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used + grapheme_width + 1 > width {
            break;
        }
        fitted.push_str(grapheme);
        used += grapheme_width;
    }
    fitted.push('…');
    fitted
}

fn live_elapsed(room: &RoomSnapshot, now: DateTime<Utc>) -> String {
    let Some(started_at) = room.live_started_at.filter(|_| room.is_live()) else {
        return "--:--:--".into();
    };
    let seconds = now.signed_duration_since(started_at).num_seconds().max(0);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn primary_status_line(
    room: &RoomSnapshot,
    app: &TerminalApp,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let live_color = if room.is_live() {
        if (app.animation_tick / 2).is_multiple_of(2) {
            STATUS_RED
        } else {
            STATUS_RED_DIM
        }
    } else {
        STATUS_BLUE
    };
    let live = format!(" {} ", room_live_label(room));
    let elapsed = format!(" ◷ {} ", live_elapsed(room, Utc::now()));
    let live_width = UnicodeWidthStr::width(live.as_str());
    let elapsed_width = UnicodeWidthStr::width(elapsed.as_str());
    let title_limit = usize::from(width).saturating_sub(2 * live_width.max(elapsed_width));
    let title = fit_display_width(&room.title, title_limit);
    let title_width = UnicodeWidthStr::width(title.as_str());
    let title_start = usize::from(width).saturating_sub(title_width) / 2;
    let left_gap = title_start.saturating_sub(live_width);
    let right_gap =
        usize::from(width).saturating_sub(live_width + left_gap + title_width + elapsed_width);
    Line::from(vec![
        status_span(live, live_color, palette),
        Span::raw(" ".repeat(left_gap)),
        Span::styled(
            title,
            Style::default()
                .fg(palette.rank)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(right_gap)),
        status_span(elapsed, STATUS_BLUE, palette),
    ])
}

fn secondary_status_line(
    room: &RoomSnapshot,
    app: &TerminalApp,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let obs_connected = app.obs_error.is_none() && app.obs_status.is_some();
    let obs_indicator = if obs_connected { "<->" } else { "> <" };
    let obs_color = if obs_connected {
        STATUS_GREEN
    } else {
        STATUS_RED
    };
    let microphone_on = app.obs_error.is_none()
        && app
            .obs_status
            .as_ref()
            .is_some_and(|status| status.microphone == crate::obs::MicrophoneState::Unmuted);
    let (microphone_indicator, microphone_color) = if microphone_on {
        ("◉", STATUS_RED)
    } else {
        ("○ ", palette.rank)
    };
    let devices_width = UnicodeWidthStr::width("OBS ")
        + UnicodeWidthStr::width(obs_indicator)
        + UnicodeWidthStr::width("  MIC ")
        + UnicodeWidthStr::width(microphone_indicator);
    let broadcaster = fit_display_width(
        &format!(" @ {} ", room.broadcaster_name),
        usize::from(width).saturating_sub(devices_width),
    );
    let gap = usize::from(width)
        .saturating_sub(UnicodeWidthStr::width(broadcaster.as_str()) + devices_width);
    let neutral = Style::default()
        .fg(palette.content)
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::with_capacity(6);
    if !broadcaster.is_empty() {
        spans.push(status_span(broadcaster, STATUS_GREEN, palette));
    }
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend([
        Span::styled("OBS ", neutral),
        Span::styled(
            obs_indicator,
            Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  MIC ", neutral),
        Span::styled(
            microphone_indicator,
            Style::default()
                .fg(microphone_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    Line::from(spans)
}

fn technical_status_lines(app: &TerminalApp, palette: Palette, width: u16) -> Vec<Line<'static>> {
    let Some(room) = app.room.as_ref() else {
        return vec![Line::from(status_span(
            " ◌ ROOM ".into(),
            STATUS_ORANGE,
            palette,
        ))];
    };
    vec![
        primary_status_line(room, app, palette, width),
        secondary_status_line(room, app, palette, width),
    ]
}
fn application_notice_lines(app: &TerminalApp, palette: Palette, width: u16) -> Vec<Line<'static>> {
    if app.notice.is_empty() {
        return Vec::new();
    }
    wrap_styled_spans(
        vec![Span::styled(
            format!("⚠ {}", app.notice),
            Style::default().fg(palette.warning),
        )],
        width,
        Alignment::Left,
    )
}

fn draw_events(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut TerminalApp,
    palette: Palette,
    title: &str,
) {
    let displayed_events = display_events(&app.session.recent_events).collect::<Vec<_>>();
    let selection_anchor = app
        .selection_active
        .then(|| app.session.recent_events.get(app.selected))
        .flatten();
    let browse_anchor = selection_anchor.or_else(|| {
        (app.scroll_offset > 0)
            .then(|| app.session.recent_events.get(app.scroll_offset))
            .flatten()
    });
    let selection_target = selection_anchor.and_then(|selected| {
        displayed_events
            .iter()
            .position(|event| event.id == selected.id)
    });
    let browse_target = browse_anchor.and_then(|selected| {
        displayed_events
            .iter()
            .position(|event| event.id == selected.id)
    });
    let title = if browse_target.is_some() {
        format!(
            "{title} · 浏览历史 · {} 条新 · End 返回实时",
            app.unread_live_count
        )
    } else {
        title.to_owned()
    };
    let block = rounded_block(&title, palette);
    let inner = block.inner(area);
    let event_texts = displayed_events
        .into_iter()
        .map(|event| event_lines(event, app, palette, inner.width))
        .collect::<Vec<_>>();
    let mut remaining_height = usize::from(inner.height);
    let mut visible_events = 0;
    for text in event_texts.iter().rev() {
        let height = text.height();
        if visible_events > 0 && height > remaining_height {
            break;
        }
        remaining_height = remaining_height.saturating_sub(height);
        visible_events += 1;
        if remaining_height == 0 {
            break;
        }
    }
    app.page_event_count = visible_events.max(1);
    let items = event_texts
        .into_iter()
        .map(ListItem::new)
        .collect::<Vec<_>>();

    let scroll_target = browse_target.or_else(|| (!items.is_empty()).then(|| items.len() - 1));
    let used_height = items.iter().fold(0_u16, |height, item| {
        height.saturating_add(item.height() as u16)
    });
    frame.render_widget(block, area);
    let visible_height = used_height.min(inner.height);
    let list_area = Rect {
        y: inner.y + inner.height.saturating_sub(visible_height),
        height: visible_height,
        ..inner
    };
    let list = List::new(items).highlight_style(if selection_target.is_some() {
        Style::default()
            .fg(palette.info)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    });
    let mut state = ListState::default().with_selected(scroll_target);
    frame.render_stateful_widget(list, list_area, &mut state);
}

fn event_marker(kind: DanmuEventKind) -> &'static str {
    match kind {
        DanmuEventKind::Danmu => "•",
        DanmuEventKind::Gift => "◆",
        DanmuEventKind::GuardEvent => "♛",
        DanmuEventKind::Superchat => "▣",
        DanmuEventKind::Enter => "→",
        DanmuEventKind::Like => "♥",
        DanmuEventKind::Follow => "+",
        DanmuEventKind::Share => "↗",
        DanmuEventKind::Pk => "⚔",
        DanmuEventKind::Lottery => "✦",
        DanmuEventKind::Moderation => "!",
        DanmuEventKind::RoomStatus => "◉",
        DanmuEventKind::System => "·",
    }
}

fn event_lines(
    event: &DanmuEvent,
    app: &TerminalApp,
    palette: Palette,
    width: u16,
) -> Text<'static> {
    let broadcaster = app
        .room
        .as_ref()
        .is_some_and(|room| event.author_id.as_deref() == Some(room.broadcaster_id.as_str()));
    let mut metadata = Vec::new();
    if app.show_time {
        metadata.push(Span::styled(
            event
                .timestamp
                .with_timezone(&Local)
                .format("%H:%M ")
                .to_string(),
            Style::default().fg(palette.time),
        ));
    }
    if event.kind != DanmuEventKind::Danmu {
        let color = event_color(event.kind, palette);
        metadata.push(Span::styled(
            format!(" {} {} ", event_marker(event.kind), kind_label(event.kind)),
            Style::default()
                .fg(palette.background)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        ));
        metadata.push(Span::raw(" "));
    }
    if app.show_name
        && let Some(name) = event.username.as_deref()
    {
        let name = clean_platform_markup(name);
        if broadcaster {
            metadata.push(Span::styled(
                "♚ ",
                Style::default()
                    .fg(palette.rank)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        metadata.push(Span::styled(
            name,
            Style::default()
                .fg(if broadcaster {
                    palette.host
                } else {
                    palette.name
                })
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.show_name && event.kind == DanmuEventKind::Danmu {
        metadata.push(Span::styled(
            kind_label(event.kind),
            Style::default()
                .fg(palette.name)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let featured = app
        .session
        .featured_event
        .as_ref()
        .is_some_and(|item| item.id == event.id);
    let content = Span::styled(
        visible_event_content(event),
        Style::default()
            .fg(event_color(event.kind, palette))
            .add_modifier(if featured {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
    );
    let alignment = if app.layout_chat && broadcaster {
        Alignment::Right
    } else {
        Alignment::Left
    };
    if app.layout_chat || !app.config.single_line {
        let mut lines = wrap_styled_spans(metadata, width, alignment);
        lines.extend(wrap_styled_spans(vec![content], width, alignment));
        if app.layout_chat {
            lines.push(Line::raw(""));
        }
        Text::from(lines)
    } else {
        if !metadata.is_empty() {
            metadata.push(Span::raw("  "));
        }
        metadata.push(content);
        Text::from(wrap_styled_spans(metadata, width, Alignment::Left))
    }
}

fn wrap_styled_spans(
    spans: Vec<Span<'static>>,
    width: u16,
    alignment: Alignment,
) -> Vec<Line<'static>> {
    if spans.is_empty() {
        return Vec::new();
    }
    let width = usize::from(width).max(1);
    let mut lines = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0;
    for span in spans {
        let style = span.style;
        let mut segment = String::new();
        for grapheme in span.content.graphemes(true) {
            if grapheme == "\n" {
                if !segment.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut segment), style));
                }
                lines.push(Line::from(std::mem::take(&mut current_spans)).alignment(alignment));
                current_width = 0;
                continue;
            }
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if current_width > 0 && current_width + grapheme_width > width {
                if !segment.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut segment), style));
                }
                lines.push(Line::from(std::mem::take(&mut current_spans)).alignment(alignment));
                current_width = 0;
            }
            segment.push_str(grapheme);
            current_width += grapheme_width;
        }
        if !segment.is_empty() {
            current_spans.push(Span::styled(segment, style));
        }
    }
    if !current_spans.is_empty() || lines.is_empty() {
        lines.push(Line::from(current_spans).alignment(alignment));
    }
    lines
}

fn clean_platform_markup(value: &str) -> String {
    value.replace("<%", "").replace("%>", "")
}

fn visible_event_content(event: &DanmuEvent) -> String {
    let content = clean_platform_markup(&map_event_emotes(event));
    if event.kind != DanmuEventKind::Danmu
        && let Some(username) = event.username.as_deref()
    {
        let username = clean_platform_markup(username);
        if let Some(remainder) = content.strip_prefix(&username) {
            let remainder = remainder.trim();
            return if remainder.is_empty() {
                kind_label(event.kind).into()
            } else {
                remainder.to_string()
            };
        }
    }
    content
}

fn visible_input(app: &TerminalApp) -> String {
    if app.secret_mode {
        "•".repeat(grapheme_count(&app.input))
    } else {
        app.input.to_string()
    }
}

fn connection_badge(app: &TerminalApp) -> String {
    if app.connection.starts_with("已连接") {
        app.last_realtime_at
            .as_ref()
            .map(|timestamp| format!("● 实时 {}", timestamp.format("%H:%M:%S")))
            .unwrap_or_else(|| "● 实时".into())
    } else if app.connection.contains("重连") {
        "◌ 重连中".into()
    } else {
        "○ 连接中".into()
    }
}

fn color_luma(color: Color) -> Option<u32> {
    match color {
        Color::Rgb(red, green, blue) => {
            Some((299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue)) / 1000)
        }
        _ => None,
    }
}

fn contrast_foreground(background: Color, palette: Palette) -> Color {
    let Some(background_luma) = color_luma(background) else {
        return palette.content;
    };
    let content_contrast =
        color_luma(palette.content).map_or(0, |luma| luma.abs_diff(background_luma));
    let canvas_contrast =
        color_luma(palette.background).map_or(0, |luma| luma.abs_diff(background_luma));
    if content_contrast >= canvas_contrast {
        palette.content
    } else {
        palette.background
    }
}
fn powerline_title(segments: Vec<(String, Color)>) -> Line<'static> {
    Line::from(
        segments
            .into_iter()
            .map(|(label, foreground)| {
                Span::styled(
                    format!(" {label} "),
                    Style::default()
                        .fg(foreground)
                        .bg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn delivery_status_title(app: &TerminalApp, palette: Palette) -> Line<'static> {
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let (label, color) = match app.delivery_status {
        DeliveryStatus::Idle => ("↑", palette.success),
        DeliveryStatus::Sending | DeliveryStatus::AwaitingEcho | DeliveryStatus::Verifying => (
            SPINNER[app.animation_tick as usize % SPINNER.len()],
            palette.info,
        ),
        DeliveryStatus::Delivered => ("✓", palette.success),
        DeliveryStatus::Uncertain => ("?", palette.warning),
        DeliveryStatus::Failed => ("×", palette.warning),
    };
    Line::from(Span::styled(
        format!(" {label} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn input_business_title(app: &TerminalApp, palette: Palette, width: u16) -> Line<'static> {
    let watched = app
        .watched
        .map_or_else(|| "--".into(), |value| value.to_string());
    let likes = app
        .likes
        .map_or_else(|| "--".into(), |value| value.to_string());
    let online = app
        .online_viewers
        .map_or_else(|| "--".into(), |value| value.to_string());
    let mut segments = Vec::with_capacity(4);
    segments.push((format!("◉ {watched}"), palette.rank));
    if width >= 24 {
        segments.push((format!("♥ {likes}"), palette.warning));
    }
    if width >= 32 {
        segments.push((format!("▤ {}", app.live_danmu_count), palette.name));
    }
    segments.push((format!("● {online}"), palette.success));
    powerline_title(segments)
}

fn line_display_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn input_top_line(
    left: Line<'static>,
    right: Line<'static>,
    width: u16,
    palette: Palette,
    microphone: Option<(MicrophoneState, Option<MicrophoneLevel>)>,
) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let fill_width = usize::from(width)
        .saturating_sub(6)
        .saturating_sub(line_display_width(&left))
        .saturating_sub(line_display_width(&right));
    let meter =
        microphone.and_then(|(state, level)| microphone_meter(state, level, fill_width, palette));
    let meter_width = meter.as_ref().map_or(0, |meter| meter.width);
    let remaining = fill_width.saturating_sub(meter_width);
    let before_meter = remaining / 2;
    let after_meter = remaining - before_meter;
    let meter_span_count = meter.as_ref().map_or(0, |meter| meter.spans.len());
    let mut spans = Vec::with_capacity(left.spans.len() + right.spans.len() + meter_span_count + 4);
    spans.push(Span::styled("╭──", border_style));
    spans.extend(left.spans);
    spans.push(Span::styled("─".repeat(before_meter), border_style));
    if let Some(meter) = meter {
        spans.extend(meter.spans);
    }
    spans.push(Span::styled("─".repeat(after_meter), border_style));
    spans.extend(right.spans);
    spans.push(Span::styled("──╮", border_style));
    Line::from(spans)
}

fn input_prompt_line(app: &TerminalApp, width: u16, palette: Palette) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let input = visible_input(app);
    let input_width = UnicodeWidthStr::width(input.as_str());
    let fill_width = usize::from(width)
        .saturating_sub(5)
        .saturating_sub(input_width);
    Line::from(vec![
        Span::styled("╰─ ", border_style),
        Span::styled(input, Style::default().fg(palette.content)),
        Span::raw(" ".repeat(fill_width)),
        Span::styled("─╯", border_style),
    ])
}

const MAX_INPUT_CONTENT_LINES: usize = 4;

fn input_content_width(width: u16) -> usize {
    usize::from(width.saturating_sub(5)).max(1)
}

fn wrapped_input_lines(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for grapheme in value.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if line_width > 0 && line_width + grapheme_width > width {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
        line.push_str(grapheme);
        line_width += grapheme_width;
        if line_width >= width {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
    }
    lines.push(line);
    lines
}

fn wrapped_cursor_position(value: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut row = 0;
    let mut column = 0;
    for grapheme in value.graphemes(true).take(cursor) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if column > 0 && column + grapheme_width > width {
            row += 1;
            column = 0;
        }
        column += grapheme_width;
        if column >= width {
            row += 1;
            column = 0;
        }
    }
    (row, column)
}

fn input_area_height(app: &TerminalApp, width: u16) -> u16 {
    let lines = wrapped_input_lines(&visible_input(app), input_content_width(width)).len();
    u16::try_from(lines.min(MAX_INPUT_CONTENT_LINES)).unwrap_or(1) + 1
}

fn input_content_line(content: &str, width: u16, palette: Palette) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let content_width = input_content_width(width);
    let fill_width = content_width.saturating_sub(UnicodeWidthStr::width(content));
    Line::from(vec![
        Span::styled("│  ", border_style),
        Span::styled(content.to_string(), Style::default().fg(palette.content)),
        Span::raw(" ".repeat(fill_width)),
        Span::styled(" │", border_style),
    ])
}

fn input_final_line(content: &str, width: u16, palette: Palette) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let content_width = input_content_width(width);
    let fill_width = content_width.saturating_sub(UnicodeWidthStr::width(content));
    Line::from(vec![
        Span::styled("╰─ ", border_style),
        Span::styled(content.to_string(), Style::default().fg(palette.content)),
        Span::raw(" ".repeat(fill_width)),
        Span::styled("─╯", border_style),
    ])
}

fn draw_input(frame: &mut ratatui::Frame, area: Rect, app: &TerminalApp, palette: Palette) {
    let (left, right) = if app.secret_mode {
        (
            powerline_title(vec![(
                "OBS WebSocket 新密码 · Esc 取消".into(),
                palette.warning,
            )]),
            Line::default(),
        )
    } else if app.awaiting_stop_confirmation {
        (
            powerline_title(vec![(
                "停止推流确认 · /obs confirm 或 /obs cancel".into(),
                palette.warning,
            )]),
            Line::default(),
        )
    } else {
        (
            delivery_status_title(app, palette),
            input_business_title(app, palette, area.width),
        )
    };

    let meter_context =
        if !app.secret_mode && !app.awaiting_stop_confirmation && app.obs_error.is_none() {
            app.obs_status
                .as_ref()
                .map(|status| (status.microphone, app.microphone_level))
        } else {
            None
        };

    let visible = visible_input(app);
    if area.width < 6 {
        frame.render_widget(
            Paragraph::new(vec![
                input_top_line(left, right, area.width, palette, meter_context),
                input_prompt_line(app, area.width, palette),
            ]),
            area,
        );
        let before = visible
            .graphemes(true)
            .take(app.input.cursor())
            .collect::<String>();
        let width = UnicodeWidthStr::width(before.as_str()) as u16;
        frame.set_cursor_position((
            area.x + 3 + width.min(area.width.saturating_sub(4)),
            area.y + 1,
        ));
        return;
    }

    let content_width = input_content_width(area.width);
    let wrapped = wrapped_input_lines(&visible, content_width);
    let (cursor_row, cursor_column) =
        wrapped_cursor_position(&visible, app.input.cursor(), content_width);
    let content_rows = usize::from(area.height.saturating_sub(1)).max(1);
    let viewport_start = cursor_row
        .saturating_sub(content_rows.saturating_sub(1))
        .min(wrapped.len().saturating_sub(content_rows));
    let mut lines = Vec::with_capacity(content_rows + 1);
    lines.push(input_top_line(
        left,
        right,
        area.width,
        palette,
        meter_context,
    ));
    for row in 0..content_rows {
        let content = wrapped
            .get(viewport_start + row)
            .map(String::as_str)
            .unwrap_or_default();
        if row + 1 == content_rows {
            lines.push(input_final_line(content, area.width, palette));
        } else {
            lines.push(input_content_line(content, area.width, palette));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
    frame.set_cursor_position((
        area.x + 3 + u16::try_from(cursor_column).unwrap_or(u16::MAX),
        area.y + 1 + u16::try_from(cursor_row.saturating_sub(viewport_start)).unwrap_or(0),
    ));
}

fn map_event_emotes(event: &DanmuEvent) -> String {
    event
        .emotes
        .iter()
        .fold(map_bili_emotes(&event.content), |content, emote| {
            let fallback = if emote.fallback.is_empty() {
                "🙂"
            } else {
                emote.fallback.as_str()
            };
            content.replace(&emote.text, fallback)
        })
}

fn map_bili_emotes(content: &str) -> String {
    [
        ("[dog]", "🐶"),
        ("[doge]", "🐕"),
        ("[妙啊]", "👍"),
        ("[笑哭]", "😂"),
        ("[辣眼睛]", "🙈"),
        ("[吃瓜]", "🍉"),
        ("[鼓掌]", "👏"),
        ("[赞]", "👍"),
        ("[爱心]", "❤️"),
        ("[捂脸]", "🤦"),
        ("[呲牙]", "😁"),
        ("[大哭]", "😭"),
        ("[花]", "🌸"),
        ("[委屈]", "🥺"),
        ("[微笑]", "🙂"),
        ("[滑稽]", "😏"),
        ("[疑惑]", "🤔"),
        ("[惊讶]", "😮"),
        ("[害羞]", "😊"),
        ("[生气]", "😠"),
        ("[无语]", "😑"),
        ("[口罩]", "😷"),
        ("[星星眼]", "🤩"),
        ("[OK]", "👌"),
    ]
    .into_iter()
    .fold(content.to_string(), |value, (code, emoji)| {
        value.replace(code, emoji)
    })
}

fn event_color(kind: DanmuEventKind, palette: Palette) -> Color {
    match kind {
        DanmuEventKind::Danmu => palette.content,
        DanmuEventKind::Gift => palette.rank,
        DanmuEventKind::GuardEvent => palette.success,
        DanmuEventKind::Superchat => palette.warning,
        DanmuEventKind::Enter | DanmuEventKind::RoomStatus => palette.info,
        DanmuEventKind::Like | DanmuEventKind::Moderation => palette.warning,
        DanmuEventKind::Follow => palette.success,
        DanmuEventKind::Share => palette.name,
        DanmuEventKind::Pk | DanmuEventKind::Lottery => palette.rank,
        DanmuEventKind::System => palette.time,
    }
}
fn grapheme_count(value: &str) -> usize {
    value.graphemes(true).count()
}
fn sanitize_input(value: String) -> String {
    value
        .chars()
        .filter(|character| {
            !matches!(
                *character,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
            )
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub async fn interactive_login(account: AccountClient) -> Result<()> {
    let challenge = account.login_challenge().await?;
    println!("请使用哔哩哔哩客户端扫码登录：\n");
    for line in qr_lines(challenge.url.as_str())? {
        println!("{line}");
    }
    println!("\n二维码地址：{}", challenge.url);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match account.poll_login(&challenge.key).await? {
            LoginPoll::Waiting => print!("."),
            LoginPoll::Scanned => print!(" 已扫码，等待确认"),
            LoginPoll::Expired => {
                return Err(anyhow!("登录二维码已过期，请重新运行 danmu --login"));
            }
            LoginPoll::SignedIn(AccountStatus::SignedIn { display_name, .. }) => {
                println!("\n登录成功：{display_name}");
                return Ok(());
            }
            LoginPoll::SignedIn(AccountStatus::SignedOut) => return Err(anyhow!("登录态验证失败")),
        }
        use std::io::Write;
        io::stdout().flush()?;
    }
}

pub async fn configure_obs(path: &std::path::Path) -> Result<()> {
    let mut configuration = crate::obs::ObsConfiguration::load(path)?;
    println!("OBS WebSocket 主机 [{}]：", configuration.host);
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if !input.trim().is_empty() {
        configuration.host = input.trim().into();
    }
    println!("OBS WebSocket 端口 [{}]：", configuration.port);
    input.clear();
    io::stdin().read_line(&mut input)?;
    if let Ok(port) = input.trim().parse::<u16>() {
        configuration.port = port;
    }
    println!("默认直播场景 [{}]：", configuration.default_live_scene);
    input.clear();
    io::stdin().read_line(&mut input)?;
    if !input.trim().is_empty() {
        configuration.default_live_scene = input.trim().into();
    }
    println!("麦克风输入 [{}]：", configuration.microphone_input_name);
    input.clear();
    io::stdin().read_line(&mut input)?;
    if !input.trim().is_empty() {
        configuration.microphone_input_name = input.trim().into();
    }
    let password = rpassword::prompt_password("OBS WebSocket 密码（留空保持不变）：")?;
    if !password.is_empty() {
        ObsController::set_password(&password)?;
    }
    configuration.save(path)?;
    println!("OBS 配置已保存：{}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::ObsConfiguration;
    use ratatui::backend::TestBackend;
    fn test_app(temp: &tempfile::TempDir, session: DanmuSession) -> TerminalApp {
        let themes = crate::theme::ThemeCatalog::load(temp.path().join("themes.json")).unwrap();
        let theme_name = themes.selected().to_owned();
        let config = TerminalConfig {
            room_id: "1".into(),
            single_line: true,
            chat_layout: false,
            show_time: false,
            show_name: true,
            palette: Palette::default(),
            theme_name,
            themes,
        };
        TerminalApp {
            config,
            client: BilibiliClient::new(temp.path().join("account.json")).unwrap(),
            account: AccountClient::new(temp.path().join("account.json")).unwrap(),
            send_queue: Arc::new(tokio::sync::Mutex::new(())),
            obs: ObsController::new(ObsConfiguration::default(), temp.path().join("obs.json")),
            journal: SessionJournal::new(temp.path().join("sessions")),
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
            notice: String::new(),
            notice_deadline: None,
            delivery_status: DeliveryStatus::Idle,
            delivery_status_deadline: None,
            layout_chat: false,
            show_name: true,
            show_time: false,
            account_status: AccountStatus::SignedOut,
            awaiting_stop_confirmation: false,
            login_qr: None,
            secret_mode: false,
            selection_active: false,
            page_event_count: 1,
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

    #[test]
    fn input_header_renders_live_meter_and_hides_it_when_obs_disconnects() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.obs_status = Some(ObsStatus {
            current_scene: "直播".into(),
            stream: crate::obs::StreamState::Live,
            microphone: MicrophoneState::Unmuted,
            compatibility_warning: None,
        });
        app.microphone_level = Some(MicrophoneLevel { peak_db: -12.0 });
        let mut terminal = Terminal::new(TestBackend::new(120, 2)).unwrap();

        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, app.config.palette))
            .unwrap();
        let connected_header = (0..120)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        assert!(connected_header.contains("MIC"));
        assert!(connected_header.contains("-12"));

        app.obs_error = Some("OBS 未连接".into());
        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, app.config.palette))
            .unwrap();
        let disconnected_header = (0..120)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        assert!(!disconnected_header.contains("MIC"));
    }

    #[test]
    fn sanitizes_ime_format_characters() {
        assert_eq!(sanitize_input("你\u{200d}好\u{feff}".into()), "你好");
    }

    #[test]
    fn maps_known_emotes_and_preserves_unknown_codes() {
        assert_eq!(map_bili_emotes("[dog] [未知]"), "🐶 [未知]");
    }

    #[test]
    fn renders_scannable_qr_rows() {
        let rows = qr_lines("https://example.com/login").unwrap();
        assert!(rows.len() > 10);
        assert!(
            rows.iter()
                .any(|row| row.contains('█') || row.contains('▀') || row.contains('▄'))
        );
    }
    #[test]
    fn tui_login_uses_a_smaller_qr_encoding_than_standalone_login() {
        let payload = "x".repeat(131);
        let compact = compact_qr_lines(&payload).unwrap();
        let standard = qr_lines(&payload).unwrap();
        let compact_width = compact
            .iter()
            .map(|row| UnicodeWidthStr::width(row.as_str()))
            .max()
            .unwrap();
        let standard_width = standard
            .iter()
            .map(|row| UnicodeWidthStr::width(row.as_str()))
            .max()
            .unwrap();

        assert!(compact_width <= 45, "{compact_width}");
        assert!(compact.len() <= 23, "{}", compact.len());
        assert!(compact_width < standard_width);
        assert!(compact.len() < standard.len());
    }

    #[test]
    fn qr_overlay_degrades_instead_of_clipping_on_small_terminals() {
        let temp = tempfile::tempdir().unwrap();
        let app = test_app(&temp, DanmuSession::new("1"));
        let rows = qr_lines(&"x".repeat(131)).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(44, 20)).unwrap();

        terminal
            .draw(|frame| draw_qr(frame, frame.area(), &rows, app.config.palette))
            .unwrap();

        let mut rendered = String::new();
        for y in 0..20 {
            for x in 0..44 {
                rendered.push_str(terminal.backend().buffer()[(x, y)].symbol());
            }
        }
        assert!(
            rendered.contains('终') && rendered.contains('足'),
            "{rendered}"
        );
        assert!(!rendered.contains('█'), "{rendered}");
    }
    #[test]
    fn displays_oldest_event_at_top_and_newest_at_bottom() {
        let mut older = DanmuEvent::new(DanmuEventKind::Danmu, "旧消息");
        older.id = "older".into();
        let mut newer = DanmuEvent::new(DanmuEventKind::Danmu, "新消息");
        newer.id = "newer".into();
        let events = vec![newer, older];
        let ids = display_events(&events)
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["older", "newer"]);
    }

    #[test]
    fn application_notices_render_below_the_danmu_frame() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        session.ingest(DanmuEvent::new(DanmuEventKind::Danmu, "真实弹幕"));
        let mut app = test_app(&temp, session);
        app.set_notice("测试错误提示", NoticeLevel::Error);
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let frame_top = lines
            .iter()
            .position(|line| line.contains("Ghost Stage"))
            .unwrap();
        let frame_bottom = lines
            .iter()
            .enumerate()
            .skip(frame_top + 1)
            .find_map(|(row, line)| line.contains('╰').then_some(row))
            .unwrap();
        let notice_row = lines
            .iter()
            .position(|line| line.replace(' ', "").contains("测试错误提示"))
            .unwrap_or_else(|| panic!("notice missing from rendered rows: {lines:#?}"));

        assert!(notice_row > frame_bottom);
        assert!(!lines[notice_row].contains('│'));
    }

    #[test]
    fn layout_separates_realtime_technical_and_business_status() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        let mut older = DanmuEvent::new(DanmuEventKind::Danmu, "旧消息");
        older.username = Some("甲".into());
        session.ingest(older);
        let mut newer = DanmuEvent::new(DanmuEventKind::Danmu, "新消息");
        newer.username = Some("乙".into());
        session.ingest(newer);
        let mut app = test_app(&temp, session);
        app.handle_ui_event(UiEvent::RoomSnapshot(RoomSnapshot {
            room_id: "1".into(),
            broadcaster_id: "42".into(),
            broadcaster_name: "停车拾穗".into(),
            title: "测试直播标题".into(),
            area: "知识".into(),
            live_started_at: Some(Utc::now() - chrono::Duration::hours(1)),
            live_status: RoomLiveStatus::Live,
        }));
        app.live_danmu_count = 7;
        app.handle_client_event(BilibiliClientEvent::Watched(123));
        app.handle_client_event(BilibiliClientEvent::Likes(321));
        app.handle_ui_event(UiEvent::OnlineViewers(Ok(Some(11))));
        app.account_status = AccountStatus::SignedIn {
            display_name: "主播".into(),
            user_id: "42".into(),
        };
        app.connection = "已连接".into();
        app.handle_ui_event(UiEvent::ObsStatus(Ok(ObsStatus {
            current_scene: "直播".into(),
            stream: crate::obs::StreamState::Live,
            microphone: crate::obs::MicrophoneState::Unmuted,
            compatibility_warning: None,
        })));
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines = (0..buffer.area.height)
            .map(|y| {
                let mut line = String::new();
                for x in 0..buffer.area.width {
                    line.push_str(buffer[(x, y)].symbol());
                }
                line
            })
            .collect::<Vec<_>>();
        let normalized = lines
            .iter()
            .map(|line| line.replace(' ', ""))
            .collect::<Vec<_>>();
        let surface = normalized.join(
            "
",
        );
        let live_row = normalized
            .iter()
            .position(|line| line.contains("●LIVE"))
            .unwrap();
        let details_row = normalized
            .iter()
            .position(|line| line.contains("OBS<->"))
            .unwrap();
        let business_row = normalized
            .iter()
            .position(|line| line.contains("◉123"))
            .unwrap();
        let live = &normalized[live_row];
        let details = &normalized[details_row];
        let business = &normalized[business_row];
        assert_eq!(live_row, 0);
        assert_eq!(details_row, 1);
        assert!(live.contains("●LIVE测试直播标题"));
        assert!(live.ends_with(|character: char| character.is_ascii_digit()));
        assert!(live.contains("◷"));
        assert!(!live.contains("@停车拾穗"));
        assert!(details.contains("@停车拾穗"));
        assert!(details.contains("OBS<->"));
        assert!(details.contains("MIC◉"));
        assert!(!details.contains("知识"));
        assert!(!details.contains("测试直播标题"));
        assert!(business.starts_with("╭──↑"));
        assert!(business.contains("◉123"));
        assert!(business.contains("♥321"));
        assert!(business.contains("▤7"));
        assert!(business.contains("●11"));
        assert!(business.ends_with("●11──╮"));
        assert!(business.find("♥321").unwrap() > business.find("◉123").unwrap());
        assert!(business.find("▤7").unwrap() > business.find("♥321").unwrap());
        assert!(business.find("●11").unwrap() > business.find("▤7").unwrap());
        assert!(!business.contains("看过"));
        assert!(!business.contains("点赞"));
        assert!(!business.contains("弹幕"));
        assert!(!business.contains("在线"));
        assert!(!business.contains("测试直播标题"));
        assert!(!business.contains("知识"));
        assert!(!business.contains("LIVE"));
        assert!(!business.contains("OBS"));
        assert!(!business.contains("技术"));
        assert!(surface.contains("╭──"));
        assert!(surface.contains("╰─"));
        assert!(surface.contains("─╯"));
        assert!(!surface.contains("拾穗弹幕台"));
        assert!(!surface.contains("B站"));

        let technical_lines = technical_status_lines(&app, app.config.palette, 120);
        assert_eq!(line_display_width(&technical_lines[0]), 120);
        assert!(technical_lines[0].spans[0].content.contains("LIVE"));
        assert!(
            technical_lines[0]
                .spans
                .last()
                .unwrap()
                .content
                .contains("◷")
        );
        assert_eq!(technical_lines.len(), 2);
        let title_span = technical_lines[0]
            .spans
            .iter()
            .find(|span| span.content == "测试直播标题")
            .unwrap();
        assert_eq!(title_span.style.fg, Some(app.config.palette.rank));
        assert_eq!(title_span.style.bg, None);
        assert!(
            technical_lines[1]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .ends_with("OBS <->  MIC ◉")
        );
        let obs_indicator = technical_lines[1]
            .spans
            .iter()
            .find(|span| span.content == "<->")
            .unwrap();
        assert_eq!(obs_indicator.style.fg, Some(STATUS_GREEN));
        assert_eq!(obs_indicator.style.bg, None);
        let mic_indicator = technical_lines[1]
            .spans
            .iter()
            .find(|span| span.content == "◉")
            .unwrap();
        assert_eq!(mic_indicator.style.fg, Some(STATUS_RED));
        assert_eq!(mic_indicator.style.bg, None);
        app.handle_ui_event(UiEvent::ObsStatus(Err("offline".into())));
        let disconnected_lines = technical_status_lines(&app, app.config.palette, 120);
        let disconnected_obs = disconnected_lines[1]
            .spans
            .iter()
            .find(|span| span.content == "> <")
            .unwrap();
        assert_eq!(disconnected_obs.style.fg, Some(STATUS_RED));
        assert_eq!(disconnected_obs.style.bg, None);
        let muted_mic = disconnected_lines[1]
            .spans
            .iter()
            .find(|span| span.content == "○ ")
            .unwrap();
        assert_eq!(muted_mic.style.fg, Some(app.config.palette.rank));
        assert_eq!(muted_mic.style.bg, None);
        assert!(
            disconnected_lines[1]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .ends_with("MIC ○ ")
        );
        let colored_spans = technical_lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.bg.is_some())
            .collect::<Vec<_>>();
        let segment_backgrounds = colored_spans
            .iter()
            .map(|span| span.style.bg.unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert!(segment_backgrounds.contains(&STATUS_RED));
        assert!(segment_backgrounds.contains(&STATUS_GREEN));
        assert!(segment_backgrounds.contains(&STATUS_BLUE));
        for span in colored_spans {
            let background = span.style.bg.unwrap();
            assert_eq!(
                span.style.fg,
                Some(contrast_foreground(background, app.config.palette))
            );
        }
        assert_eq!(technical_lines[0].spans[0].style.bg, Some(STATUS_RED));
        app.animation_tick = 2;
        let dim_live_background = technical_status_lines(&app, app.config.palette, 120)[0].spans[0]
            .style
            .bg;
        assert_eq!(dim_live_background, Some(STATUS_RED_DIM));

        let narrow_lines = technical_status_lines(&app, app.config.palette, 32);
        assert_eq!(narrow_lines.len(), 2);
        assert_eq!(line_display_width(&narrow_lines[0]), 32);
        let narrow_text = narrow_lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!narrow_text.contains("知识"));

        let room_title = input_business_title(&app, app.config.palette, 120);
        let mut foregrounds = std::collections::HashSet::new();
        for span in &room_title.spans {
            let background = span.style.bg.expect("数据分段必须有背景色");
            let foreground = span.style.fg.expect("数据分段必须有前景色");
            assert_eq!(background, Color::Black);
            assert_ne!(foreground, Color::Black);
            foregrounds.insert(foreground);
        }
        assert!(foregrounds.len() >= 3);
        let delivery_title = delivery_status_title(&app, app.config.palette);
        assert_eq!(
            delivery_title.spans[0].style.fg,
            Some(app.config.palette.success)
        );
        assert_eq!(delivery_title.spans[0].content, " ↑ ");
        assert_eq!(delivery_title.spans[0].style.bg, None);
        let compact = input_top_line(
            delivery_status_title(&app, app.config.palette),
            input_business_title(&app, app.config.palette, 40),
            40,
            app.config.palette,
            None,
        );
        let compact_text = compact
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(line_display_width(&compact), 40);
        assert!(compact_text.contains("◉ 123"));
        assert!(compact_text.contains("♥ 321"));
        assert!(compact_text.contains("▤ 7"));
        assert!(!compact_text.contains("在线"));
        assert!(compact_text.contains("● 11"));
        assert!(lines.iter().any(|line| line.contains("╭ Ghost Stage ")));
        assert!(!surface.contains("Questions"));
        assert!(!surface.contains("待回答"));
        let older_row = normalized
            .iter()
            .position(|line| line.contains("旧消息"))
            .unwrap();
        let newer_row = normalized
            .iter()
            .position(|line| line.contains("新消息"))
            .unwrap();
        assert!(older_row < newer_row);
    }

    #[test]
    fn hides_enter_messages_after_five_seconds() {
        let mut expired = DanmuEvent::new(DanmuEventKind::Enter, "旧进场消息");
        expired.timestamp = Utc::now() - chrono::Duration::seconds(6);
        let current = DanmuEvent::new(DanmuEventKind::Danmu, "真实弹幕");
        let events = vec![current, expired];

        let displayed = display_events(&events).collect::<Vec<_>>();
        assert_eq!(displayed.len(), 1);
        assert_eq!(displayed[0].content, "真实弹幕");
    }

    #[test]
    fn hides_like_messages_after_three_seconds() {
        let mut expired = DanmuEvent::new(DanmuEventKind::Like, "旧点赞消息");
        expired.timestamp = Utc::now() - chrono::Duration::seconds(4);
        let mut current = DanmuEvent::new(DanmuEventKind::Like, "新点赞消息");
        current.timestamp = Utc::now() - chrono::Duration::seconds(2);
        let danmu = DanmuEvent::new(DanmuEventKind::Danmu, "真实弹幕");
        let events = vec![danmu, current, expired];

        let displayed = display_events(&events)
            .map(|event| event.content.as_str())
            .collect::<Vec<_>>();
        assert!(!displayed.contains(&"旧点赞消息"));
        assert!(displayed.contains(&"新点赞消息"));
        assert!(displayed.contains(&"真实弹幕"));
    }

    #[test]
    fn polled_likes_cannot_overwrite_a_newer_realtime_total() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_client_event(BilibiliClientEvent::Likes(300));
        app.handle_ui_event(UiEvent::Likes(Ok(Some(221))));

        assert_eq!(app.likes, Some(300));
    }

    #[test]
    fn event_badges_use_type_colors_and_strip_bilibili_markup() {
        let temp = tempfile::tempdir().unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Enter, "<%战区超人%> 来了");
        let app = test_app(&temp, DanmuSession::new("1"));
        let palette = app.config.palette;
        let rendered = event_lines(&event, &app, palette, 80);
        let line = &rendered.lines[0];
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let badge = line
            .spans
            .iter()
            .find(|span| span.content.contains("进场"))
            .unwrap();
        assert!(text.contains("→ 进场"));
        assert!(text.contains("战区超人 来了"));
        assert!(!text.contains("<%"));
        assert!(!text.contains("%>"));
        assert_eq!(badge.style.bg, Some(palette.info));
        assert_ne!(
            event_color(DanmuEventKind::Enter, palette),
            event_color(DanmuEventKind::Danmu, palette)
        );
        assert_ne!(
            event_color(DanmuEventKind::Gift, palette),
            event_color(DanmuEventKind::Superchat, palette)
        );
        assert_ne!(
            event_color(DanmuEventKind::Follow, palette),
            event_color(DanmuEventKind::Like, palette)
        );
    }

    #[test]
    fn broadcaster_name_has_identity_mark_and_pink_color() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.room = Some(RoomSnapshot {
            room_id: "1".into(),
            broadcaster_id: "42".into(),
            broadcaster_name: "停车拾穗".into(),
            title: "测试直播".into(),
            area: "知识".into(),
            live_started_at: None,
            live_status: RoomLiveStatus::Offline,
        });
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "主播消息");
        event.username = Some("停车拾穗".into());
        event.author_id = Some("42".into());
        let rendered = event_lines(&event, &app, app.config.palette, 80);
        let crown = rendered.lines[0]
            .spans
            .iter()
            .find(|span| span.content == "♚ ")
            .unwrap();
        let identity = rendered.lines[0]
            .spans
            .iter()
            .find(|span| span.content == "停车拾穗")
            .unwrap();
        assert_eq!(crown.style.fg, Some(app.config.palette.rank));
        assert_eq!(identity.style.fg, Some(app.config.palette.host));
    }

    #[tokio::test]
    async fn escape_and_selection_behavior_remain_intact() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "问题");
        event.username = Some("观众".into());
        session.ingest(event);
        let mut app = test_app(&temp, session);
        let (tx, _rx) = mpsc::channel(1);

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(!app.selection_active);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT), tx.clone())
            .await
            .unwrap();
        assert!(app.selection_active);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.input.is_empty());
        let quit_selection = app
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(!quit_selection);
        assert!(!app.selection_active);
        let quit_app = app
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(quit_app);
    }
    #[test]
    fn sent_message_accepts_masked_live_echo_and_restores_broadcaster_identity() {
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
    fn canonical_history_reconciles_masked_live_event_without_a_duplicate() {
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
    fn masked_live_echo_after_history_confirmation_is_not_inserted_twice() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let submitted_at = Utc::now();
        let delivery = PendingDelivery::new(
            "古天乐，神殿侠侣，应该看过吧".into(),
            "停车拾穗".into(),
            "42".into(),
            submitted_at,
        );
        let (confirmation_tx, mut confirmation_rx) = oneshot::channel();
        app.handle_ui_event(UiEvent::DeliveryStarted {
            delivery,
            confirmation: confirmation_tx,
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
    fn app_shows_its_own_live_echo_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let submitted_at = Utc::now();
        let delivery = PendingDelivery::new(
            "主播消息".into(),
            "拾穗数据".into(),
            "42".into(),
            submitted_at,
        );
        let (confirmation_tx, mut confirmation_rx) = oneshot::channel();
        app.handle_ui_event(UiEvent::DeliveryStarted {
            delivery,
            confirmation: confirmation_tx,
        });
        assert_eq!(app.delivery_status, DeliveryStatus::AwaitingEcho);
        app.animation_tick = 1;
        assert!(
            delivery_status_title(&app, app.config.palette)
                .spans
                .iter()
                .any(|span| span.content.contains("⠙"))
        );

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
        assert_eq!(app.live_danmu_count, 1);
        assert!(app.last_live_danmu_at.is_some());
        assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
        assert!(app.notice.is_empty());
    }

    #[test]
    fn segmented_send_tracks_multiple_unconfirmed_echoes_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
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
        app.handle_ui_event(UiEvent::DeliveryStarted {
            delivery: first,
            confirmation: first_tx,
        });
        app.handle_ui_event(UiEvent::DeliveryStarted {
            delivery: second,
            confirmation: second_tx,
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
        assert_eq!(app.delivery_status, DeliveryStatus::AwaitingEcho);

        let mut first_echo = DanmuEvent::new(DanmuEventKind::Danmu, "第一段");
        first_echo.timestamp = submitted_at + chrono::Duration::seconds(4);
        first_echo.username = Some("拾***据".into());
        first_echo.author_id = Some("0".into());
        app.ingest_event(first_echo);

        assert_eq!(first_rx.try_recv(), Ok(()));
        assert!(app.pending_deliveries.is_empty());
        assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
    }
    #[test]
    fn delivery_reminders_include_content_and_expire_once() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));

        app.handle_ui_event(UiEvent::DeliveryRejected {
            content: "请手动重发这条消息".into(),
            message: "发送失败：还没有 B 站登录态".into(),
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Failed);
        assert!(app.notice.contains("请手动重发这条消息"));
        assert!(
            delivery_status_title(&app, app.config.palette)
                .spans
                .iter()
                .any(|span| span.content.contains('×'))
        );
        let rejection_deadline = app.notice_deadline.expect("发送失败提醒应自动消失");
        app.expire_notice_at(rejection_deadline);
        assert!(app.notice.is_empty());
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);

        let delivery = PendingDelivery::new(
            "待确认消息".into(),
            "拾穗数据".into(),
            "42".into(),
            Utc::now(),
        );
        let delivery_id = delivery.id.clone();
        let (confirmation, _receiver) = oneshot::channel();
        app.handle_ui_event(UiEvent::DeliveryStarted {
            delivery,
            confirmation,
        });
        app.handle_ui_event(UiEvent::DeliveryHistory { events: Vec::new() });
        assert_eq!(app.delivery_status, DeliveryStatus::Verifying);
        app.handle_ui_event(UiEvent::DeliveryTimedOut {
            delivery_ids: vec![delivery_id],
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Uncertain);
        assert!(app.notice.contains("待确认消息"));
        assert!(app.notice.contains("已停止提醒"));
        assert!(
            delivery_status_title(&app, app.config.palette)
                .spans
                .iter()
                .any(|span| span.content.contains('?'))
        );
        let timeout_deadline = app.notice_deadline.expect("未确认提醒应自动消失");
        app.expire_notice_at(timeout_deadline);
        assert!(app.notice.is_empty());
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);
    }

    #[test]
    fn success_notice_expires_but_warning_remains() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let now = std::time::Instant::now();

        app.set_notice_at("已切换主题：chatroom", NoticeLevel::Success, now);
        app.expire_notice_at(now + Duration::from_millis(2_999));
        assert_eq!(app.notice, "已切换主题：chatroom");
        app.expire_notice_at(now + Duration::from_secs(3));
        assert!(app.notice.is_empty());
        app.set_delivery_status_at(DeliveryStatus::Delivered, now);
        app.expire_notice_at(now + Duration::from_millis(2_999));
        assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
        app.expire_notice_at(now + Duration::from_secs(3));
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);

        app.set_notice_at("停止推流有中断直播风险", NoticeLevel::Warning, now);
        app.expire_notice_at(now + Duration::from_secs(60));
        assert_eq!(app.notice, "停止推流有中断直播风险");
    }

    #[test]
    fn overflowing_live_feed_keeps_newest_event_visible() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..30 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("历史消息-{index:02}"));
            event.username = Some(format!("观众-{index:02}"));
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
        let mut latest = DanmuEvent::new(DanmuEventKind::Danmu, "最新实时弹幕");
        latest.origin = DanmuEventOrigin::Live;
        latest.username = Some("新观众".into());
        app.handle_client_event(BilibiliClientEvent::Danmu(latest));

        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let palette = app.config.palette;
        terminal
            .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace(' ', "");

        assert!(rendered.contains("最新实时弹幕"), "{rendered}");
        assert!(!rendered.contains("历史消息-00"), "{rendered}");
    }

    #[test]
    fn periodic_history_recovery_restores_a_message_missed_by_websocket() {
        let temp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut session = DanmuSession::new("1");
        let mut previous = DanmuEvent::new(DanmuEventKind::Danmu, "上一条消息");
        previous.id = "previous".into();
        previous.timestamp = now - chrono::Duration::seconds(10);
        session.ingest(previous);
        let mut app = test_app(&temp, session);
        let mut recovered = DanmuEvent::new(DanmuEventKind::Danmu, "做点小玩意");
        recovered.id = "bili-recovered".into();
        recovered.platform_event_id = Some("recovered".into());
        recovered.origin = DanmuEventOrigin::History;
        recovered.timestamp = now;
        recovered.username = Some("停车拾穗".into());

        app.handle_client_event(BilibiliClientEvent::Danmu(recovered.clone()));
        app.handle_client_event(BilibiliClientEvent::Danmu(recovered));

        assert_eq!(app.session.recent_events[0].content, "做点小玩意");
        assert_eq!(app.session.recent_events.len(), 2);
    }

    #[test]
    fn incoming_live_event_preserves_the_message_being_browsed() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..10 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("消息-{index}"));
            event.id = format!("event-{index}");
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
        app.selection_active = true;
        app.selected = 5;
        let selected_id = app.session.recent_events[app.selected].id.clone();

        let mut latest = DanmuEvent::new(DanmuEventKind::Danmu, "最新实时弹幕");
        latest.origin = DanmuEventOrigin::Live;
        latest.id = "live-latest".into();
        app.handle_client_event(BilibiliClientEvent::Danmu(latest));

        assert_eq!(app.session.recent_events[app.selected].id, selected_id);
    }

    #[test]
    fn browsing_mode_reports_unread_live_events_and_return_key() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..20 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("历史-{index}"));
            event.username = Some(format!("观众-{index}"));
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
        app.selection_active = true;
        app.selected = 10;
        let mut latest = DanmuEvent::new(DanmuEventKind::Danmu, "最新实时弹幕");
        latest.origin = DanmuEventOrigin::Live;
        latest.username = Some("新观众".into());
        app.handle_client_event(BilibiliClientEvent::Danmu(latest));

        let mut terminal = Terminal::new(TestBackend::new(72, 8)).unwrap();
        let palette = app.config.palette;
        terminal
            .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace(' ', "");

        assert!(
            rendered.contains("浏览历史·1条新·End返回实时"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn end_returns_from_history_browsing_to_live_feed() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.selection_active = true;
        app.selected = 7;
        app.unread_live_count = 3;
        let (tx, _rx) = mpsc::channel(1);

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), tx)
            .await
            .unwrap();

        assert!(!app.selection_active);
        assert_eq!(app.selected, 0);
        assert_eq!(app.unread_live_count, 0);
    }

    #[tokio::test]
    async fn enter_executes_the_highlighted_command_without_tab_completion() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.input = "/lay".into();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();

        assert!(app.layout_chat);
        assert!(app.input.is_empty());
    }

    #[tokio::test]
    async fn theme_command_opens_choices_and_enter_selects_one() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.input = "/theme".into();
        let choices = slash_suggestions(&app.input, &app.config.themes);
        assert_eq!(choices.len(), 5);
        assert!(matches!(
            choices[0],
            SlashSuggestion::Theme {
                id: "shisui",
                current: true,
                ..
            }
        ));
        assert_eq!(choices[1].completion(), "/theme catppuccin-mocha");
        drop(choices);

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();

        assert_eq!(app.config.theme_name, "catppuccin-mocha");
        assert!(app.input.is_empty());
    }

    #[tokio::test]
    async fn slash_palette_filters_and_completes_commands() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);

        app.input = "/obs sc".into();
        app.slash_selection = 0;
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(app.input, "/obs scene ");
        assert!(slash_suggestions("/question", &app.config.themes).is_empty());
    }

    #[tokio::test]
    async fn theme_command_switches_palette_and_persists_selection() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);

        assert!(!slash_suggestions("/theme", &app.config.themes).is_empty());
        app.command("/theme tokyo-night", tx.clone()).await.unwrap();
        assert_eq!(app.config.theme_name, "tokyo-night");
        assert_eq!(app.config.palette.background, Color::Rgb(26, 27, 38));
        assert_eq!(
            crate::theme::ThemeCatalog::load(temp.path().join("themes.json"))
                .unwrap()
                .selected(),
            "tokyo-night"
        );

        app.command("/theme", tx).await.unwrap();
        assert!(app.notice.contains("当前主题：tokyo-night"));
        assert!(app.notice.contains("themes.json"));
    }

    #[test]
    fn every_builtin_theme_reaches_the_rendered_terminal_surface() {
        let temp = tempfile::tempdir().unwrap();
        for theme_name in ["shisui", "catppuccin-mocha", "tokyo-night", "gruvbox-dark"] {
            let mut app = test_app(&temp, DanmuSession::new("1"));
            let (_, palette) = app.config.themes.resolve(theme_name).unwrap();
            app.config.theme_name = theme_name.into();
            app.config.palette = palette;
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

            terminal.draw(|frame| draw(frame, &mut app)).unwrap();

            assert_eq!(
                terminal.backend().buffer()[(40, 10)].bg,
                palette.background,
                "主题 {theme_name} 的背景色没有进入终端缓冲区"
            );
        }
    }
    #[test]
    fn obs_poll_failure_clears_stale_status_without_polluting_room_strip() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_ui_event(UiEvent::ObsStatus(Ok(ObsStatus {
            current_scene: "直播".into(),
            stream: crate::obs::StreamState::Live,
            microphone: crate::obs::MicrophoneState::Unmuted,
            compatibility_warning: None,
        })));
        assert!(app.obs_status.is_some());
        assert!(app.obs_error.is_none());

        app.handle_ui_event(UiEvent::ObsStatus(Err("OBS WebSocket 连接失败".into())));
        assert!(app.obs_status.is_none());
        assert_eq!(app.obs_error.as_deref(), Some("OBS WebSocket 连接失败"));
        assert!(app.obs_checked_at.is_some());

        let mut terminal = Terminal::new(TestBackend::new(120, 2)).unwrap();
        let palette = app.config.palette;
        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, palette))
            .unwrap();
        let rendered = (0..terminal.backend().buffer().area.width)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>()
            .replace(' ', "");
        assert!(rendered.contains("◉--"), "{rendered}");
        assert!(rendered.contains("●--"), "{rendered}");
        assert!(!rendered.contains("OBS"), "{rendered}");
    }

    #[test]
    fn long_input_wraps_inside_the_input_frame() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.input = format!("{}TAIL", "a".repeat(35)).into();
        assert_eq!(
            wrapped_cursor_position(&app.input, app.input.cursor(), 35),
            (1, 4)
        );
        assert_eq!(
            wrapped_input_lines("甲乙丙丁", 6),
            vec!["甲乙丙".to_string(), "丁".to_string()]
        );
        let palette = app.config.palette;
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();

        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, palette))
            .unwrap();

        let final_input_row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 2)].symbol())
            .collect::<String>();
        assert_eq!(final_input_row, format!("╰─ TAIL{}─╯", " ".repeat(31)));
    }

    #[test]
    fn room_snapshot_preserves_all_three_bilibili_live_states() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_ui_event(UiEvent::RoomSnapshot(RoomSnapshot {
            room_id: "1".into(),
            broadcaster_id: "42".into(),
            broadcaster_name: "停车拾穗".into(),
            title: "测试直播".into(),
            area: "知识".into(),
            live_started_at: Some(Utc::now() - chrono::Duration::hours(1)),
            live_status: RoomLiveStatus::Live,
        }));

        assert_eq!(room_live_label(app.room.as_ref().unwrap()), "● LIVE");
        assert!(app.room_updated_at.is_some());

        app.handle_ui_event(UiEvent::RoomSnapshot(RoomSnapshot {
            live_status: RoomLiveStatus::Rotating,
            ..app.room.clone().unwrap()
        }));
        assert_eq!(room_live_label(app.room.as_ref().unwrap()), "◉ ROTATING");

        app.handle_ui_event(UiEvent::RoomSnapshot(RoomSnapshot {
            live_status: RoomLiveStatus::Offline,
            ..app.room.clone().unwrap()
        }));
        assert_eq!(room_live_label(app.room.as_ref().unwrap()), "○ OFFLINE");
    }

    #[test]
    fn official_custom_emote_metadata_is_visible_in_text_terminals() {
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "你好[主播表情]");
        event.emotes.push(crate::domain::DanmuEmote {
            text: "[主播表情]".into(),
            fallback: "🙂".into(),
            image_url: url::Url::parse("https://example.com/emote.png").unwrap(),
            width: Some(48),
            height: Some(48),
            is_animated: false,
        });

        assert_eq!(map_event_emotes(&event), "你好🙂");
        assert_eq!(map_bili_emotes("[花] [委屈]"), "🌸 🥺");
    }

    #[test]
    fn danmu_content_wraps_to_the_available_terminal_width() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.show_name = false;
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "abcdefghij");

        let rendered = event_lines(&event, &app, app.config.palette, 6);
        let content = rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered.height(), 2);
        assert_eq!(content, "abcdefghij");
    }

    #[test]
    fn mouse_wheel_pages_history_without_selecting_a_reply_target() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..8 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("消息 {index}"));
            event.id = index.to_string();
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
        app.page_event_count = 3;
        let wheel_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let wheel_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..wheel_up
        };

        app.handle_mouse(wheel_up);
        assert_eq!(app.scroll_offset, 3);
        assert!(!app.selection_active);
        app.handle_mouse(wheel_down);
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.selection_active);
    }

    #[test]
    fn failed_online_refresh_preserves_the_last_successful_value() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_ui_event(UiEvent::OnlineViewers(Ok(Some(11))));
        app.handle_ui_event(UiEvent::OnlineViewers(Err("暂时失败".into())));

        assert_eq!(app.online_viewers, Some(11));
        assert!(app.notice.contains("继续显示上次成功值"));
    }

    #[tokio::test]
    async fn slash_obs_reports_progress_and_quit_exits() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(4);

        app.command("/obs", tx.clone()).await.unwrap();
        assert_eq!(app.notice, "正在检查 OBS 连接…");
        app.command("/quit", tx).await.unwrap();
        assert!(app.quit_requested);
    }
}
