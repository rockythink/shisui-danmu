struct MainLogin {
    token: uuid::Uuid,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MainLogin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

mod accounts;
mod activity;
mod agent;
mod assistant;
mod settings;
#[cfg(all(test, windows))]
mod windows_tests;

pub use agent::run_local as local;
mod input;
mod meter;
mod qr;
mod replay;
pub use replay::run as replay;
mod send;
use send::TerminalTransport;

pub use qr::qr_lines;

use crate::{
    bilibili::{
        AccountClient, AccountStatus, BilibiliClient, BilibiliClientEvent, LoginPoll,
        RoomLiveStatus, RoomSnapshot, cross_origin_duplicate, kind_label, masked_name_matches,
        normalized_message, segment_message, usable_author_id,
    },
    bridge::{self, Bridge},
    config::TerminalConfig,
    delivery::{Outcome, SendQueue, Transport},
    domain::{DanmuEvent, DanmuEventKind, DanmuEventOrigin, DanmuSession, DanmuSessionEndReason},
    obs::{MicrophoneLevel, MicrophoneState, ObsController, ObsStatus},
    persistence::SessionJournal,
    theme::Palette,
};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, Utc};
#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::VecDeque,
    io::{self, Stdout},
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const DELIVERY_NOTICE_LIFETIME: Duration = Duration::from_secs(15);
const DELIVERY_ECHO_WINDOW: chrono::Duration = chrono::Duration::seconds(90);
// Register once and keep the receivers alive across all startup/main-loop draws.
fn shutdown_signal() -> io::Result<impl std::future::Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        Ok(async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
                _ = hangup.recv() => {},
            }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupCheck {
    Waiting,
    Running,
    Passed(&'static str),
    Skipped(&'static str),
    Warning(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct StartupView {
    local: StartupCheck,
    account: StartupCheck,
    room: StartupCheck,
    metrics: StartupCheck,
    obs: StartupCheck,
}

impl StartupView {
    fn new() -> Self {
        Self {
            local: StartupCheck::Running,
            account: StartupCheck::Running,
            room: StartupCheck::Waiting,
            metrics: StartupCheck::Skipped("进入后后台读取"),
            obs: StartupCheck::Running,
        }
    }

    fn apply(&mut self, update: StartupUpdate) {
        match update {
            StartupUpdate::Local(status) => self.local = status,
            StartupUpdate::Account(status) => self.account = status,
            StartupUpdate::Room(status) => self.room = status,
        }
    }

    fn active_label(self) -> &'static str {
        if self.local == StartupCheck::Running {
            "恢复本地会话"
        } else if self.account == StartupCheck::Running {
            "检查登录状态"
        } else if self.room == StartupCheck::Running {
            "检查直播间　"
        } else if self.obs == StartupCheck::Running {
            "后台检查OBS连接"
        } else {
            "启动自检完成"
        }
    }
}

#[derive(Debug)]
enum StartupUpdate {
    Local(StartupCheck),
    Account(StartupCheck),
    Room(StartupCheck),
}

#[derive(Debug)]
struct StartupData {
    room: Option<RoomSnapshot>,
    initial_room_error: Option<String>,
    account_status: AccountStatus,
}

#[derive(Debug)]
struct StartupSession {
    session: DanmuSession,
    initial_error: Option<String>,
}

#[derive(Debug, Default)]
enum StartupGate {
    #[default]
    Checking,
    Ready,
    Blocked,
    EnteringObsPassword(EditorInput),
    Skipped,
}

impl StartupGate {
    fn can_finish(&self) -> bool {
        matches!(self, Self::Ready | Self::Skipped)
    }
}

impl StartupView {
    fn has_warning(self) -> bool {
        [self.local, self.room]
            .into_iter()
            .any(|status| matches!(status, StartupCheck::Warning(_)))
    }

    fn obs_requires_password(self) -> bool {
        matches!(self.obs, StartupCheck::Warning(message) if message.contains("/obs config password"))
    }
}

async fn load_startup_clients(account_session: PathBuf) -> Result<(BilibiliClient, AccountClient)> {
    tokio::task::spawn_blocking(move || {
        let client = BilibiliClient::new(account_session.clone())?;
        let account = AccountClient::new(account_session)?;
        Ok::<_, anyhow::Error>((client, account))
    })
    .await
    .map_err(|error| anyhow!("B 站客户端初始化任务失败：{error}"))?
}

async fn load_startup_session(
    journal: SessionJournal,
    room_id: String,
    updates: mpsc::UnboundedSender<StartupUpdate>,
) -> StartupSession {
    let history_room_id = room_id.clone();
    let recovery = tokio::task::spawn_blocking(move || {
        journal
            .recent_room_history(&history_room_id, 240)
            .map(|history| {
                let mut session = DanmuSession::new(&history_room_id);
                session.preload_history(history);
                session
            })
    })
    .await;
    let recovered = match recovery {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => Err(format!("会话恢复任务失败：{error}")),
    };
    match recovered {
        Ok(session) => {
            let status = if session.recent_events.is_empty() {
                "已读取"
            } else {
                "已恢复"
            };
            let _ = updates.send(StartupUpdate::Local(StartupCheck::Passed(status)));
            StartupSession {
                session,
                initial_error: None,
            }
        }
        Err(error) => {
            let _ = updates.send(StartupUpdate::Local(StartupCheck::Warning(
                "读取失败 · 可重试",
            )));
            StartupSession {
                session: DanmuSession::new(&room_id),
                initial_error: Some(error),
            }
        }
    }
}

async fn check_startup_obs(obs: ObsController) -> StartupCheck {
    match tokio::time::timeout(Duration::from_secs(3), obs.fetch_status()).await {
        Ok(Ok(_)) => StartupCheck::Passed("已连接"),
        Ok(Err(error)) if error.to_string().contains("/obs config password") => {
            StartupCheck::Warning("需要密码 · /obs config password")
        }
        Ok(Err(_)) => StartupCheck::Warning("连接失败 · 进入后重试"),
        Err(_) => StartupCheck::Warning("连接超时 · 进入后重试"),
    }
}

async fn load_startup_data(
    client: BilibiliClient,
    account: AccountClient,
    room_id: String,
    updates: mpsc::UnboundedSender<StartupUpdate>,
) -> StartupData {
    let account_updates = updates.clone();
    let account_request = async {
        match account.status().await {
            Ok(status @ AccountStatus::SignedIn { .. }) => {
                let _ =
                    account_updates.send(StartupUpdate::Account(StartupCheck::Passed("已登录")));
                status
            }
            Ok(AccountStatus::SignedOut) => {
                let _ = account_updates.send(StartupUpdate::Account(StartupCheck::Passed(
                    "未登录 · 监看模式",
                )));
                AccountStatus::SignedOut
            }
            Err(_) => {
                let _ = account_updates.send(StartupUpdate::Account(StartupCheck::Warning(
                    "检查失败 · 可重试",
                )));
                AccountStatus::SignedOut
            }
        }
    };

    let _ = updates.send(StartupUpdate::Room(StartupCheck::Running));
    let room_request = async {
        match client.room_snapshot(&room_id).await {
            Ok(room) => {
                let _ = updates.send(StartupUpdate::Room(StartupCheck::Passed("房间可访问")));
                (Some(room), None)
            }
            Err(error) => {
                let _ = updates.send(StartupUpdate::Room(StartupCheck::Warning(
                    "读取失败 · 可重试",
                )));
                (None, Some(error.to_string()))
            }
        }
    };

    let (account_status, (room, initial_room_error)) = tokio::join!(account_request, room_request);

    StartupData {
        room,
        initial_room_error,
        account_status,
    }
}

#[derive(Debug)]
enum UiEvent {
    LocalEcho(String),
    SettingSaved {
        token: uuid::Uuid,
        result: std::result::Result<settings::SavedSetting, String>,
    },

    DeliveryAccepted,
    DeliveryNotice(String),
    Notice {
        message: String,
        level: NoticeLevel,
    },
    LoginQr {
        token: uuid::Uuid,
        lines: Vec<String>,
    },
    LoginDone {
        token: uuid::Uuid,
        account: AccountClient,
        status: AccountStatus,
    },
    LoginFailed {
        token: uuid::Uuid,
        message: String,
    },
    AssistantAccount(accounts::AccountEvent),
    ObsStatus(std::result::Result<ObsStatus, String>),
    ObsStopDone(std::result::Result<(), String>),
    RoomSnapshot(RoomSnapshot),
    BroadcasterProfile {
        room_id: String,
        user_id: String,
        result: std::result::Result<Option<crate::bilibili::PublicProfile>, String>,
    },
    OnlineViewers(std::result::Result<Option<u64>, String>),
    Likes(std::result::Result<Option<u64>, String>),
    DeliveryStarted {
        delivery: PendingDelivery,
        confirmation: oneshot::Sender<()>,
        registered: oneshot::Sender<()>,
    },
    DeliveryHistory {
        events: Vec<DanmuEvent>,
    },
    DeliveryTimedOut {
        delivery_ids: Vec<String>,
    },
    DeliveryEchoMissing {
        delivery_ids: Vec<String>,
    },
    DeliveryExpired {
        delivery_id: String,
    },
    DeliveryRejected {
        content: String,
        message: String,
    },
    AssistantDelivery {
        session_id: String,
        record: serde_json::Value,
    },
    DeliveryCompleted,
}

async fn send_room_metrics(
    account: &AccountClient,
    tx: &mpsc::Sender<UiEvent>,
    room: &RoomSnapshot,
) -> bool {
    let (online, likes) = tokio::join!(
        account.current_online_viewers(&room.room_id, &room.broadcaster_id),
        account.current_likes(&room.room_id),
    );
    tx.send(UiEvent::OnlineViewers(
        online.map_err(|error| error.to_string()),
    ))
    .await
    .is_ok()
        && tx
            .send(UiEvent::Likes(likes.map_err(|error| error.to_string())))
            .await
            .is_ok()
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
            || event.timestamp > self.submitted_at + DELIVERY_ECHO_WINDOW
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
enum CommandCategory {
    Display,
    Account,
    Obs,
    Ai,
    System,
}

impl CommandCategory {
    fn color(self, palette: Palette) -> Color {
        match self {
            Self::Display => palette.host,
            Self::Account => palette.info,
            Self::Obs => palette.name,
            Self::Ai => palette.success,
            Self::System => palette.content,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandSpec {
    completion: &'static str,
    usage: &'static str,
    description: &'static str,
    category: CommandCategory,
    danger: bool,
}

enum SlashKeyAction {
    Ignored,
    Handled,
    Submit(String),
}

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        completion: "/mute",
        usage: "静音麦克风",
        description: "关闭声音；重复执行仍静音",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/unmute",
        usage: "恢复麦克风",
        description: "声音将公开",
        category: CommandCategory::Obs,
        danger: true,
    },
    CommandSpec {
        completion: "/scene",
        usage: "切换场景",
        description: "输入场景名称",
        category: CommandCategory::Obs,
        danger: true,
    },
    CommandSpec {
        completion: "/commands",
        usage: "指令",
        description: "中文搜索，保留草稿",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/more",
        usage: "更多",
        description: "OBS、诊断、帮助、退出",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/settings",
        usage: "设置与操作",
        description: "五类设置、本场与常用操作",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/diag",
        usage: "诊断",
        description: "历史告警与连接信息",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/pin",
        usage: "重点消息",
        description: "标记选中或最新消息",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/find",
        usage: "搜索归档",
        description: "输入关键词；也可直接带参数",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/obs ",
        usage: "OBS",
        description: "已有连接与控制操作",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/help",
        usage: "帮助",
        description: "操作说明",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/quit",
        usage: "退出弹幕台",
        description: "不停止推流",
        category: CommandCategory::System,
        danger: true,
    },
    CommandSpec {
        completion: "/ai",
        usage: "AI助手",
        description: "打开运行面板，不启动",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/review",
        usage: "待审回复",
        description: "全文审核后发送",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/pause",
        usage: "暂停AI",
        description: "撤权，保留可继续状态",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai start",
        usage: "启动AI",
        description: "连接工具，处理新弹幕",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai auto on",
        usage: "自动发送",
        description: "公开发送 · 同房间同账号记住",
        category: CommandCategory::Ai,
        danger: true,
    },
    CommandSpec {
        completion: "/ai auto off",
        usage: "逐条发送",
        description: "立即撤销自动许可",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai range",
        usage: "本场范围",
        description: "仅改变当前场次",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai topic",
        usage: "本场主题",
        description: "仅影响AI上下文，不修改直播间",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai visible",
        usage: "处理可见弹幕",
        description: "重建上下文，处理可见内容",
        category: CommandCategory::Ai,
        danger: true,
    },
    CommandSpec {
        completion: "/ai reset",
        usage: "重建上下文",
        description: "丢弃当前上下文",
        category: CommandCategory::Ai,
        danger: true,
    },
    CommandSpec {
        completion: "/ai stop",
        usage: "结束AI",
        description: "释放进程并撤销许可",
        category: CommandCategory::Ai,
        danger: true,
    },
    CommandSpec {
        completion: "/diag repair",
        usage: "重建历史",
        description: "先备份，再修复副本",
        category: CommandCategory::System,
        danger: true,
    },
    CommandSpec {
        completion: "/ai model",
        usage: "模型",
        description: "AI工具与原生选项",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai replies",
        usage: "发送策略",
        description: "方案与默认范围",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai materials",
        usage: "人设与资料",
        description: "AI名字、人设与上下文",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai advanced",
        usage: "Agent配置",
        description: "规则、运行策略与维护",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai workspace",
        usage: "工作空间",
        description: "编辑Agent工作目录",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/about",
        usage: "关于弹幕台",
        description: "官网、创始人、版本与源码",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/room title",
        usage: "直播间标题",
        description: "公开修改B站标题；不改AI主题",
        category: CommandCategory::Account,
        danger: true,
    },
    CommandSpec {
        completion: "/room cover",
        usage: "直播间封面",
        description: "上传本地图片并提交审核",
        category: CommandCategory::Account,
        danger: true,
    },
    CommandSpec {
        completion: "/display theme",
        usage: "主题",
        description: "选择界面主题",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display names on",
        usage: "显示昵称",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display names off",
        usage: "隐藏昵称",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display time on",
        usage: "显示时间",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display time off",
        usage: "隐藏时间",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display layout chat",
        usage: "聊天布局",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display layout list",
        usage: "列表布局",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
];

const OBS_COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        completion: "/obs config host",
        usage: "接入地址",
        description: "编辑OBS WebSocket主机名或IP",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs config port",
        usage: "接入端口",
        description: "编辑OBS WebSocket端口",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs status",
        usage: "连接状态",
        description: "查看OBS",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs connect",
        usage: "检查连接",
        description: "检查OBS",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs config mic",
        usage: "麦克风配置",
        description: "输入麦克风名称",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs config password",
        usage: "连接密码",
        description: "编辑私有密码",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs start",
        usage: "开始推流",
        description: "立即开始直播",
        category: CommandCategory::Obs,
        danger: true,
    },
    CommandSpec {
        completion: "/obs stop",
        usage: "停止推流",
        description: "直播将中断",
        category: CommandCategory::Obs,
        danger: true,
    },
];

const SETTINGS_COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        completion: "/settings reading",
        usage: "外观与显示",
        description: "主题、布局与阅读",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/settings account",
        usage: "B站账号与直播间",
        description: "主账号、AI发送账号、标题与封面",
        category: CommandCategory::Account,
        danger: false,
    },
    CommandSpec {
        completion: "/settings obs",
        usage: "OBS设置",
        description: "接入配置与当前控制",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/settings ai",
        usage: "AI助手设置",
        description: "工作空间、Agent与发送策略",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/settings system",
        usage: "系统与关于",
        description: "工具信息、诊断与修复",
        category: CommandCategory::System,
        danger: false,
    },
];

fn command_value(mut raw: &str, tokens: usize) -> Option<&str> {
    for _ in 0..tokens {
        raw = raw.trim_start();
        raw = &raw[raw.find(char::is_whitespace).unwrap_or(raw.len())..];
    }
    let value = raw.trim();
    (!value.is_empty()).then_some(value)
}

fn ai_command(spec: &CommandSpec) -> bool {
    matches!(spec.completion, "/ai" | "/review" | "/pause")
        || spec.completion.starts_with("/ai ")
        || spec.completion == "/settings ai"
}

fn obs_command(command: &str) -> bool {
    matches!(command, "/obs" | "/mute" | "/unmute" | "/scene") || command.starts_with("/obs ")
}

fn command_suggestions(input: &str, include_ai: bool) -> Vec<&'static CommandSpec> {
    let query = input.strip_prefix('/').unwrap_or(input);
    let (primary, settings, obs): (&[CommandSpec], &[CommandSpec], &[CommandSpec]) =
        if input.starts_with("/obs ") {
            (OBS_COMMAND_SPECS, &[], &[])
        } else if input.starts_with("/settings ") {
            (SETTINGS_COMMAND_SPECS, &[], &[])
        } else {
            (COMMAND_SPECS, SETTINGS_COMMAND_SPECS, OBS_COMMAND_SPECS)
        };
    let show_ai = include_ai || !query.trim().is_empty();
    let commands = primary
        .iter()
        .chain(settings)
        .chain(obs)
        .filter(|spec| show_ai || !ai_command(spec))
        .filter(|spec| {
            !query.is_empty()
                || matches!(
                    spec.completion,
                    "/mute"
                        | "/unmute"
                        | "/scene"
                        | "/ai"
                        | "/review"
                        | "/pause"
                        | "/pin"
                        | "/find"
                        | "/settings"
                        | "/diag"
                        | "/help"
                        | "/quit"
                )
        });
    let prefixes: Vec<_> = commands
        .clone()
        .filter(|spec| spec.completion.trim_start_matches('/').starts_with(query))
        .collect();
    if !prefixes.is_empty() {
        return prefixes;
    }
    let titles: Vec<_> = commands
        .clone()
        .filter(|spec| spec.usage.contains(query))
        .collect();
    if !titles.is_empty() {
        return titles;
    }
    commands
        .filter(|spec| spec.description.contains(query))
        .collect()
}

#[derive(Default)]
struct HelpState {
    scroll: usize,
    max_scroll: usize,
    page_rows: usize,
}

const HELP_SECTIONS: &[(&str, &str)] = &[
    (
        "常用",
        "输入 / 只显示常用操作；输入中文或命令前缀搜索全部，↑↓选择、Enter执行、Tab补全。/mute静音，/unmute恢复声音，/scene切场景，/diag看诊断。Ctrl-O独立搜索并保留草稿与光标。低频配置在/settings，AI运行操作在/ai。
普通输入 Enter 人工发送；粘贴只插入、不执行。Shift-↑/↓选择回复对象，Enter插入@；滚轮或↑↓浏览历史，Esc / End回最新。主界面直接拖选文字，用终端复制快捷键（macOS为⌘C）；↶表示重启前的弹幕。/pin标记消息；/find 打开关键词编辑，/find [关键词]直接搜索归档。",
    ),
    (
        "设置与回复",
        "设置用方向键选择、Enter执行、Esc返回；“设置”“当前操作”为只读分组。直达页Esc一次关闭。Tab切换同级分类，F1或?展开说明。编辑Enter保存、Esc取消；归档搜索必须输入关键词，取消不搜索。
/display theme选择主题；/display names或time on|off；/display layout chat|list保存布局。/settings ai集中AI配置；/ai model、/ai replies、/ai materials、/ai advanced直达子页，/ai workspace编辑工作空间。
/review查看候选全文；Shift-Enter发送本条，e编辑，Delete丢弃，Tab下一条。长文须先下滑看完；编辑只保存，须重新看完才可发送。",
    ),
    (
        "AI运行",
        "/ai start 启动；/pause 暂停并撤权、保留可继续状态；/ai stop 释放专用进程。/ai range 本场范围，/ai topic 本场主题。\n/ai auto on 允许公开自动发送，同房间同账号记住；/ai auto off 立即撤权。旧自动偏好不是许可。/ai visible 处理可见弹幕；/ai reset 重建上下文。/diag repair 先备份再重建历史。",
    ),
    (
        "指令与编辑",
        "指令搜索用↑↓选择、Enter执行；Tab只补全，带参数命令不猜参数。Ctrl-O、Ctrl-G、Ctrl-P、Ctrl-C仍兼容；不需要鼠标点击。\n←/→移光标，Home/End到首尾，Alt-←/→跳词。Backspace/Delete删除，Ctrl-U清空，Ctrl-W删前词，Ctrl-K删到末尾。单次粘贴最多4096字节，控制字符过滤。",
    ),
    (
        "状态与诊断",
        "输入框右侧保留人数、点赞与在线数；-- 表示暂无可靠数据。✓送达，?送达未确认且不自动重发，×被拒。AI逐条为绿色，未授权为黄色，自动为红色。\n/settings system提供诊断和修复；/about查看官网、创始人和版本。/diag保留历史告警、真实路径与连接细节，不扫描工具。人工发送失败仅结束本次及旧排队任务，后续新输入可继续发送；未确认消息不自动重发，登录失效仍须重新登录。",
    ),
    (
        "OBS与退出",
        "/settings obs配置接入地址、端口、密码和输入；/obs config host、port、mic不带参数打开编辑器，带值立即保存；/obs connect检查连接。\n/mute静音；/unmute公开声音；/scene [名称]编辑或切换场景；/obs start开始推流。只有/obs stop二次确认，确认后3秒内可取消。/quit退出弹幕台，不停止推流。\n/room title [标题]公开修改直播间标题，/room cover [图片路径]提交封面；仅主账号本人的直播间可修改，审核中不代表已生效。本地模式拒绝真实账号、直播间修改与OBS。",
    ),
];

#[derive(Debug, Clone, Copy)]
enum StopFlow {
    Confirm { stop_selected: bool },
    Countdown { deadline: Instant },
    Stopping,
}

pub struct TerminalApp {
    config: TerminalConfig,
    client: BilibiliClient,
    account: AccountClient,
    assistant_accounts: accounts::AssistantAccounts,
    manual_send_queue: SendQueue,
    independent_send_queue: SendQueue,
    bridge: Bridge,
    local_transport: Option<std::sync::Arc<agent::LocalTransport>>,
    live_scope: Option<String>,
    candidate_selected: Option<(String, String)>,
    candidate_edit: Option<agent::CandidateEdit>,
    review_frame: Option<agent::ReviewFrame>,
    runner: crate::runner::Runner,
    assistant_panel: Option<assistant::Panel>,
    settings_operation: Option<uuid::Uuid>,
    profile: agent::ProfileState,
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
    command_search_draft: Option<EditorInput>,
    secret_draft: Option<EditorInput>,
    help: Option<HelpState>,
    selected: usize,
    scroll_offset: usize,
    last_user_activity: Instant,
    activity: activity::ActivityNotices,
    notice: String,
    notice_deadline: Option<Instant>,
    notice_is_delivery: bool,
    notice_level: NoticeLevel,
    delivery_status: DeliveryStatus,
    delivery_status_deadline: Option<Instant>,
    layout_chat: bool,
    show_name: bool,
    show_time: bool,
    account_status: AccountStatus,
    stop_flow: Option<StopFlow>,
    login_qr: Option<Vec<String>>,
    main_login: Option<MainLogin>,
    secret_mode: bool,
    selection_active: bool,
    quit_requested: bool,
    unread_live_count: u64,
    obs_status: Option<ObsStatus>,
    microphone_level: Option<MicrophoneLevel>,
    obs_error: Option<String>,
    obs_checked_at: Option<DateTime<Local>>,
    pending_deliveries: VecDeque<ActiveDelivery>,
    confirmed_deliveries: VecDeque<PendingDelivery>,
    last_realtime_at: Option<DateTime<Local>>,
    last_live_danmu_at: Option<DateTime<Local>>,
    animation_tick: u64,
    resume_pending: bool,
}

impl TerminalApp {
    fn record_reading_config(&mut self) {
        self.runner
            .record_reading_config(crate::workspace_config::Reading {
                single_line: self.config.single_line,
                chat_layout: self.layout_chat,
                show_time: self.show_time,
                show_name: self.show_name,
                history_idle_seconds: self.config.history_idle_seconds,
                theme: self.config.theme_name.clone(),
            });
        if let Some(warning) = self.runner.config_warning.clone() {
            self.set_notice(warning, NoticeLevel::Error);
        }
    }
    fn persist_ui(&mut self, key: &str, value: toml_edit::Value) -> bool {
        match self.config.save_value(key, value) {
            Ok(()) => true,
            Err(error) => {
                self.set_notice(format!("配置未保存：{error}"), NoticeLevel::Error);
                false
            }
        }
    }
    pub async fn run(
        config: TerminalConfig,
        account_session: PathBuf,
        obs: ObsController,
        journal: SessionJournal,
    ) -> Result<()> {
        let shutdown = shutdown_signal()?;
        tokio::pin!(shutdown);
        let room_id = config.room_id.clone();
        let palette = config.palette;
        let mut terminal = TerminalGuard::enter()?;
        let mut events = EventStream::new();
        let mut startup_tick = tokio::time::interval(Duration::from_millis(90));
        startup_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut startup_frame = 0_u64;
        let startup_started_at = Instant::now();
        let mut startup_view = StartupView::new();
        terminal.terminal.draw(|frame| {
            draw_startup(
                frame,
                palette,
                startup_frame,
                &startup_view,
                &StartupGate::Checking,
                startup_started_at.elapsed(),
                false,
            )
        })?;

        let (startup_update_tx, mut startup_update_rx) = mpsc::unbounded_channel();
        let mut startup_clients = Box::pin(load_startup_clients(account_session));
        let mut clients = None;
        let mut startup_session = Box::pin(load_startup_session(
            journal.clone(),
            room_id.clone(),
            startup_update_tx.clone(),
        ));
        let mut startup_data = None;
        let mut startup_obs_check = Box::pin(check_startup_obs(obs.clone()));
        let mut startup_result: Option<StartupData> = None;
        let mut startup_session_result = None;
        let mut startup_obs_done = false;
        let mut startup_gate = StartupGate::default();
        let mut runner = crate::runner::Runner::load(&config.config_path);
        let mut startup_accounts: Option<accounts::AssistantAccounts> = None;
        let mut automatic_restore = false;
        let mut automatic_restore_warning = None;
        let mut resume_decision = (!runner.settings.resume_on_start).then_some(false);
        let (
            StartupData {
                room,
                initial_room_error,
                account_status,
            },
            StartupSession {
                session,
                initial_error: initial_session_error,
            },
        ) = loop {
            let checks_ready = startup_result.is_some() && startup_session_result.is_some();
            if checks_ready && matches!(startup_gate, StartupGate::Checking) {
                startup_gate = if startup_view.has_warning() {
                    StartupGate::Blocked
                } else {
                    StartupGate::Ready
                };
            }
            if checks_ready && startup_gate.can_finish() {
                if resume_decision.is_none() {
                    let accounts = startup_accounts
                        .as_ref()
                        .expect("startup accounts are loaded");
                    let status = &startup_result
                        .as_ref()
                        .expect("startup data is ready")
                        .account_status;
                    if runner.settings.automatic && accounts.automatic_matches(&room_id, status) {
                        automatic_restore = true;
                        resume_decision = Some(true);
                        continue;
                    }
                    resume_decision = Some(false);
                    continue;
                } else {
                    break (
                        startup_result.take().expect("startup result checked above"),
                        startup_session_result
                            .take()
                            .expect("startup session checked above"),
                    );
                }
            }
            tokio::select! {
                result = &mut startup_clients, if clients.is_none() => {
                    let (client, account) = result?;
                    startup_accounts = Some(accounts::AssistantAccounts::load(&account, &config.config_path)?);
                    startup_data = Some(Box::pin(load_startup_data(
                        client.clone(),
                        account.clone(),
                        room_id.clone(),
                        startup_update_tx.clone(),
                    )));
                    clients = Some((client, account));
                }
                result = &mut startup_session, if startup_session_result.is_none() => {
                    startup_session_result = Some(result);
                }
                result = async {
                    startup_data
                        .as_mut()
                        .expect("startup data is guarded above")
                        .await
                }, if startup_data.is_some() && startup_result.is_none() => {
                    let accounts = startup_accounts.as_mut().expect("startup accounts are loaded");
                    if accounts.has_automatic_grant() && !accounts.automatic_matches(&room_id, &result.account_status)
                        && let Err(error) = accounts.forget_automatic() {
                        automatic_restore_warning = Some(format!("房间或账号不匹配，未恢复自动发送；清除旧授权失败：{error}"));
                    }
                    startup_result = Some(result);
                }
                status = &mut startup_obs_check, if !startup_obs_done => {
                    startup_obs_done = true;
                    startup_view.obs = status;
                }
                Some(update) = startup_update_rx.recv() => {
                    startup_view.apply(update);
                }
                _ = &mut shutdown => return Ok(()),
                _ = startup_tick.tick() => {
                    startup_frame = startup_frame.wrapping_add(1);
                    terminal.terminal.draw(|frame| {
                        draw_startup(
                            frame,
                            palette,
                            startup_frame,
                            &startup_view,
                            &startup_gate,
                            startup_started_at.elapsed(),
                            checks_ready,
                        )
                    })?;
                }
                event = events.next() => {
                    let Some(event) = event else { return Ok(()); };
                    let event = event?;
                    match event {
                        Event::Key(key) => {
                            if key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }

                            let mut retry_all = false;
                            let mut password_to_save = None;
                            match &mut startup_gate {
                                StartupGate::Blocked => match key.code {
                                    KeyCode::Char('s' | 'S') => {
                                        startup_gate = StartupGate::Skipped;
                                    }
                                    KeyCode::Enter if startup_view.obs_requires_password() => {
                                        startup_gate = StartupGate::EnteringObsPassword(
                                            EditorInput::default(),
                                        );
                                    }
                                    KeyCode::Enter => retry_all = true,
                                    _ => {}
                                },
                                StartupGate::EnteringObsPassword(input) => match key.code {
                                    KeyCode::Esc => startup_gate = StartupGate::Blocked,
                                    KeyCode::Enter if !input.is_empty() => {
                                        password_to_save = Some(input.take());
                                    }
                                    KeyCode::Backspace => input.delete_before_cursor(),
                                    KeyCode::Char('u')
                                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                    {
                                        input.clear();
                                    }
                                    KeyCode::Char(character)
                                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                                    {
                                        input.insert_text(&character.to_string());
                                    }
                                    _ => {}
                                },
                                _ => {}
                            }

                            if retry_all {
                                startup_view = StartupView::new();
                                startup_result = None;
                                startup_session_result = None;
                                startup_obs_done = false;
                                let (client, account) =
                                    clients.as_ref().expect("startup clients initialized");
                                startup_data = Some(Box::pin(load_startup_data(
                                    client.clone(),
                                    account.clone(),
                                    room_id.clone(),
                                    startup_update_tx.clone(),
                                )));
                                startup_session = Box::pin(load_startup_session(
                                    journal.clone(),
                                    room_id.clone(),
                                    startup_update_tx.clone(),
                                ));
                                startup_obs_check =
                                    Box::pin(check_startup_obs(obs.clone()));
                                startup_gate = StartupGate::Checking;
                            }
                            if let Some(password) = password_to_save {
                                match obs.set_password(&password).await {
                                    Ok(()) => {
                                        startup_view.obs = StartupCheck::Running;
                                        startup_obs_done = false;
                                        startup_obs_check =
                                            Box::pin(check_startup_obs(obs.clone()));
                                        startup_gate = StartupGate::Checking;
                                    }
                                    Err(_) => {
                                        startup_view.obs = StartupCheck::Warning(
                                            "密码保存失败 · 检查文件权限",
                                        );
                                        startup_gate = StartupGate::Blocked;
                                    }
                                }
                            }
                        }
                        Event::Paste(text) => {
                            if let StartupGate::EnteringObsPassword(input) = &mut startup_gate
                                && text.len() <= 4096
                            {
                                input.insert_paste(&text, false);
                            }
                        }
                        _ => {}
                    }
                }
            }
        };
        let (client, account) = clients.expect("startup clients checked above");
        journal.start(&session)?;
        let history_error = runner
            .begin_history_import(&session.room_id, &journal)
            .err();
        let room_updated_at = room.as_ref().map(|_| Local::now());
        let mut app = Self {
            bridge: Bridge::new(false),
            local_transport: None,
            live_scope: None,
            candidate_selected: None,
            candidate_edit: None,

            review_frame: None,
            runner,
            assistant_panel: None,
            settings_operation: None,
            profile: agent::ProfileState::default(),
            layout_chat: config.chat_layout,
            show_name: config.show_name,
            show_time: config.show_time,
            assistant_accounts: startup_accounts.expect("startup accounts are loaded"),
            config,
            client: client.clone(),
            account,
            manual_send_queue: SendQueue::default(),
            independent_send_queue: SendQueue::default(),
            obs,
            journal,
            session,
            room,
            connection: "连接中".into(),
            watched: None,
            likes: None,
            online_viewers: None,
            input: EditorInput::default(),
            room_updated_at,
            slash_selection: 0,
            command_search_draft: None,
            secret_draft: None,
            help: None,
            selected: 0,
            scroll_offset: 0,
            last_user_activity: Instant::now(),
            activity: activity::ActivityNotices::default(),
            notice: "/ · Commands     Tab · Layout".into(),
            notice_deadline: Some(Instant::now() + Duration::from_secs(6)),
            notice_is_delivery: false,
            notice_level: NoticeLevel::Info,
            delivery_status: DeliveryStatus::Idle,
            delivery_status_deadline: None,
            account_status,
            stop_flow: None,
            login_qr: None,
            main_login: None,
            secret_mode: false,
            selection_active: false,
            quit_requested: false,
            unread_live_count: 0,
            obs_status: None,
            microphone_level: None,
            pending_deliveries: VecDeque::new(),
            confirmed_deliveries: VecDeque::new(),
            last_realtime_at: None,
            obs_error: None,
            obs_checked_at: None,
            last_live_danmu_at: None,
            animation_tick: 0,
            resume_pending: false,
        };
        if let Some(error) = history_error {
            app.runner.history_warning = Some(format!("后台历史加载未启动：{error:#}"));
            app.set_notice("历史后台加载未启动；输入 /diag 查看", NoticeLevel::Error);
        } else if let Some(error) = initial_session_error {
            app.set_notice(format!("本地会话恢复失败：{error}"), NoticeLevel::Error);
        } else if let Some(error) = initial_room_error {
            app.set_notice(format!("房间数据读取失败：{error}"), NoticeLevel::Error);
        }
        if let Some(warning) = automatic_restore_warning {
            app.set_notice(warning, NoticeLevel::Error);
        }
        app.record_reading_config();
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
            if let Some(room) = last_room.as_ref()
                && !send_room_metrics(&room_account, &room_tx, room).await
            {
                return;
            }

            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                match room_client
                    .room_snapshot_preserving(&refresh_room_id, last_room.as_ref())
                    .await
                {
                    Ok(snapshot) => {
                        last_room = Some(snapshot.clone());
                        if room_tx.send(UiEvent::RoomSnapshot(snapshot)).await.is_err()
                            || !send_room_metrics(
                                &room_account,
                                &room_tx,
                                last_room
                                    .as_ref()
                                    .expect("successful room refresh is stored"),
                            )
                            .await
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
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let _bridge_server = if let Some(root) = &app.config.instance {
            Some(bridge::wire::serve(root, app.bridge.clone()).await?)
        } else {
            None
        };
        let mut should_quit = false;
        app.resume_pending = resume_decision == Some(true);
        if !app.resume_pending && app.runner.settings.resume_on_start {
            app.set_notice("AI已暂停 · /ai start 继续", NoticeLevel::Info);
        }
        let restore_scope = (app.bridge.session_id(), app.assistant_accounts.generation());
        let mut restore_verified = !automatic_restore;
        let mut restore_check = if automatic_restore {
            let identity = app.assistant_accounts.send_identity(&app.account_status);
            Some(tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(10), identity.status())
                    .await
                    .map_err(|_| "发送身份验证超时；本次未恢复自动发送".to_string())?
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }))
        } else {
            None
        };
        if app.resume_pending {
            app.set_notice(
                "已记住自动发送；后台核对发送身份，Esc 可取消本次恢复",
                NoticeLevel::Info,
            );
        }

        let loop_result: Result<()> = async {
        while !should_quit {
            if !app.resume_pending && let Some(check) = restore_check.take() { check.abort(); }
            app.advance_bridge(ui_tx.clone());
            if app.resume_pending
                && restore_verified
                && !app.runner.history_loading()
                && app.bridge.status()["available"] == true
            {
                app.resume_pending = false;
                let result = app
                    .restore_automatic(&restore_scope.0, restore_scope.1)
                    .and_then(|()| app.runner.start(&app.bridge, false, false));
                if let Err(error) = result {
                    app.runner.pause(&app.bridge);
                    app.set_notice(
                        format!("助手未启用：{error}；请从 /ai 检查后重试"),
                        NoticeLevel::Error,
                    );
                } else {
                    app.set_notice(
                        "身份核验通过，AI正在启动；/pause 暂停",
                        NoticeLevel::Info,
                    );
                }
            }
            terminal.draw_app(&mut app)?;
            tokio::select! {
                result = async { restore_check.as_mut().expect("restore check is guarded").await }, if restore_check.is_some() => {
                    restore_check = None;
                    match result {
                        Ok(Ok(())) => restore_verified = true,
                        error => {
                            app.resume_pending = false;
                            app.set_notice(format!("自动发送未恢复：{}；请从 /ai 检查身份", match error {
                                Ok(Err(message)) => message,
                                Err(error) => error.to_string(),
                                Ok(Ok(())) => unreachable!(),
                            }), NoticeLevel::Error);
                        }
                    }
                },
                _ = &mut shutdown => { should_quit = true; },
                _ = tick.tick() => {
                    app.animation_tick = app.animation_tick.wrapping_add(1);
                    app.expire_notice_at(Instant::now());
                    app.advance_stop_at(Instant::now(), &ui_tx);
                    app.advance_history_at(Instant::now());
                },
                event = events.next() => {
                    let Some(event) = event else { break; };
                    let event = event?;
                    match event {
                        Event::Key(key) => {
                            if app.resume_pending && key.kind == crossterm::event::KeyEventKind::Press
                                && (!automatic_restore || key.code == KeyCode::Esc || (key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL))) {
                                app.resume_pending = false;
                                if let Some(check) = restore_check.take() { check.abort(); }
                                app.set_notice("已取消本次待启用；助手保持关闭", NoticeLevel::Info);
                            }
                            should_quit = app.handle_key(key, ui_tx.clone()).await?;
                        }
                        Event::Paste(text) => app.handle_paste(&text),
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
        Ok(())
        }.await;
        if let Some(check) = restore_check {
            check.abort();
        }
        let routing_log = app
            .runner
            .record_routing(&app.bridge, &app.journal, &app.session, true);
        app.runner.shutdown(&app.bridge).await;
        app.bridge.end();
        let _ = stop_tx.send(true);
        loop_result?;
        routing_log?;
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
                    self.last_live_danmu_at = Some(Local::now());
                }
                self.ingest_event(event);
            }
        }
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::SettingSaved { token, result } => self.finish_setting_save(token, result),
            UiEvent::LocalEcho(text) => {
                let mut event = DanmuEvent::new(DanmuEventKind::Danmu, text);
                event.username = Some("本机发送".into());
                event.author_id = Some("local-host".into());
                self.ingest_event(event);
            }

            UiEvent::DeliveryAccepted => self.set_delivery_status(DeliveryStatus::AwaitingEcho),
            UiEvent::DeliveryNotice(message) => self.set_delivery_notice(message),
            UiEvent::ObsStopDone(result) => {
                self.stop_flow = None;
                self.handle_ui_event(operation_notice(
                    result
                        .map(|()| "OBS 已停止推流".to_owned())
                        .map_err(anyhow::Error::msg),
                ));
            }
            UiEvent::AssistantAccount(event) => {
                let ready = matches!(&event, accounts::AccountEvent::Ready { .. });
                let unavailable = matches!(&event, accounts::AccountEvent::Unavailable { .. });
                if let Some(mut message) = self.assistant_accounts.apply_event(event) {
                    if unavailable {
                        self.resume_pending = false;
                        self.bridge.identity_changed();
                        self.runner.pause(&self.bridge);
                        self.review_frame = None;
                        if let Err(error) = self.assistant_accounts.forget_automatic() {
                            message.push_str(&format!("；清除记住的授权失败：{error}"));
                        }
                    } else {
                        self.login_qr = self.assistant_accounts.qr_lines().map(<[String]>::to_vec);
                    }
                    self.set_notice(
                        message,
                        if unavailable {
                            NoticeLevel::Error
                        } else {
                            NoticeLevel::Info
                        },
                    );
                    if ready {
                        let result = self
                            .assistant_accounts
                            .complete_independent_ready(&self.account_status);
                        match result {
                            Ok(message) => {
                                self.bridge.identity_changed();
                                self.runner.pause(&self.bridge);
                                self.review_frame = None;
                                self.set_notice(message, NoticeLevel::Success);
                            }
                            Err(error) => self.set_notice(error.to_string(), NoticeLevel::Error),
                        }
                        // Keep the visited account page and its return stack.
                    }
                }
            }
            UiEvent::Notice { message, level } => {
                self.set_notice(message, level);
            }
            UiEvent::LoginQr { token, lines } => {
                if self
                    .main_login
                    .as_ref()
                    .is_some_and(|login| login.token == token)
                {
                    self.login_qr = Some(lines);
                    self.set_notice("请使用主账号扫码登录", NoticeLevel::Progress);
                }
            }
            UiEvent::LoginDone {
                token,
                account,
                status,
            } => {
                if self
                    .main_login
                    .as_ref()
                    .is_some_and(|login| login.token == token)
                {
                    self.cancel_main_login();
                    let saved = self
                        .account
                        .session_path()
                        .ok_or_else(|| anyhow!("主账号凭据目录不可用"))
                        .and_then(|path| account.persist_to(path.to_path_buf()));
                    match saved {
                        Ok(_) => {
                            self.account_status = status;
                            let forgotten = self.assistant_accounts.main_changed();
                            self.bridge.identity_changed();
                            self.runner.pause(&self.bridge);
                            self.review_frame = None;
                            match forgotten {
                                Ok(()) => self.set_notice(
                                    "主账号登录成功；助手仍暂停，未授权发送",
                                    NoticeLevel::Success,
                                ),
                                Err(error) => self.set_notice(
                                    format!("主账号已登录，助手已停发；清除旧授权失败：{error}"),
                                    NoticeLevel::Error,
                                ),
                            }
                        }
                        Err(error) => self.set_notice(
                            format!("登录态保存失败，原账号保留：{error}"),
                            NoticeLevel::Error,
                        ),
                    }
                }
            }
            UiEvent::LoginFailed { token, message } => {
                if self
                    .main_login
                    .as_ref()
                    .is_some_and(|login| login.token == token)
                {
                    self.cancel_main_login();
                    self.set_notice(message, NoticeLevel::Error);
                }
            }
            UiEvent::ObsStatus(result) => {
                self.obs_checked_at = Some(Local::now());
                match result {
                    Ok(status) => {
                        self.runner.observe_microphone(status.microphone);
                        self.obs_status = Some(status);
                        self.obs_error = None;
                    }
                    Err(error) => {
                        self.runner.observe_microphone(MicrophoneState::Unknown);
                        self.obs_status = None;
                        self.obs_error = Some(error);
                    }
                }
            }
            UiEvent::RoomSnapshot(snapshot) => {
                self.room = Some(snapshot);
                self.room_updated_at = Some(Local::now());
                self.sync_assistant_room();
            }
            UiEvent::BroadcasterProfile {
                room_id,
                user_id,
                result,
            } => {
                if self
                    .room
                    .as_ref()
                    .is_some_and(|room| room.room_id == room_id && room.broadcaster_id == user_id)
                {
                    self.profile.pending = false;
                    self.profile.checked_at = Some(Instant::now());
                    match result {
                        Ok(value) => {
                            self.profile.value = value;
                            self.profile.error = None;
                        }
                        Err(error) => {
                            self.profile.value = None;
                            self.profile.error = Some(error);
                        }
                    }
                    self.sync_assistant_room();
                }
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
                registered,
            } => {
                while self.pending_deliveries.len() >= 32 {
                    self.pending_deliveries.pop_front();
                }
                self.pending_deliveries.push_back(ActiveDelivery {
                    delivery,
                    confirmation: Some(confirmation),
                });
                let _ = registered.send(());
                self.set_delivery_status(DeliveryStatus::Sending);
            }
            UiEvent::AssistantDelivery { session_id, record } => {
                if let Err(error) = self.journal.reply_record(&session_id, &record) {
                    self.runner.pause(&self.bridge);
                    self.set_notice(
                        format!("发送结果归档失败，助手已暂停；真实发送结果不变且不重发：{error}"),
                        NoticeLevel::Error,
                    );
                }
            }
            UiEvent::DeliveryHistory { events } => {
                for event in events {
                    self.ingest_event(event);
                }
                if !self.pending_deliveries.is_empty() {
                    self.set_delivery_status(DeliveryStatus::Verifying);
                }
            }
            UiEvent::DeliveryEchoMissing { delivery_ids } => {
                if self
                    .pending_deliveries
                    .iter()
                    .any(|active| delivery_ids.contains(&active.delivery.id))
                {
                    self.set_delivery_status(DeliveryStatus::Uncertain);
                    self.set_delivery_notice("未确认送达；不自动重发，详情见助手");
                }
            }
            UiEvent::DeliveryExpired { delivery_id } => {
                self.clear_delivery(&delivery_id);
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
                self.set_delivery_status(DeliveryStatus::Uncertain);
                self.set_delivery_notice(format!(
                    "平台接受或回流未确认；本次剩余部分取消，可继续输入新消息，不自动重发。{}",
                    contents
                        .iter()
                        .map(|content| format!("「{content}」"))
                        .collect::<Vec<_>>()
                        .join(" ｜ ")
                ));
            }
            UiEvent::DeliveryRejected { content, message } => {
                self.set_delivery_status(DeliveryStatus::Failed);
                self.set_delivery_notice(format!("{message}；内容：「{content}」"));
            }
            UiEvent::DeliveryCompleted => {
                if self.notice_is_delivery {
                    self.notice.clear();
                    self.notice_deadline = None;
                    self.notice_is_delivery = false;
                }
                if self.pending_deliveries.is_empty() {
                    self.set_delivery_status(DeliveryStatus::Delivered);
                } else {
                    self.set_delivery_status(DeliveryStatus::AwaitingEcho);
                }
            }
        }
    }

    fn ingest_event(&mut self, mut event: DanmuEvent) {
        if event.kind == DanmuEventKind::RoomStatus
            && event.origin == DanmuEventOrigin::Live
            && let Some(room) = self.room.as_mut()
        {
            room.live_status = RoomLiveStatus::Offline;
            self.bridge.end();
        }
        let live_arrival =
            event.origin == DanmuEventOrigin::Live && event.kind == DanmuEventKind::Danmu;
        let confirmed = self
            .pending_deliveries
            .iter()
            .position(|active| active.delivery.matches(&event));
        self.confirmed_deliveries
            .retain(|delivery| event.timestamp <= delivery.submitted_at + DELIVERY_ECHO_WINDOW);
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
            // Echo alone is insufficient: the sending service still checks HTTP acceptance.
            self.set_delivery_status(DeliveryStatus::Verifying);
        }

        if reconcile_cross_origin_event(&mut self.session.recent_events, &event) {
            return;
        }
        let is_new_logical_event = !self
            .session
            .recent_events
            .iter()
            .any(|existing| existing.id == event.id);
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
            if is_new_logical_event {
                self.bridge.ingest(event.clone());
                if let Err(error) = self.runner.record_history(&self.session.room_id, &event) {
                    self.set_notice(
                        format!("历史索引未更新；原件仍单独保存：{error}"),
                        NoticeLevel::Error,
                    );
                }
            }
            if let Err(error) = self.journal.event(&self.session, &event) {
                self.set_notice(
                    format!("直播原始归档或工作区副本未完整保存：{error}"),
                    NoticeLevel::Error,
                );
            }
            self.activity.push(&event, Instant::now(), Utc::now());
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
            if is_new_logical_event
                && live_arrival
                && (self.selection_active || self.scroll_offset > 0)
            {
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
        self.notice_is_delivery = true;
        self.notice_level = NoticeLevel::Warning;
    }

    fn set_notice_at(&mut self, message: impl Into<String>, level: NoticeLevel, now: Instant) {
        self.notice = message.into();
        self.notice_deadline = level.lifetime().map(|lifetime| now + lifetime);
        self.notice_is_delivery = false;
        self.notice_level = level;
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
        self.activity.advance(now);
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

    fn is_browsing_history(&self) -> bool {
        self.selection_active || self.scroll_offset > 0
    }

    fn advance_history_at(&mut self, now: Instant) {
        // Modal operations must not silently discard a reply target underneath them.
        if self.stop_flow.is_some()
            || self.login_qr.is_some()
            || self.secret_mode
            || self.candidate_edit.is_some()
            || self.assistant_panel.is_some()
            || self.help.is_some()
        {
            self.last_user_activity = now;
            return;
        }
        if self.is_browsing_history()
            && self.config.history_idle_seconds > 0
            && now.saturating_duration_since(self.last_user_activity)
                >= Duration::from_secs(u64::from(self.config.history_idle_seconds))
        {
            self.return_to_live();
            self.set_notice("FOLLOW", NoticeLevel::Info);
        }
    }

    fn return_to_live(&mut self) {
        self.selection_active = false;
        self.selected = 0;
        self.scroll_offset = 0;
        self.unread_live_count = 0;
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        if !matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return;
        }
        self.last_user_activity = Instant::now();
        if self.stop_flow.is_some()
            || self.login_qr.is_some()
            || self.secret_mode
            || self.candidate_edit.is_some()
        {
            return;
        }
        if let Some(help) = self.help.as_mut() {
            match event.kind {
                MouseEventKind::ScrollUp => help.scroll = help.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => {
                    help.scroll = help.scroll.saturating_add(3).min(help.max_scroll)
                }
                _ => {}
            }
        } else if self.command_search_draft.is_some() || self.input.starts_with('/') {
            let count = self.command_suggestions().len();
            if count > 0 {
                self.slash_selection = if event.kind == MouseEventKind::ScrollUp {
                    self.slash_selection.saturating_sub(1)
                } else {
                    (self.slash_selection + 1).min(count - 1)
                };
            }
        } else if self.assistant_panel.is_some() {
            self.assistant_mouse(event);
        } else {
            self.scroll_history(event.kind == MouseEventKind::ScrollUp);
        }
    }

    fn open_commands(&mut self) {
        if self.command_search_draft.is_none() {
            self.command_search_draft = Some(std::mem::take(&mut self.input));
        }
        self.slash_selection = 0;
        self.clear_review_surface();
    }

    fn scroll_history(&mut self, older: bool) {
        let mut candidates = self
            .session
            .recent_events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.kind.activity_lifetime().is_none())
            .map(|(index, _)| index);
        let Some(newest) = candidates.next() else {
            self.return_to_live();
            return;
        };
        let anchor = if self.selection_active {
            self.selected
        } else {
            self.scroll_offset
        }
        .max(newest);
        let next = if older {
            candidates.find(|index| *index > anchor).unwrap_or(anchor)
        } else {
            candidates
                .rev()
                .find(|index| *index < anchor)
                .unwrap_or(newest)
        };
        if next == newest {
            self.return_to_live();
        } else {
            self.selection_active = false;
            self.selected = 0;
            self.scroll_offset = next;
        }
    }

    fn handle_slash_key(&mut self, key: &KeyEvent) -> SlashKeyAction {
        let searching = self.command_search_draft.is_some();
        if self.secret_mode || (!searching && !self.input.starts_with('/')) {
            return SlashKeyAction::Ignored;
        }

        let suggestions = self.command_suggestions();
        let move_up = key.code == KeyCode::Up;
        let move_down = key.code == KeyCode::Down
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('n'));

        if key.code == KeyCode::Esc {
            self.close_command_search();
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        if suggestions.is_empty() {
            if searching
                && matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Tab | KeyCode::Up | KeyCode::Down
                )
            {
                if key.code == KeyCode::Enter && self.input.starts_with('/') {
                    let command = sanitize_input(self.input.take());
                    self.close_command_search();
                    return SlashKeyAction::Submit(command);
                }
                return SlashKeyAction::Handled;
            }
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
            self.input
                .replace(suggestions[selected].completion.to_owned());
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        if key.code == KeyCode::Enter {
            return self.activate_palette_row(selected);
        }
        SlashKeyAction::Ignored
    }

    fn activate_palette_row(&mut self, index: usize) -> SlashKeyAction {
        let suggestions = self.command_suggestions();
        let Some(suggestion) = suggestions.get(index) else {
            return SlashKeyAction::Handled;
        };
        let completion = suggestion.completion;
        let raw = self.input.trim();
        let has_arguments = raw.starts_with(completion.trim_end())
            && raw.len() > completion.trim_end().len()
            && raw.as_bytes().get(completion.trim_end().len()) == Some(&b' ');
        if completion.ends_with(' ') && !has_arguments {
            self.input.replace(completion.to_owned());
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        let command = if has_arguments {
            raw.to_owned()
        } else {
            completion.to_owned()
        };
        self.close_command_search();
        SlashKeyAction::Submit(sanitize_input(command))
    }

    fn command_suggestions(&self) -> Vec<&'static CommandSpec> {
        let include_ai = self.runner.has_saved_settings()
            || self.runner.running
            || self.runner.report.connected
            || self.bridge.candidate_count() != 0;
        if self.command_search_draft.is_some() || self.input.starts_with('/') {
            command_suggestions(&self.input, include_ai)
        } else {
            Vec::new()
        }
    }

    fn close_command_search(&mut self) {
        self.input = self.command_search_draft.take().unwrap_or_default();
        self.slash_selection = 0;
    }

    fn handle_stop_key(&mut self, key: KeyCode, now: Instant) {
        match self.stop_flow {
            Some(StopFlow::Confirm { stop_selected }) => match key {
                KeyCode::Up | KeyCode::Left => {
                    self.stop_flow = Some(StopFlow::Confirm {
                        stop_selected: true,
                    });
                }
                KeyCode::Down | KeyCode::Right => {
                    self.stop_flow = Some(StopFlow::Confirm {
                        stop_selected: false,
                    });
                }
                KeyCode::Enter if stop_selected => {
                    self.stop_flow = Some(StopFlow::Countdown {
                        deadline: now + Duration::from_secs(3),
                    });
                }
                KeyCode::Enter | KeyCode::Esc => self.cancel_stop(),
                _ => {}
            },
            Some(StopFlow::Countdown { .. }) if key == KeyCode::Esc => self.cancel_stop(),
            _ => {}
        }
    }

    fn cancel_stop(&mut self) {
        self.stop_flow = None;
        self.set_notice("已取消停止推流", NoticeLevel::Info);
    }

    fn advance_stop_at(&mut self, now: Instant, tx: &mpsc::Sender<UiEvent>) {
        if !matches!(self.stop_flow, Some(StopFlow::Countdown { deadline }) if now >= deadline) {
            return;
        }
        // Leave Countdown before spawning: later ticks must never send a second stop.
        self.stop_flow = Some(StopFlow::Stopping);
        let obs = self.obs.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let result = obs.stop_stream().await.map_err(|error| error.to_string());
            let _ = tx.send(UiEvent::ObsStopDone(result)).await;
        });
    }

    async fn handle_key(&mut self, key: KeyEvent, tx: mpsc::Sender<UiEvent>) -> Result<bool> {
        if key.kind == crossterm::event::KeyEventKind::Press
            && self.stop_flow.is_some()
            && !(key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'p')))
        {
            self.handle_stop_key(key.code, Instant::now());
            return Ok(false);
        }
        if key.kind == crossterm::event::KeyEventKind::Press {
            if key.code == KeyCode::Esc && self.main_login.is_some() {
                self.cancel_main_login();
                self.set_notice("已取消主账号扫码，原账号未更改", NoticeLevel::Info);
                return Ok(false);
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(true);
            }
            if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.pause_assistant();
                return Ok(false);
            }
            if key.code == KeyCode::Esc && self.assistant_accounts.login_pending() {
                self.assistant_accounts.cancel_login();
                self.login_qr = None;
                self.set_notice("已取消助手扫码，原账号未更改", NoticeLevel::Info);
                return Ok(false);
            }
            if self.login_qr.is_some() {
                if key.code == KeyCode::Esc {
                    self.login_qr = None;
                }
                return Ok(false);
            }
            if self.handle_help_key(key.code) {
                return Ok(false);
            }
            if self.command_search_draft.is_none()
                && self.candidate_edit.is_none()
                && !self.secret_mode
                && self.assistant_key(key, tx.clone()).await
            {
                return Ok(self.quit_requested);
            }
            if key.code == KeyCode::Char('g')
                && key.modifiers == KeyModifiers::CONTROL
                && self.command_search_draft.is_none()
            {
                if let Err(error) = self.open_assistant() {
                    self.set_notice(error.to_string(), NoticeLevel::Error);
                }
                return Ok(false);
            }
        }

        if key.kind != crossterm::event::KeyEventKind::Release {
            self.last_user_activity = Instant::now();
        }
        if key.kind != crossterm::event::KeyEventKind::Press {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT {
            if self.secret_mode
                || self.stop_flow.is_some()
                || self.login_qr.is_some()
                || self.command_search_draft.is_some()
            {
                return Ok(false);
            }
            self.review_key(key);
            return Ok(false);
        }
        if self.stop_flow.is_some() {
            self.handle_stop_key(key.code, Instant::now());
            return Ok(false);
        }
        if key.code == KeyCode::Char('o') && key.modifiers == KeyModifiers::CONTROL {
            if self.candidate_edit.is_none() && !self.secret_mode {
                if self.command_search_draft.is_some() {
                    self.close_command_search();
                } else {
                    self.open_commands();
                }
            }
            return Ok(false);
        }

        if self.candidate_edit.is_some() {
            match key.code {
                KeyCode::Enter => {
                    self.save_candidate_edit();
                    return Ok(false);
                }
                KeyCode::Esc => {
                    self.finish_candidate_edit();
                    self.set_notice("已取消回复编辑；人工草稿已恢复", NoticeLevel::Info);
                    return Ok(false);
                }
                KeyCode::Tab | KeyCode::Up | KeyCode::Down => return Ok(false),
                _ => {}
            }
        }

        match if self.candidate_edit.is_some() {
            SlashKeyAction::Ignored
        } else {
            self.handle_slash_key(&key)
        } {
            SlashKeyAction::Ignored => {}
            SlashKeyAction::Handled => return Ok(false),
            SlashKeyAction::Submit(command) => {
                self.submit(command, tx).await?;
                return Ok(self.quit_requested);
            }
        }
        let selection_navigation = (key.modifiers.is_empty()
            || key.modifiers.contains(KeyModifiers::SHIFT))
            && matches!(key.code, KeyCode::Up | KeyCode::Down);
        if self.candidate_edit.is_none()
            && !self.secret_mode
            && self.command_search_draft.is_none()
            && self.selection_active
            && !(self.input.starts_with('/')
                || (self.input.is_empty() && key.code == KeyCode::Char('/')))
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
                    self.input = self.secret_draft.take().unwrap_or_default();
                    return Ok(false);
                }
                if self.selection_active || self.scroll_offset > 0 {
                    self.return_to_live();
                    return Ok(false);
                }
                return Ok(false);
            }
            KeyCode::Tab => {
                if !self.persist_ui("chat_layout", (!self.layout_chat).into()) {
                    return Ok(false);
                }
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
                self.record_reading_config();
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_selection(true),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_selection(false)
            }
            KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() && !self.secret_mode => {
                self.scroll_history(key.code == KeyCode::Up);
            }
            KeyCode::Home => self.input.move_to_start(),
            KeyCode::End
                if self.candidate_edit.is_none()
                    && self.command_search_draft.is_none()
                    && (self.selection_active || self.scroll_offset > 0) =>
            {
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
                if !self.secret_mode && self.selection_active && !self.input.starts_with('/') {
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
            if let Err(error) = self.obs.set_password(&input).await {
                self.input.replace(input);
                self.set_notice(format!("保存失败：{error}（内容保留）"), NoticeLevel::Error);
            } else {
                self.secret_mode = false;
                self.input = self.secret_draft.take().unwrap_or_default();
                self.set_notice("已保存", NoticeLevel::Success);
            }
            return Ok(());
        }
        if input.starts_with('/') {
            return self.command(&input, tx).await;
        }

        let segments = segment_message(&input, crate::bilibili::SEND_SEGMENT_LIMIT);
        if !segments.is_empty() {
            self.enqueue(segments, None, tx);
        }
        Ok(())
    }

    async fn command(&mut self, raw: &str, tx: mpsc::Sender<UiEvent>) -> Result<()> {
        let action: Result<()> = async {
            let parts = raw.split_whitespace().collect::<Vec<_>>();
            if self.local_transport.is_some()
                && parts
                    .first()
                    .is_some_and(|command| obs_command(command) || *command == "/room")
            {
                anyhow::bail!("本地模式禁止账号与OBS操作");
            }
            match parts.as_slice() {
                ["/about"] => self.open_about()?,
                ["/display", "theme"] => self.open_theme_settings()?,
                ["/display", setting, value] => self.configure_display(setting, value)?,
                [
                    "/ai",
                    section @ ("model" | "replies" | "materials" | "advanced"),
                ] => self.open_ai_section(section)?,
                ["/ai", "workspace", ..] => {
                    self.edit_workspace(command_value(raw, 2), tx)?;
                }
                ["/settings", "obs"] => self.open_obs_settings().await?,
                ["/room", setting @ ("title" | "cover"), ..] => {
                    let field = if *setting == "title" {
                        settings::ExternalField::RoomTitle
                    } else {
                        settings::ExternalField::RoomCover
                    };
                    self.open_external_editor(field, command_value(raw, 2), tx)
                        .await?;
                }
                ["/obs", "config", setting @ ("host" | "port" | "mic"), ..] => {
                    let field = match *setting {
                        "host" => settings::ExternalField::ObsHost,
                        "port" => settings::ExternalField::ObsPort,
                        _ => settings::ExternalField::ObsMicrophone,
                    };
                    self.open_external_editor(field, command_value(raw, 3), tx)
                        .await?;
                }
                ["/scene", ..] => {
                    self.open_external_editor(
                        settings::ExternalField::ObsScene,
                        command_value(raw, 1),
                        tx,
                    )
                    .await?;
                }
                ["/commands"] => self.open_commands(),
                ["/more"] => self.open_more()?,
                ["/ai", "start"] => {
                    self.start_assistant(false, false)?;
                }
                ["/ai", "auto", "on"] => {
                    self.set_automatic_permission(true)?;
                }
                ["/ai", "auto", "off"] => {
                    self.set_automatic_permission(false)?;
                }
                ["/ai", "range"] => self.open_ai_range()?,
                ["/ai", "topic"] => self.open_ai_topic()?,
                ["/ai", "visible"] => self.start_assistant(true, true)?,
                ["/ai", "reset"] => self.start_assistant(false, true)?,
                ["/ai", "stop"] => self.stop_assistant(),
                ["/diag", "repair"] => {
                    self.runner.repair_history(&self.journal)?;
                }
                ["/settings", section] => {
                    if let Err(error) = self.open_settings_section(section) {
                        self.set_notice(error.to_string(), NoticeLevel::Error);
                    }
                }
                ["/settings"] | ["/diag"] | ["/ai"] | ["/review"] => {
                    let result = match parts[0] {
                        "/settings" => self.open_settings(),
                        "/diag" => self.open_diagnostics(),
                        "/ai" => self.open_assistant(),
                        _ => self.open_candidate_review(),
                    };
                    if let Err(error) = result {
                        self.set_notice(error.to_string(), NoticeLevel::Error);
                    }
                }
                ["/pause"] => self.pause_assistant(),

                ["/quit"] => {
                    self.quit_requested = true;
                }
                ["/help"] => {
                    self.help = Some(HelpState::default());
                }
                ["/pin"] => {
                    let notice = if self.selection_active {
                        "已将选中消息设为重点"
                    } else {
                        "未选择消息，已将最新一条设为重点"
                    };
                    self.with_selected(|session, id| session.feature(Some(id)), notice)
                }
                ["/find"] => self.open_archive_search()?,
                ["/find", rest @ ..] => self.search_archive(&rest.join(" ")),
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
                ["/mute"] | ["/unmute"] => {
                    self.set_notice(
                        if parts[0] == "/mute" {
                            "正在静音麦克风…"
                        } else {
                            "正在取消麦克风静音…"
                        },
                        NoticeLevel::Progress,
                    );
                    let muted = parts[0] == "/mute";
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
                ["/obs", "config", "password"] => {
                    self.secret_mode = true;
                    self.secret_draft = Some(std::mem::take(&mut self.input));
                    self.set_notice(
                        "请输入 OBS WebSocket 密码并按 Enter；输入不会显示或进入历史",
                        NoticeLevel::Progress,
                    );
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
                    if self.stop_flow.is_none() {
                        self.stop_flow = Some(StopFlow::Confirm {
                            stop_selected: false,
                        });
                    }
                }
                _ => self.set_notice(
                    format!("未知命令：{raw}；输入 /help 查看命令"),
                    NoticeLevel::Error,
                ),
            }
            Ok(())
        }
        .await;
        if let Err(error) = action {
            self.set_notice(error.to_string(), NoticeLevel::Error);
            return Ok(());
        }
        // Operation failures stay in the UI; current journal write failures still propagate.
        self.journal.snapshot(&self.session)?;
        Ok(())
    }

    fn cancel_main_login(&mut self) {
        if self.main_login.take().is_some() {
            self.login_qr = None;
        }
    }

    fn start_login(&mut self, tx: mpsc::Sender<UiEvent>) {
        self.cancel_main_login();
        self.assistant_accounts.cancel_login();
        self.bridge.identity_changed();
        self.runner.pause(&self.bridge);
        self.review_frame = None;
        self.login_qr = None;
        let token = uuid::Uuid::new_v4();
        let account = self.account.staged();
        let task = tokio::spawn(async move {
            let result: Result<AccountStatus> = async {
                let challenge =
                    tokio::time::timeout(Duration::from_secs(10), account.login_challenge())
                        .await??;
                let lines = compact_qr_lines(challenge.url.as_str())?;
                tx.send(UiEvent::LoginQr { token, lines }).await?;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
                loop {
                    anyhow::ensure!(
                        tokio::time::Instant::now() < deadline,
                        "登录二维码已过期，请重新扫码"
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        account.poll_login(&challenge.key),
                    )
                    .await??
                    {
                        LoginPoll::Waiting | LoginPoll::Scanned => {}
                        LoginPoll::Expired => return Err(anyhow!("登录二维码已过期，请重新扫码")),
                        LoginPoll::SignedIn(status) => return Ok(status),
                    }
                }
            }
            .await;
            let event = match result {
                Ok(status) => UiEvent::LoginDone {
                    token,
                    account,
                    status,
                },
                Err(error) => UiEvent::LoginFailed {
                    token,
                    message: format!("主账号登录失败，原账号未更改：{error}"),
                },
            };
            let _ = tx.send(event).await;
        });
        self.main_login = Some(MainLogin { token, task });
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

    fn handle_help_key(&mut self, key: KeyCode) -> bool {
        let Some(help) = self.help.as_mut() else {
            return false;
        };
        match key {
            KeyCode::Esc | KeyCode::Enter => self.help = None,
            KeyCode::Up => help.scroll = help.scroll.saturating_sub(1),
            KeyCode::Down => help.scroll = help.scroll.saturating_add(1).min(help.max_scroll),
            KeyCode::PageUp => help.scroll = help.scroll.saturating_sub(help.page_rows),
            KeyCode::PageDown => {
                help.scroll = help
                    .scroll
                    .saturating_add(help.page_rows)
                    .min(help.max_scroll)
            }
            KeyCode::Home => help.scroll = 0,
            KeyCode::End => help.scroll = help.max_scroll,
            _ => {}
        }
        true
    }

    fn handle_paste(&mut self, text: &str) {
        if self.help.is_some()
            || self.stop_flow.is_some()
            || self.login_qr.is_some()
            || self.main_login.is_some()
            || self.assistant_accounts.login_pending()
        {
            return;
        }
        if self.assistant_panel.is_some()
            && self.candidate_edit.is_none()
            && self.command_search_draft.is_none()
        {
            self.paste_assistant(text);
            return;
        }
        if !self.secret_mode
            && self.candidate_edit.is_none()
            && self.command_search_draft.is_none()
            && self.selection_active
            && !(self.input.starts_with('/') || (self.input.is_empty() && text.starts_with('/')))
        {
            return;
        }
        if text.len() > 4096 {
            self.set_notice("粘贴超过4096字节，未插入", NoticeLevel::Error);
            return;
        }
        self.last_user_activity = Instant::now();
        self.input.insert_paste(text, false);
        self.slash_selection = 0;
    }

    fn insert_text(&mut self, text: &str) {
        self.input.insert_text(text);
        self.slash_selection = 0;
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse_capture: bool,
}
impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        // Windows can only restore mouse mode after EnableMouseCapture saved it.
        #[cfg(not(windows))]
        execute!(stdout, DisableMouseCapture)?;
        execute!(stdout, EnableBracketedPaste)?;
        // Crossterm's Windows backend rejects the Kitty keyboard protocol.
        #[cfg(not(windows))]
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        )?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            mouse_capture: false,
        })
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        if self.mouse_capture == enabled {
            return Ok(());
        }
        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
        }
        self.mouse_capture = enabled;
        Ok(())
    }

    pub(super) fn draw_app(&mut self, app: &mut TerminalApp) -> Result<()> {
        // Main-screen dragging belongs to the terminal. Alternate-screen wheels
        // arrive as arrow keys; overlays retain their existing mouse handling.
        self.set_mouse_capture(
            app.stop_flow.is_some()
                || app.login_qr.is_some()
                || app.secret_mode
                || app.candidate_edit.is_some()
                || app.help.is_some()
                || app.assistant_panel.is_some()
                || app.command_search_draft.is_some()
                || app.input.starts_with('/'),
        )?;
        self.terminal.draw(|frame| draw(frame, app))?;
        Ok(())
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Windows mouse restoration reinstates the saved raw input mode.
        // Release owned capture before restoring cooked input for the shell.
        let _ = self.set_mouse_capture(false);
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), DisableBracketedPaste);
        #[cfg(not(windows))]
        let _ = execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags);
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
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
    if incoming.reply_to.is_some() {
        existing.reply_to.clone_from(&incoming.reply_to);
    }
    true
}

fn display_events(events: &[DanmuEvent]) -> impl Iterator<Item = &DanmuEvent> {
    events
        .iter()
        .rev()
        .filter(|event| event.kind.activity_lifetime().is_none())
}

fn startup_check_line(
    label: &'static str,
    status: StartupCheck,
    palette: Palette,
) -> Line<'static> {
    let (marker, detail, color) = match status {
        StartupCheck::Waiting => ("·", "等待", palette.frame),
        StartupCheck::Running => ("›", "检查中", palette.info),
        StartupCheck::Passed(detail) => ("✓", detail, palette.success),
        StartupCheck::Skipped(detail) => ("–", detail, palette.time),
        StartupCheck::Warning(detail) => ("!", detail, palette.warning),
    };
    Line::from(vec![
        Span::styled(format!("{marker} "), Style::default().fg(color)),
        Span::styled(format!("{label}  "), Style::default().fg(palette.content)),
        Span::styled(detail, Style::default().fg(color)),
    ])
}

fn startup_progress_line(
    elapsed: Duration,
    data_ready: bool,
    width: usize,
    palette: Palette,
) -> Line<'static> {
    let filled = if data_ready {
        width
    } else {
        ((elapsed.as_millis() / 90) as usize % width.max(1)).min(width.saturating_sub(1))
    };
    Line::from(vec![
        Span::styled("━".repeat(filled), Style::default().fg(palette.info)),
        Span::styled(
            "─".repeat(width.saturating_sub(filled)),
            Style::default().fg(palette.frame),
        ),
    ])
}

fn draw_startup(
    frame: &mut ratatui::Frame,
    palette: Palette,
    tick: u64,
    startup: &StartupView,
    gate: &StartupGate,
    elapsed: Duration,
    data_ready: bool,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.background)),
        area,
    );

    let cursor = if (tick / 3).is_multiple_of(2) {
        "▮"
    } else {
        " "
    };
    let masked_password = match gate {
        StartupGate::EnteringObsPassword(input) => {
            let visible = input.graphemes(true).count().min(16);
            format!("OBS 密码 {}{cursor}", "●".repeat(visible))
        }
        _ => String::new(),
    };
    let (active_label, active_color) = match gate {
        StartupGate::Blocked if startup.obs_requires_password() => {
            ("启动已阻断 · OBS 需要配置".to_owned(), palette.warning)
        }
        StartupGate::Blocked => ("启动已阻断 · 检查未通过".to_owned(), palette.warning),
        StartupGate::EnteringObsPassword(_) => (masked_password, palette.info),
        StartupGate::Skipped => ("已跳过未通过检查".to_owned(), palette.warning),
        _ => (startup.active_label().to_owned(), palette.time),
    };
    let (footer, footer_color) = match gate {
        StartupGate::Blocked if startup.obs_requires_password() => {
            ("Enter 配置 OBS · S 跳过 · Ctrl+C 退出", palette.warning)
        }
        StartupGate::Blocked => ("Enter 重试 · S 跳过 · Ctrl+C 退出", palette.warning),
        StartupGate::EnteringObsPassword(_) => {
            ("Enter 保存 · Esc 返回 · Ctrl+C 退出", palette.info)
        }
        _ => ("Ctrl+C 取消", palette.frame),
    };
    if area.width < 42 || area.height < 16 {
        let height = area.height.min(3);
        let top = area.height.saturating_sub(height) / 2;
        let compact_area = Rect::new(area.x, area.y + top, area.width, height);
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    "DANMU",
                    Style::default()
                        .fg(palette.content)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · Elazer · ", Style::default().fg(palette.rank)),
                Span::styled("elazer.wang", Style::default().fg(palette.info)),
            ])
            .alignment(Alignment::Center),
            startup_progress_line(elapsed, data_ready, 12, palette).alignment(Alignment::Center),
            Line::from(Span::styled(
                active_label.clone(),
                Style::default().fg(active_color),
            ))
            .alignment(Alignment::Center),
        ];
        frame.render_widget(Paragraph::new(Text::from(lines)), compact_area);
        return;
    }

    let border = Style::default().fg(palette.info);
    let logo_lines = vec![
        Line::from(Span::styled("╭────────────────╮", border)),
        Line::from(vec![
            Span::styled("│ ", border),
            Span::styled("●", Style::default().fg(palette.warning)),
            Span::raw("  "),
            Span::styled("●", Style::default().fg(palette.rank)),
            Span::raw("  "),
            Span::styled("●", Style::default().fg(palette.success)),
            Span::styled("        │", border),
        ]),
        Line::from(vec![
            Span::styled("│  ", border),
            Span::styled(
                "DANMU",
                Style::default()
                    .fg(palette.content)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ▸ ", border),
            Span::styled(cursor, Style::default().fg(palette.host)),
            Span::styled("     │", border),
        ]),
        Line::from(Span::styled("╰──────────╮     │", border)),
        Line::from(Span::styled("           ╰─────╯", border)),
    ];
    let total_height = 16_u16;
    let top = area.height.saturating_sub(total_height) / 2;
    let logo_area = Rect::new(area.x, area.y + top, area.width, 5);
    let identity_area = Rect::new(area.x, logo_area.y + 5, area.width, 1);
    let status_width = area.width.min(46);
    let status_area = Rect::new(
        area.x + area.width.saturating_sub(status_width) / 2,
        logo_area.y + 7,
        status_width,
        9,
    );
    frame.render_widget(
        Paragraph::new(Text::from(logo_lines)).alignment(Alignment::Center),
        logo_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("by Elazer", Style::default().fg(palette.rank)),
            Span::styled("  ·  ", Style::default().fg(palette.frame)),
            Span::styled("elazer.wang", Style::default().fg(palette.info)),
        ]))
        .alignment(Alignment::Center),
        identity_area,
    );
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                "启动自检",
                Style::default()
                    .fg(palette.content)
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
            startup_progress_line(elapsed, data_ready, 24, palette).alignment(Alignment::Center),
            Line::from(vec![
                Span::styled("› ", Style::default().fg(palette.info)),
                Span::styled(active_label, Style::default().fg(active_color)),
            ]),
            startup_check_line("本地数据", startup.local, palette),
            startup_check_line("登录状态", startup.account, palette),
            startup_check_line("B 站房间", startup.room, palette),
            startup_check_line("直播指标", startup.metrics, palette),
            startup_check_line("OBS 连接", startup.obs, palette),
            Line::from(Span::styled(footer, Style::default().fg(footer_color)))
                .alignment(Alignment::Center),
        ])),
        status_area,
    );
}

fn draw(frame: &mut ratatui::Frame, app: &mut TerminalApp) {
    app.clear_review_surface();
    let palette = app.config.palette;
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.background)),
        area,
    );
    if area.width < 32 || area.height < 7 {
        draw_compact(frame, area, app, palette);
        draw_overlays(frame, area, app, palette);
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
    let review_height =
        agent::review_height(app).min(available_after_body.saturating_sub(notice_height + 2));
    let input_height = input_area_height(app, area.width)
        .min(available_after_body.saturating_sub(notice_height + review_height))
        .max(2);
    let rows = Layout::vertical([
        Constraint::Length(status_height),
        Constraint::Min(2),
        Constraint::Length(notice_height),
        Constraint::Length(review_height),
        Constraint::Length(input_height),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(Text::from(status_lines)), rows[0]);
    draw_body(frame, rows[1], app, palette);

    if notice_height > 0 {
        frame.render_widget(Paragraph::new(Text::from(notice_lines)), rows[2]);
    }
    agent::draw_review(frame, rows[3], app);
    draw_input(frame, rows[4], app, palette);

    draw_overlays(frame, area, app, palette);
}

fn draw_overlays(frame: &mut ratatui::Frame, area: Rect, app: &mut TerminalApp, palette: Palette) {
    if app.stop_flow.is_some() {
        draw_stop_flow(frame, area, app, palette);
    } else if let Some(lines) = &app.login_qr {
        draw_qr(frame, area, lines, palette);
    } else if app.secret_mode || app.candidate_edit.is_some() {
        // The focused input already carries Enter/Esc instructions.
    } else if app
        .assistant_panel
        .as_ref()
        .is_some_and(|panel| panel.is_editing())
    {
        assistant::draw(frame, app);
    } else if app.help.is_some() {
        draw_help(frame, area, app, palette);
    } else if app.command_search_draft.is_some() || app.input.starts_with('/') {
        draw_command_palette(frame, area, app, palette);
    } else if app.assistant_panel.is_some() {
        assistant::draw(frame, app);
    }
}

fn draw_stop_flow(frame: &mut ratatui::Frame, area: Rect, app: &mut TerminalApp, palette: Palette) {
    let Some(flow) = app.stop_flow else {
        return;
    };
    let width = area.width.min(42);
    let height = area.height.min(6);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = rounded_block(" 停止推流？ ", palette);
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let text = match flow {
        StopFlow::Confirm { .. } => "直播将中断".to_owned(),
        StopFlow::Countdown { deadline } => format!(
            "{} 秒后停止推流",
            deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64()
                .ceil()
                .max(1.0) as u64
        ),
        StopFlow::Stopping => "正在停止推流…".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(palette.content)),
        inner,
    );
    if inner.height < 2 {
        return;
    }
    match flow {
        StopFlow::Confirm { stop_selected } => {
            for (row, label, selected, color) in [
                (1, "停止推流", stop_selected, Color::Red),
                (2, "取消", !stop_selected, palette.content),
            ] {
                if row < inner.height {
                    frame.render_widget(
                        Paragraph::new(format!("{} {label}", if selected { "›" } else { " " }))
                            .style(Style::default().fg(color)),
                        Rect::new(inner.x, inner.y + row, inner.width, 1),
                    );
                }
            }
            if inner.height > 3 {
                frame.render_widget(
                    Paragraph::new("↑↓选择 · Enter执行 · Esc取消")
                        .style(Style::default().fg(palette.time)),
                    Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
                );
            }
        }
        StopFlow::Countdown { .. } => {
            frame.render_widget(
                Paragraph::new("Esc 取消停止").style(Style::default().fg(palette.info)),
                Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            );
        }
        StopFlow::Stopping => {}
    }
}

fn draw_help(frame: &mut ratatui::Frame, area: Rect, app: &mut TerminalApp, palette: Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .title(" 帮助 ")
        .style(Style::default().bg(palette.background).fg(palette.content));
    let inner = block.inner(area);
    let mut lines = Vec::new();
    for (heading, body) in HELP_SECTIONS {
        lines.extend(wrap_styled_spans(
            vec![Span::styled(
                *heading,
                Style::default()
                    .fg(palette.info)
                    .add_modifier(Modifier::BOLD),
            )],
            inner.width,
            Alignment::Left,
        ));
        lines.extend(wrap_styled_spans(
            vec![Span::raw(*body)],
            inner.width,
            Alignment::Left,
        ));
        lines.push(Line::default());
    }
    if app.local_transport.is_some() {
        lines.extend(wrap_styled_spans(
            vec![Span::styled(
                "仅本地练习（不连接直播平台）",
                Style::default().fg(palette.info),
            )],
            inner.width,
            Alignment::Left,
        ));
        lines.extend(wrap_styled_spans(
            vec![Span::raw("/event 姓名 正文 注入本地弹幕；/local confirmed|uncertain|rejected 设下一次本地送达结果。\n/session end 结束本地场次；/session new 新建场次且不授权，不是 OBS 控制。")],
            inner.width,
            Alignment::Left,
        ));
    }
    let help = app.help.as_mut().expect("帮助已打开");
    help.page_rows = usize::from(inner.height).max(1);
    help.max_scroll = lines.len().saturating_sub(help.page_rows);
    help.scroll = help.scroll.min(help.max_scroll);
    let footer = format!(
        " {}/{} · ↑↓/PgDn · Esc返回 · 助手发送{} ",
        help.scroll + 1,
        help.max_scroll + 1,
        if app.bridge.sending_enabled() {
            "已许可"
        } else {
            "未许可"
        },
    );
    frame.render_widget(block.title_bottom(footer), area);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(help.scroll)
                .take(help.page_rows)
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}

fn draw_command_palette(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut TerminalApp,
    palette: Palette,
) {
    if app.candidate_edit.is_some() || app.secret_mode {
        return;
    }
    let area = Rect {
        height: area
            .height
            .saturating_sub(input_area_height(app, area.width) + 1),
        ..area
    };
    let suggestions = app.command_suggestions();
    if (suggestions.is_empty() && app.command_search_draft.is_none()) || area.height < 4 {
        return;
    }

    let capacity = usize::from(
        (area.height / 2)
            .clamp(8, 12)
            .min(area.height.saturating_sub(2)),
    );
    let visible = suggestions.len().max(1).min(capacity);
    app.slash_selection = app.slash_selection.min(suggestions.len().saturating_sub(1));
    let offset = app
        .slash_selection
        .saturating_sub(visible - 1)
        .min(suggestions.len().saturating_sub(visible));
    let height = (visible as u16 + 2).min(area.height);
    let width = area.width.saturating_sub(4).min(72);
    let popup = Rect::new(
        area.x + 2,
        area.y + area.height.saturating_sub(height),
        width,
        height,
    );
    let mut items = suggestions
        .iter()
        .skip(offset)
        .take(visible)
        .map(|suggestion| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(
                        "{}  {}   ",
                        suggestion.usage,
                        suggestion.completion.trim_end()
                    ),
                    Style::default()
                        .fg(if suggestion.danger {
                            Color::Red
                        } else {
                            suggestion.category.color(palette)
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(suggestion.description, Style::default().fg(palette.time)),
            ]))
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(ListItem::new("没有匹配项；请换个词，Esc恢复草稿"));
    }
    let mut state = ListState::default().with_selected(Some(app.slash_selection - offset));
    let title = format!(
        " 命令 · 共 {} 项 · ↑↓选择 · Enter执行 · Tab补全 ",
        suggestions.len()
    );
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .title(title)
                .title_bottom(if app.command_search_draft.is_some() {
                    " 输入中文搜索全部 · Esc恢复草稿 · 不会发送弹幕 "
                } else {
                    " 常用操作 · 输入中文搜索全部 · Ctrl-O保留草稿 "
                }),
        )
        .style(Style::default().bg(palette.background).fg(palette.content))
        .highlight_style(
            Style::default()
                .bg(palette.frame)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut state);
}

fn draw_compact(
    frame: &mut ratatui::Frame,
    mut area: Rect,
    app: &mut TerminalApp,
    palette: Palette,
) {
    if app.runner.running || app.resume_pending {
        let assistant = assistant::compact(app, area.width.saturating_sub(8));
        let room = fit_display_width(
            &format!(" ROOM {}", app.config.room_id),
            usize::from(area.width).saturating_sub(assistant.width()),
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                assistant,
                Span::styled(room, Style::default().fg(palette.content)),
            ])),
            Rect::new(area.x, area.y, area.width, area.height.min(1)),
        );
        area.y = area.y.saturating_add(1);
        area.height = area.height.saturating_sub(1);
    }
    if area.height == 0 {
        return;
    }
    if app.is_browsing_history() && area.height < 3 {
        draw_history_status(frame, area, app, palette);
        return;
    }
    if area.height < 3 {
        frame.render_widget(
            Paragraph::new(format!(
                "{} · ROOM {} · ❯ {}",
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
    if app.is_browsing_history() {
        draw_events(frame, rows[0], app, palette, "History");
        draw_input(frame, rows[1], app, palette);
        return;
    }
    let latest = display_events(&app.session.recent_events)
        .map(|event| {
            Line::from(Span::styled(
                map_event_emotes(event),
                Style::default().fg(palette.content),
            ))
        })
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
const STATUS_ORANGE: Color = Color::Rgb(194, 65, 12);
const STATUS_GREEN: Color = Color::Rgb(21, 128, 61);
const STATUS_BLUE: Color = Color::Rgb(29, 78, 216);

// Local identity emphasis, deliberately distinct from green delivery confirmation.
const ASSISTANT_MESSAGE_COLOR: Color = Color::Rgb(255, 175, 95);
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
    let indicator_on = (app.animation_tick / 2).is_multiple_of(2);
    // Preserve the original six-column indicator throughout its blink cycle.
    let live_label = if room.is_live() && !indicator_on {
        "      "
    } else {
        room_live_label(room)
    };
    let live = format!(" {live_label} ");
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
        if room.is_live() {
            Span::styled(
                live,
                Style::default()
                    .fg(Color::LightRed)
                    .bg(palette.background)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            status_span(live, STATUS_BLUE, palette)
        },
        Span::raw(" ".repeat(left_gap)),
        Span::styled(
            title,
            Style::default()
                .fg(palette.rank)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(right_gap)),
        Span::styled(
            elapsed,
            Style::default()
                .fg(palette.content)
                .bg(palette.background)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn secondary_status_line(
    room: &RoomSnapshot,
    app: &TerminalApp,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let obs_connected = app.obs_error.is_none() && app.obs_status.is_some();
    let obs_indicator = "●";
    let obs_color = if obs_connected {
        STATUS_GREEN
    } else {
        STATUS_RED
    };
    let (microphone_indicator, microphone_color) = match app
        .obs_status
        .as_ref()
        .filter(|_| app.obs_error.is_none())
        .map(|status| status.microphone)
    {
        Some(MicrophoneState::Unmuted) => ("◉", STATUS_RED),
        Some(MicrophoneState::Muted) => ("○ ", palette.rank),
        _ => ("?", palette.rank),
    };
    let devices_width = UnicodeWidthStr::width("OBS ")
        + UnicodeWidthStr::width(obs_indicator)
        + UnicodeWidthStr::width("  MIC ")
        + UnicodeWidthStr::width(microphone_indicator);
    let assistant = assistant::compact(app, width.saturating_sub(devices_width as u16 + 8));
    let assistant_width = UnicodeWidthStr::width(assistant.content.as_ref());
    let broadcaster = fit_display_width(
        &format!(" @ {} ", room.broadcaster_name),
        usize::from(width).saturating_sub(devices_width + assistant_width),
    );
    let gap = usize::from(width).saturating_sub(
        UnicodeWidthStr::width(broadcaster.as_str()) + devices_width + assistant_width,
    );
    let neutral = Style::default()
        .fg(palette.content)
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::with_capacity(6);
    if !broadcaster.is_empty() {
        spans.push(status_span(broadcaster, STATUS_GREEN, palette));
    }
    spans.push(assistant);
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
        return vec![Line::from(vec![
            assistant::compact(app, width.saturating_sub(8)),
            status_span(" ◌ ROOM ".into(), STATUS_ORANGE, palette),
        ])];
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
    let (prefix, color) = match app.notice_level {
        NoticeLevel::Info => ("·", palette.time),
        NoticeLevel::Success => ("+", palette.success),
        NoticeLevel::Progress => ("·", palette.info),
        NoticeLevel::Warning => ("!", palette.rank),
        NoticeLevel::Error => ("!", palette.warning),
    };
    vec![Line::from(Span::styled(
        format!(
            "{prefix} {}",
            fit_display_width(
                &app.notice.replace(['\n', '\r'], " "),
                usize::from(width).saturating_sub(2).min(64)
            )
        ),
        Style::default().fg(color),
    ))]
}

fn draw_history_status(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &TerminalApp,
    palette: Palette,
) {
    let first = if area.width >= 44 {
        " HISTORY · Esc to follow"
    } else if area.width >= 24 {
        " HISTORY · Esc"
    } else {
        " HISTORY"
    };
    let mut second = format!(" +{} NEW", app.unread_live_count);
    if app.config.history_idle_seconds > 0 {
        if app.stop_flow.is_some() || app.login_qr.is_some() || app.secret_mode {
            second.push_str(" · TIMER PAUSED");
        } else {
            let remaining = u64::from(app.config.history_idle_seconds)
                .saturating_sub(app.last_user_activity.elapsed().as_secs());
            second.push_str(&format!(" · AUTO {remaining}s"));
        }
    } else {
        second.push_str(" · MANUAL");
    }
    frame.render_widget(
        Paragraph::new(vec![Line::from(first), Line::from(second)]).style(
            Style::default()
                .fg(contrast_foreground(palette.warning, palette))
                .bg(palette.warning)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

fn draw_events(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut TerminalApp,
    palette: Palette,
    title: &str,
) {
    if app.is_browsing_history() && area.height < 4 {
        draw_history_status(frame, area, app, palette);
        return;
    }
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
        format!("{title} · HISTORY")
    } else {
        format!("{title} · FOLLOW")
    };
    let mut block = rounded_block(&title, palette);
    if browse_target.is_some() {
        block = block.border_style(Style::default().fg(palette.warning));
    }
    if let Some((kind, content)) = app.activity.render(
        usize::from(area.width.saturating_sub(4)),
        app.show_name,
        Instant::now(),
    ) {
        block = block.title_bottom(Line::styled(
            format!(" {content} "),
            Style::default().fg(event_color(kind, palette)),
        ));
    }
    let mut inner = block.inner(area);
    let items = displayed_events
        .into_iter()
        .map(|event| ListItem::new(event_lines(event, app, palette, inner.width)))
        .collect::<Vec<_>>();

    let scroll_target = browse_target.or_else(|| (!items.is_empty()).then(|| items.len() - 1));
    let used_height = items.iter().fold(0_u16, |height, item| {
        height.saturating_add(item.height() as u16)
    });
    frame.render_widget(block, area);
    if browse_target.is_some() {
        let height = inner.height.min(2);
        let banner = Rect {
            y: inner.bottom().saturating_sub(height),
            height,
            ..inner
        };
        draw_history_status(frame, banner, app, palette);
        inner.height = inner.height.saturating_sub(height);
    }
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

fn is_assistant_message(event: &DanmuEvent, app: &TerminalApp) -> bool {
    event.kind == DanmuEventKind::Danmu
        && app
            .assistant_accounts
            .independent_user_id(&app.account_status)
            .is_some_and(|uid| event.author_id.as_deref() == Some(uid))
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
    let assistant = is_assistant_message(event, app);
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
    if event.origin == DanmuEventOrigin::Archived {
        metadata.push(Span::styled("↶ ", Style::default().fg(palette.time)));
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
                .fg(if assistant {
                    ASSISTANT_MESSAGE_COLOR
                } else if broadcaster {
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
    if app.runner.running
        && let Some(mark) = app.bridge.mark(&event.id)
    {
        let color = match mark {
            bridge::Mark::Processing => Color::Yellow,
            bridge::Mark::Confirmed => Color::Green,
            bridge::Mark::Finished => Color::DarkGray,
            bridge::Mark::Failed => Color::Red,
        };
        metadata.push(Span::raw(" "));
        metadata.push(Span::styled("◆", Style::default().fg(color)));
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
            metadata.push(Span::raw(" "));
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
    if let Some(target) = event.reply_to.as_deref() {
        let target = clean_platform_markup(target);
        let target = target.trim();
        let already_in_body = content
            .strip_prefix('@')
            .and_then(|body| body.strip_prefix(target))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
        if !target.is_empty() && !already_in_body {
            return format!("@{target} {content}");
        }
    }
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
        "•".repeat(app.input.to_string().graphemes(true).count())
    } else {
        app.input.to_string()
    }
}

fn connection_badge(app: &TerminalApp) -> String {
    if app.connection.starts_with("已连接") {
        app.last_realtime_at
            .as_ref()
            .map(|timestamp| format!("实时 {}", timestamp.format("%H:%M:%S")))
            .unwrap_or_else(|| "已连接".into())
    } else if app.connection.contains("重连") {
        "正在重连".into()
    } else {
        "正在连接".into()
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
    let mut segments = Vec::with_capacity(3);
    let mut remaining = usize::from(width);
    for (label, value, color) in [
        ("◉", watched, palette.rank),
        ("♥", likes, palette.warning),
        ("●", online, palette.success),
    ] {
        let needed = UnicodeWidthStr::width(label) + UnicodeWidthStr::width(value.as_str()) + 3;
        if needed > remaining {
            break;
        }
        remaining -= needed;
        segments.push((format!("{label} {value}"), color));
    }
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
    let (left, right) = if app.candidate_edit.is_some() {
        (
            powerline_title(vec![(
                "编辑待确认回复 · Enter保存 · Esc取消 · 不发送".into(),
                palette.warning,
            )]),
            Line::default(),
        )
    } else if app.command_search_draft.is_some() {
        (
            Line::from(" 命令搜索 · 输入中文 · Esc恢复草稿 "),
            Line::default(),
        )
    } else if app.secret_mode {
        (
            powerline_title(vec![(
                "OBS 密码 · Enter保存 · Esc取消".into(),
                palette.warning,
            )]),
            Line::default(),
        )
    } else if app.stop_flow.is_some() {
        (
            powerline_title(vec![("停止推流 · 请在弹窗中操作".into(), palette.warning)]),
            Line::default(),
        )
    } else {
        let status = delivery_status_title(app, palette);
        let available = area
            .width
            .saturating_sub(line_display_width(&status) as u16 + 6);
        (status, input_business_title(app, palette, available))
    };

    let meter_context = if !app.secret_mode && !app.stop_flow.is_some() && app.obs_error.is_none() {
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
        if app.stop_flow.is_some() || app.assistant_panel.is_some() {
            return;
        }
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
    if app.stop_flow.is_some() || app.assistant_panel.is_some() {
        return;
    }
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
    let password = rpassword::prompt_password("OBS WebSocket 密码（留空保持当前值）：")?;
    if !password.is_empty() {
        crate::obs::save_obs_password(path, &password)?;
    }
    configuration.save(path)?;
    println!("OBS 配置已保存：{}", path.display());
    println!("OBS 密码保存在 TUI 私有文件中，不使用系统钥匙串。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::ObsConfiguration;
    use ratatui::backend::TestBackend;
    fn test_app(temp: &tempfile::TempDir, session: DanmuSession) -> TerminalApp {
        replay::app(temp.path(), session)
    }
    #[tokio::test]
    async fn startup_history_creates_a_fresh_session_without_live_metrics() {
        let temp = tempfile::tempdir().unwrap();
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
        let (updates, _) = mpsc::unbounded_channel();

        let loaded = load_startup_session(journal.clone(), "42".into(), updates).await;

        assert_ne!(loaded.session.id, archived.id);
        assert_eq!(
            loaded.session.status,
            crate::domain::DanmuSessionStatus::Active
        );
        assert_eq!(
            loaded.session.metrics,
            crate::domain::SessionMetrics::default()
        );
        assert_eq!(loaded.session.recent_events.len(), 1);
        assert_eq!(loaded.session.recent_events[0].id, event.id);
        assert_eq!(
            loaded.session.recent_events[0].origin,
            DanmuEventOrigin::Archived
        );
        journal.start(&loaded.session).unwrap();
        assert_eq!(std::fs::read(source).unwrap(), original);
    }

    #[test]
    fn independent_assistant_message_color_requires_uid_not_name_or_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        crate::storage::write_private_atomic(
            &temp.path().join("assistant-account.json"),
            &serde_json::to_vec(&serde_json::json!({"mode":"independent", "account":id})).unwrap(),
        )
        .unwrap();
        crate::storage::write_private_atomic(
            &temp.path().join(format!("AssistantAccounts/{id}.json")),
            &serde_json::to_vec(&serde_json::json!({
                "cookieHeader":"SESSDATA=fake-local; bili_jct=fake-local; DedeUserID=22",
                "csrf":"fake-local",
                "identity":{"SignedIn":{"display_name":"助手", "user_id":"22"}}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "✦ 同名同前缀回复");
        event.username = Some("助手".into());
        let emphasized = |event: &DanmuEvent, app: &TerminalApp| {
            event_lines(event, app, app.config.palette, 80)
                .lines
                .into_iter()
                .flat_map(|line| line.spans)
                .filter(|span| span.style.fg == Some(ASSISTANT_MESSAGE_COLOR))
                .map(|span| span.content.into_owned())
                .collect::<Vec<_>>()
        };
        for uid in [None, Some("33")] {
            event.author_id = uid.map(str::to_owned);
            assert!(emphasized(&event, &app).is_empty());
        }
        event.author_id = Some("22".into());
        for running in [true, false] {
            app.runner.running = running;
            assert_eq!(emphasized(&event, &app), ["助手"]);
        }
        let body = event_lines(&event, &app, app.config.palette, 80);
        let body = body
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("同名同前缀回复"))
            .unwrap();
        assert_eq!(body.style.fg, Some(app.config.palette.content));
        assert!(!body.style.add_modifier.contains(Modifier::BOLD));

        app.show_name = false;
        assert!(emphasized(&event, &app).is_empty());
        app.account_status = AccountStatus::SignedIn {
            display_name: "主账号".into(),
            user_id: "22".into(),
        };
        assert!(emphasized(&event, &app).is_empty());
        app.account_status = AccountStatus::SignedOut;
        app.assistant_accounts.reuse_main().unwrap();
        assert!(emphasized(&event, &app).is_empty());
    }

    #[tokio::test]
    async fn stop_flow_requires_selection_and_supports_cancelling_countdown() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(8);
        let now = Instant::now();
        app.command("/obs stop", tx.clone()).await.unwrap();
        assert!(
            !app.handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                tx.clone()
            )
            .await
            .unwrap()
        );
        app.advance_stop_at(now + Duration::from_secs(10), &tx);
        assert!(app.stop_flow.is_none());

        app.command("/obs stop", tx.clone()).await.unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        // Holding Enter must not confirm through a repeat event.
        app.handle_key(
            KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Repeat,
            ),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(matches!(app.stop_flow, Some(StopFlow::Confirm { .. })));
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(matches!(app.stop_flow, Some(StopFlow::Countdown { .. })));
        app.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.input.is_empty());
        assert!(
            !app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap()
        );
        app.advance_stop_at(now + Duration::from_secs(10), &tx);
        assert!(app.stop_flow.is_none());

        app.command("/obs stop", tx.clone()).await.unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(
            app.stop_flow.is_none(),
            "reopening must reset selection to return"
        );
    }

    #[tokio::test]
    async fn stop_flow_sends_one_request_only_after_three_seconds() {
        use futures_util::SinkExt;
        use serde_json::{Value, json};
        use tokio::net::TcpListener;
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.obs = ObsController::new(
            ObsConfiguration {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                ..ObsConfiguration::default()
            },
            temp.path().join("obs.json"),
        );
        let (tx, mut rx) = mpsc::channel(8);
        app.command("/obs stop", tx.clone()).await.unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(matches!(
            app.stop_flow,
            Some(StopFlow::Confirm {
                stop_selected: false
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(app.stop_flow.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        app.command("/obs stop", tx.clone()).await.unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        let Some(StopFlow::Countdown { deadline }) = app.stop_flow else {
            panic!("countdown expected");
        };
        let now = deadline - Duration::from_secs(3);
        app.advance_stop_at(now + Duration::from_millis(2999), &tx);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );

        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            ws.send(Message::Text(
                json!({"op": 0, "d": {"obsWebSocketVersion": "5.6.0", "rpcVersion": 1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let identify: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(identify["op"], 1);
            ws.send(Message::Text(
                json!({"op": 2, "d": {"negotiatedRpcVersion": 1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let mut stops = 0;
            while let Ok(message) =
                tokio::time::timeout(Duration::from_millis(100), ws.next()).await
            {
                let request: Value =
                    serde_json::from_str(message.unwrap().unwrap().to_text().unwrap()).unwrap();
                let response = match request["d"]["requestType"].as_str().unwrap() {
                    "GetVersion" => json!({
                        "obsVersion": "31.0.0", "obsWebSocketVersion": "5.6.0", "rpcVersion": 1,
                        "availableRequests": [], "supportedImageFormats": [],
                        "platform": "macos", "platformDescription": "macOS"
                    }),
                    "StopStream" => {
                        stops += 1;
                        json!({})
                    }
                    "GetStreamStatus" => json!({
                        "outputActive": false, "outputReconnecting": false,
                        "outputTimecode": "00:00:00.000", "outputDuration": 0,
                        "outputCongestion": 0.0, "outputBytes": 0,
                        "outputSkippedFrames": 0, "outputTotalFrames": 0
                    }),
                    other => panic!("unexpected OBS request: {other}"),
                };
                let mut reply = json!({"op": 7, "d": {
                    "requestType": request["d"]["requestType"], "requestId": request["d"]["requestId"],
                    "requestStatus": {"result": true, "code": 100}
                }});
                if request["d"]["requestType"] != "StopStream" {
                    reply["d"]["responseData"] = response;
                }
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
            assert_eq!(stops, 1, "stop must be sent only once");
        });
        app.advance_stop_at(now + Duration::from_secs(3), &tx);
        app.advance_stop_at(now + Duration::from_secs(4), &tx);
        let result = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, UiEvent::ObsStopDone(Ok(()))), "{result:?}");
        app.handle_ui_event(result);
        assert!(app.stop_flow.is_none());
        assert!(!app.quit_requested);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn microphone_shortcuts_are_idempotent_and_require_remote_confirmation() {
        use futures_util::SinkExt;
        use serde_json::{Value, json};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        let temp = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.obs = ObsController::new(
            crate::obs::ObsConfiguration {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                ..Default::default()
            },
            temp.path().join("obs.json"),
        );
        let muted = Arc::new(AtomicBool::new(false));
        let ignore_change = Arc::new(AtomicBool::new(false));
        let remote = muted.clone();
        let ignore = ignore_change.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            ws.send(Message::Text(
                json!({"op":0,"d":{"obsWebSocketVersion":"5.6.0","rpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            while let Some(Ok(message)) = ws.next().await {
                if message.is_close() {
                    break;
                }
                let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                let response = match request["d"]["requestType"].as_str().unwrap() {
                    "GetVersion" => {
                        json!({"obsVersion":"31.0.0","obsWebSocketVersion":"5.6.0","rpcVersion":1,"availableRequests":[],"supportedImageFormats":[],"platform":"macos","platformDescription":"macOS"})
                    }
                    "SetInputMute" => {
                        if !ignore.load(Ordering::SeqCst) {
                            remote.store(
                                request["d"]["requestData"]["inputMuted"].as_bool().unwrap(),
                                Ordering::SeqCst,
                            );
                        }
                        json!({})
                    }
                    "GetInputMute" => json!({"inputMuted":remote.load(Ordering::SeqCst)}),
                    other => panic!("unexpected OBS operation: {other}"),
                };
                let mut reply = json!({"op":7,"d":{
                    "requestType":request["d"]["requestType"],"requestId":request["d"]["requestId"],
                    "requestStatus":{"result":true,"code":100}
                }});
                if request["d"]["requestType"] != "SetInputMute" {
                    reply["d"]["responseData"] = response;
                }
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let (tx, mut rx) = mpsc::channel(4);
        app.input = "人工草稿".into();
        for (command, expected) in [("/mute", true), ("/mute", true), ("/unmute", false)] {
            app.open_commands();
            app.handle_paste(command);
            app.handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                tx.clone(),
            )
            .await
            .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap();
            app.handle_ui_event(result);
            assert!(
                matches!(app.notice_level, NoticeLevel::Success),
                "{}",
                app.notice
            );
            assert_eq!(muted.load(Ordering::SeqCst), expected);
            assert_eq!(app.input, "人工草稿");
        }
        ignore_change.store(true, Ordering::SeqCst);
        app.command("/mute", tx).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        app.handle_ui_event(result);
        assert!(
            matches!(app.notice_level, NoticeLevel::Error),
            "{}",
            app.notice
        );
        assert!(!muted.load(Ordering::SeqCst));
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn gift_combo_renders_one_row_without_moving_past_newer_chat() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        for index in 0..2 {
            let gift = crate::domain::GiftDetails {
                name: "人气票".into(),
                action: "投喂".into(),
                blind_gift: None,
                receipts: [(format!("receipt-{index}"), 1)].into(),
                reported_quantity: 0,
            };
            let mut event = DanmuEvent::new(DanmuEventKind::Gift, gift.content());
            event.id = "one-gift-batch".into();
            event.gift = Some(Box::new(gift));
            app.ingest_event(event);
            if index == 0 {
                app.ingest_event(DanmuEvent::new(DanmuEventKind::Danmu, "后来的聊天"));
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let rendered = (0..20)
            .map(|y| {
                (0..100)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace(' ', "");
        assert_eq!(rendered.matches("人气票").count(), 1, "{rendered}");
        assert!(rendered.contains("人气票×2"), "{rendered}");
        assert!(
            rendered.find("人气票").unwrap() < rendered.find("后来的聊天").unwrap(),
            "{rendered}"
        );
    }

    #[test]
    fn optional_startup_checks_do_not_block_and_password_stays_private() {
        let palette = Palette::default();
        let mut startup = StartupView::new();
        startup.account = StartupCheck::Passed("未登录 · 监看模式");
        startup.room = StartupCheck::Passed("房间可访问");
        startup.metrics = StartupCheck::Skipped("未登录 · 已跳过");
        startup.obs = StartupCheck::Warning("需要密码 · /obs config password");
        startup.local = StartupCheck::Passed("已读取");
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();

        assert!(!startup.has_warning());
        startup.account = StartupCheck::Warning("账号未登录");
        startup.metrics = StartupCheck::Warning("指标不可用");
        assert!(!startup.has_warning());
        startup.room = StartupCheck::Warning("直播间不可访问");
        assert!(startup.has_warning());

        let password_gate = StartupGate::EnteringObsPassword(EditorInput::from("secret"));
        terminal
            .draw(|frame| {
                draw_startup(
                    frame,
                    palette,
                    0,
                    &startup,
                    &password_gate,
                    Duration::ZERO,
                    true,
                )
            })
            .unwrap();
        let password_frame = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(password_frame.contains("●●●●●●"), "{password_frame}");
        assert!(!password_frame.contains("secret"), "{password_frame}");
        assert!(StartupGate::Skipped.can_finish());
    }

    #[test]
    fn startup_animation_degrades_to_the_active_compact_check() {
        let startup = StartupView::new();
        let mut terminal = Terminal::new(TestBackend::new(30, 3)).unwrap();
        terminal
            .draw(|frame| {
                draw_startup(
                    frame,
                    Palette::default(),
                    2,
                    &startup,
                    &StartupGate::Checking,
                    Duration::from_secs(1),
                    false,
                )
            })
            .unwrap();
        let rendered = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("DANMU"));
        assert!(rendered.contains("Elazer"), "{rendered}");
        assert!(rendered.contains("elazer.wang"), "{rendered}");
        assert!(
            rendered.replace(' ', "").contains("恢复本地会话"),
            "{rendered}"
        );
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
        let frame_top = lines.iter().position(|line| line.starts_with('╭')).unwrap();
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
    fn activity_bursts_and_expiry_leave_chat_rows_unchanged() {
        for chat_layout in [false, true] {
            for (browsing, selecting) in [(false, false), (true, false), (false, true)] {
                let temp = tempfile::tempdir().unwrap();
                let now = Utc::now();
                let mut session = DanmuSession::with_options("1", 12, now);
                for index in 0..12 {
                    let mut event = DanmuEvent::new(
                        DanmuEventKind::Danmu,
                        format!("历史弹幕-{index:02}，这是一条需要保留的消息"),
                    );
                    event.timestamp = now - chrono::Duration::seconds(60 - index);
                    session.ingest(event);
                }
                let mut app = test_app(&temp, session);
                app.layout_chat = chat_layout;
                if browsing {
                    app.scroll_offset = 4;
                }
                if selecting {
                    app.selection_active = true;
                    app.selected = 4;
                }
                let palette = app.config.palette;
                let mut terminal = Terminal::new(TestBackend::new(42, 10)).unwrap();
                terminal
                    .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
                    .unwrap();
                let before = terminal.backend().buffer().clone();
                for index in 0..30 {
                    let kind = if index % 2 == 0 {
                        DanmuEventKind::Enter
                    } else {
                        DanmuEventKind::Like
                    };
                    let mut event = DanmuEvent::new(kind, "临时互动提示");
                    event.timestamp = now + chrono::Duration::minutes(1);
                    app.ingest_event(event);
                    terminal
                        .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
                        .unwrap();
                    let during = terminal.backend().buffer();
                    for y in 0..9 {
                        for x in 0..42 {
                            assert_eq!(
                                during[(x, y)],
                                before[(x, y)],
                                "chat={chat_layout}, browsing={browsing}, selecting={selecting}, cell=({x},{y})"
                            );
                        }
                    }
                    let mut footer = String::new();
                    let mut x = 0;
                    while x < 42 {
                        let symbol = during[(x, 9)].symbol();
                        footer.push_str(symbol);
                        x += UnicodeWidthStr::width(symbol).max(1) as u16;
                    }
                    assert!(footer.contains("观众"), "{footer}");
                }
                app.expire_notice_at(Instant::now() + Duration::from_secs(10));
                terminal
                    .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
                    .unwrap();
                assert_eq!(terminal.backend().buffer(), &before);
            }
        }
    }

    #[test]
    fn history_scrolling_skips_activity_even_after_it_expires() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let now = Utc::now();
        for index in 0..10 {
            let mut message = DanmuEvent::new(DanmuEventKind::Danmu, format!("message-{index}"));
            message.timestamp = now - chrono::Duration::seconds(30 - index * 2);
            app.ingest_event(message);
            let mut activity = DanmuEvent::new(DanmuEventKind::Like, "过期的点赞");
            activity.timestamp = now - chrono::Duration::seconds(29 - index * 2);
            app.ingest_event(activity);
        }
        for (older, expected) in [
            (true, "message-8"),
            (true, "message-7"),
            (false, "message-8"),
        ] {
            app.scroll_history(older);
            assert_eq!(
                app.session.recent_events[app.scroll_offset].content,
                expected
            );
        }
        app.unread_live_count = 2;
        app.scroll_history(false);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_live_count, 0);
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
    fn viewer_reply_target_survives_history_reconciliation() {
        use serde_json::json;
        for target in ["另一位观众", "拾穗数据"] {
            let mut metadata = vec![serde_json::Value::Null; 16];
            metadata[15] = json!({"extra": json!({
                "show_reply": true, "reply_mid": 99, "reply_uname": target
            }).to_string()});
            let live = crate::bilibili::parse_command(&json!({
                "cmd": "DANMU_MSG", "info": [metadata, "这个问题怎么看？", [42, "观众"]]
            }))
            .unwrap();
            let mut history = DanmuEvent::new(DanmuEventKind::Danmu, "这个问题怎么看？");
            history.username = Some("观众".into());
            history.author_id = Some("42".into());
            history.timestamp = live.timestamp;
            history.origin = DanmuEventOrigin::History;
            let expected = format!("@{target} 这个问题怎么看？");
            assert_eq!(visible_event_content(&live), expected);
            for (first, second) in [(live.clone(), history.clone()), (history, live)] {
                let mut events = vec![first];
                assert!(reconcile_cross_origin_event(&mut events, &second));
                assert_eq!(visible_event_content(&events[0]), expected);
                assert_eq!(events[0].content, "这个问题怎么看？");
            }
        }
    }

    #[test]
    fn reply_metadata_does_not_drop_body_or_duplicate_text_mentions() {
        use serde_json::json;
        let cases = [
            (serde_json::Value::Null, "普通弹幕", "普通弹幕"),
            (json!("{broken"), "普通弹幕", "普通弹幕"),
            (
                json!(r#"{"show_reply":false,"reply_uname":"观众"}"#),
                "普通弹幕",
                "普通弹幕",
            ),
            (
                json!(r#"{"show_reply":true,"reply_uname":" "}"#),
                "普通弹幕",
                "普通弹幕",
            ),
            (
                json!(r#"{"reply_uname":"观众"}"#),
                "@观众 你好",
                "@观众 你好",
            ),
            (
                json!(r#"{"reply_uname":"观众"}"#),
                "@观众甲 你好",
                "@观众 @观众甲 你好",
            ),
        ];
        for (extra, body, expected) in cases {
            let mut metadata = vec![serde_json::Value::Null; 16];
            metadata[15] = json!({"extra": extra});
            let event = crate::bilibili::parse_command(&json!({
                "cmd": "DANMU_MSG", "info": [metadata, body, [42, "观众乙"]]
            }))
            .unwrap();
            assert_eq!(visible_event_content(&event), expected);
        }
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
        assert!(text.contains("战区超人 来了"));
        assert!(!text.contains("<%"));
        assert!(!text.contains("%>"));
        assert!(
            line.spans
                .iter()
                .any(|span| span.style.bg == Some(palette.info))
        );
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
    async fn pin_command_keeps_the_selected_message_when_live_messages_arrive() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        let mut question = DanmuEvent::new(DanmuEventKind::Danmu, "需要重点回答的问题");
        question.username = Some("观众".into());
        let question_id = question.id.clone();
        session.ingest(question);
        let mut app = test_app(&temp, session);
        let (tx, _rx) = mpsc::channel(1);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT), tx.clone())
            .await
            .unwrap();
        for character in "/pin".chars() {
            app.handle_key(
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                tx.clone(),
            )
            .await
            .unwrap();
        }
        let mut incoming = DanmuEvent::new(DanmuEventKind::Danmu, "稍后到达的新消息");
        incoming.origin = DanmuEventOrigin::Live;
        app.handle_client_event(BilibiliClientEvent::Danmu(incoming));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(
            app.session.featured_event.as_ref().map(|event| &event.id),
            Some(&question_id)
        );
        assert!(app.input.is_empty());
    }

    #[tokio::test]
    async fn login_qr_consumes_keys_before_the_underlying_settings_panel() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.input = "人工草稿".into();
        app.open_settings().unwrap();
        let token = uuid::Uuid::new_v4();
        app.main_login = Some(MainLogin {
            token,
            task: tokio::spawn(std::future::pending()),
        });
        app.handle_ui_event(UiEvent::LoginQr {
            token,
            lines: vec!["QR".into()],
        });
        for code in [KeyCode::Enter, KeyCode::Char('x')] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(app.login_qr.is_none());
        assert!(app.assistant_panel.is_some());
        let notice = app.notice.clone();
        app.handle_ui_event(UiEvent::LoginQr {
            token,
            lines: vec!["迟到二维码".into()],
        });
        app.handle_ui_event(UiEvent::LoginDone {
            token,
            account: app.account.staged(),
            status: AccountStatus::SignedIn {
                display_name: "迟到账号".into(),
                user_id: "99".into(),
            },
        });
        assert!(app.login_qr.is_none());
        assert!(matches!(app.account_status, AccountStatus::SignedOut));
        assert_eq!(app.notice, notice);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.assistant_panel.is_none());
        assert_eq!(&*app.input, "人工草稿");
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
        for _ in 0..3 {
            let quit_app = app
                .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap();
            assert!(!quit_app);
        }
        assert!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), tx)
                .await
                .unwrap()
        );
    }
    #[test]
    fn successful_delivery_clears_only_transient_delivery_notices() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_ui_event(UiEvent::DeliveryNotice("未确认送达".into()));
        assert!(app.notice_deadline.is_some());
        app.handle_ui_event(UiEvent::DeliveryCompleted);
        assert!(app.notice.is_empty());
        app.handle_ui_event(UiEvent::DeliveryNotice("再次未确认".into()));
        app.expire_notice_at(Instant::now() + DELIVERY_NOTICE_LIFETIME);
        assert!(app.notice.is_empty());
        app.handle_ui_event(UiEvent::error("登录态需处理"));
        app.handle_ui_event(UiEvent::DeliveryCompleted);
        assert_eq!(app.notice, "登录态需处理");
    }
    #[tokio::test]
    async fn post_waits_for_echo_matcher_and_fast_echo_confirms_once() {
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
        let endpoint =
            url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
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

        let temp = tempfile::tempdir().unwrap();
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
        let transport = TerminalTransport {
            account: accounts::SendIdentity::manual(account),
            client: BilibiliClient::new(temp.path().join("live-session.json")).unwrap(),
            room: "1".into(),
            tx,
            job: None,
            bridge: bridge::Bridge::new(true),
            queue: SendQueue::default(),
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
            "UI尚未注册回声匹配器时不得开始POST"
        );

        let mut app = test_app(&temp, DanmuSession::new("1"));
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
    fn late_echo_keeps_its_identity_and_expires_at_the_delivery_window() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let submitted = Utc::now();
        let delivery =
            PendingDelivery::new("late reply".into(), "host".into(), "42".into(), submitted);
        let mut boundary = DanmuEvent::new(DanmuEventKind::Danmu, "late reply");
        boundary.author_id = Some("42".into());
        boundary.timestamp = submitted + chrono::Duration::seconds(90);
        assert!(delivery.matches(&boundary));
        boundary.timestamp += chrono::Duration::milliseconds(1);
        assert!(!delivery.matches(&boundary));
        let id = delivery.id.clone();
        let (tx, mut rx) = oneshot::channel();
        let (registered_tx, _registered_rx) = oneshot::channel();
        app.handle_ui_event(UiEvent::DeliveryStarted {
            delivery,
            confirmation: tx,
            registered: registered_tx,
        });
        app.handle_ui_event(UiEvent::DeliveryEchoMissing {
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
        app.handle_ui_event(UiEvent::DeliveryCompleted);
        assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
        app.handle_ui_event(UiEvent::DeliveryEchoMissing {
            delivery_ids: vec![id],
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
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
            registered: oneshot::channel().0,
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Sending);
        app.handle_ui_event(UiEvent::DeliveryAccepted);
        assert_eq!(app.delivery_status, DeliveryStatus::AwaitingEcho);
        app.animation_tick = 1;

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
        app.handle_ui_event(UiEvent::DeliveryCompleted);
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
            registered: oneshot::channel().0,
        });
        app.handle_ui_event(UiEvent::DeliveryStarted {
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
        app.handle_ui_event(UiEvent::DeliveryCompleted);
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
            registered: oneshot::channel().0,
        });
        app.handle_ui_event(UiEvent::DeliveryHistory { events: Vec::new() });
        assert_eq!(app.delivery_status, DeliveryStatus::Verifying);
        app.handle_ui_event(UiEvent::DeliveryTimedOut {
            delivery_ids: vec![delivery_id],
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Uncertain);
        assert!(app.notice.contains("待确认消息"));

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

    #[tokio::test]
    async fn history_idle_timeout_tracks_input_not_incoming_messages() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..8 {
            session.ingest(DanmuEvent::new(
                DanmuEventKind::Danmu,
                format!("历史-{index}"),
            ));
        }
        let mut app = test_app(&temp, session);
        let (tx, _rx) = mpsc::channel(1);
        app.config.history_idle_seconds = 60;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        // A fresh input must restart a previously expired idle period.
        app.last_user_activity = Instant::now() - Duration::from_secs(120);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        let input_at = app.last_user_activity;
        let anchor = app.session.recent_events[app.scroll_offset].id.clone();
        app.advance_history_at(input_at + Duration::from_secs(59));
        assert_eq!(app.session.recent_events[app.scroll_offset].id, anchor);
        assert!(app.is_browsing_history());
        let mut latest = DanmuEvent::new(DanmuEventKind::Danmu, "超时后应该看到的最新弹幕");
        latest.origin = DanmuEventOrigin::Live;
        app.handle_client_event(BilibiliClientEvent::Danmu(latest));
        assert_eq!(app.unread_live_count, 1);
        assert_eq!(app.session.recent_events[app.scroll_offset].id, anchor);
        app.advance_history_at(input_at + Duration::from_secs(60));
        assert!(!app.is_browsing_history());
        assert_eq!(app.unread_live_count, 0);
        assert_eq!(&*app.input, "x");

        app.config.history_idle_seconds = 0;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        app.advance_history_at(app.last_user_activity + Duration::from_secs(3600));
        assert!(app.is_browsing_history());
        assert!(
            !app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
                .await
                .unwrap()
        );
        assert!(!app.is_browsing_history());
        assert_eq!(&*app.input, "x");
    }

    #[test]
    fn history_idle_timeout_waits_for_modal_operation_to_finish() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        session.ingest(DanmuEvent::new(DanmuEventKind::Danmu, "正在准备回复的问题"));
        let mut app = test_app(&temp, session);
        app.selection_active = true;
        app.config.history_idle_seconds = 60;
        app.stop_flow = Some(StopFlow::Confirm {
            stop_selected: false,
        });
        let now = app.last_user_activity + Duration::from_secs(120);
        app.advance_history_at(now);
        assert!(app.selection_active);
        app.cancel_stop();
        app.advance_history_at(now + Duration::from_secs(59));
        assert!(app.selection_active);
        app.advance_history_at(now + Duration::from_secs(60));
        assert!(!app.is_browsing_history());
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
    async fn chinese_archive_search_completes_without_execution() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.input = "/搜索".into();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(app.input.trim(), "/find");
    }

    #[tokio::test]
    async fn paste_stays_in_draft_and_cannot_cross_modal_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_paste("/quit\r\n/obs start\t\u{3}\u{10}");
        assert_eq!(app.input, "/quit /obs start ");
        assert!(!app.quit_requested);
        assert!(app.stop_flow.is_none());
        assert!(app.pending_deliveries.is_empty());

        app.input = "保留人工草稿".into();
        app.stop_flow = Some(StopFlow::Confirm {
            stop_selected: true,
        });
        app.handle_paste("\r\n/quit\u{1b}");
        assert!(matches!(
            app.stop_flow,
            Some(StopFlow::Confirm {
                stop_selected: true
            })
        ));
        assert_eq!(app.input, "保留人工草稿");
        app.stop_flow = None;
        app.login_qr = Some(vec!["QR".into()]);
        app.handle_paste("/quit\r");
        assert_eq!(app.input, "保留人工草稿");
        assert!(app.login_qr.is_some());
        app.login_qr = None;
        app.selection_active = true;
        app.handle_paste("不应编辑回复对象下的草稿\n");
        assert_eq!(app.input, "保留人工草稿");
    }

    #[tokio::test]
    async fn secret_paste_stays_hidden_and_bypasses_slash_completion() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.secret_mode = true;
        app.handle_paste("/obs config password\r\nsecret-marker\u{3}");
        let secret = app.input.to_string();
        assert_eq!(secret, "/obs config password secret-marker");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert_eq!(app.input, secret.as_str());
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let visible = format!("{:?}", terminal.backend().buffer());
        assert!(!visible.contains("secret-marker"));
        assert!(!visible.contains("/obs config password"));
        assert!(!app.notice.contains("secret-marker"));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(!app.secret_mode);
        assert!(app.input.is_empty());
        assert!(app.session.recent_events.is_empty());
    }

    #[tokio::test]
    async fn command_search_restores_cursor_and_never_sends_query_text() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, mut rx) = mpsc::channel(8);
        app.input = "草稿甲🙂乙".into();
        app.input.set_cursor(3);
        app.selection_active = true;
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_paste("没有匹配的中文搜索");
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.command_search_draft.is_some());
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);
        assert!(rx.try_recv().is_err());
        app.handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(!app.runner.running);
        assert!(!app.bridge.sending_enabled());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert_eq!(app.input, "草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
        assert!(app.selection_active);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_paste("/ai materials");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.assistant_panel.is_some());
        assert!(app.command_search_draft.is_none());
        assert_eq!(app.input, "草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn command_search_obs_submenu_and_password_preserve_the_original_draft() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(8);
        app.input = "未完成的人工回复".into();
        app.input.set_cursor(2);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.input.replace("/obs".into());
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert_eq!(app.input, "/obs ");
        assert!(app.command_search_draft.is_some());
        app.input.replace("/obs config password".into());
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.secret_mode);
        assert!(app.input.is_empty());
        app.handle_paste("secret");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(app.input, "未完成的人工回复");
        assert_eq!(app.input.cursor(), 2);
        assert!(!app.secret_mode);
    }

    #[test]
    fn optional_ai_commands_stay_out_of_the_idle_palette() {
        let idle = command_suggestions("", false);
        assert!(idle.iter().any(|spec| spec.completion == "/settings"));
        assert!(idle.iter().any(|spec| spec.completion == "/diag"));
        assert!(!idle.iter().any(|spec| ai_command(spec)));

        let explicit = command_suggestions("/ai", false);
        assert!(explicit.iter().any(|spec| spec.completion == "/ai"));

        let active = command_suggestions("", true);
        assert!(active.iter().any(|spec| spec.completion == "/review"));
        assert!(!active.iter().any(|spec| spec.completion.contains(' ')));
        assert!(
            command_suggestions("模型", false)
                .iter()
                .any(|spec| spec.completion == "/ai model")
        );
        assert!(
            command_suggestions("静音", false)
                .iter()
                .any(|spec| spec.completion == "/mute")
        );
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
            microphone: crate::obs::MicrophoneState::Muted,
            compatibility_warning: None,
        })));
        assert!(app.obs_status.is_some());
        assert!(app.obs_error.is_none());
        assert_eq!(app.runner.microphone_context()["state"], "muted");

        app.handle_ui_event(UiEvent::ObsStatus(Err("OBS WebSocket 连接失败".into())));
        assert!(app.obs_status.is_none());
        assert_eq!(app.obs_error.as_deref(), Some("OBS WebSocket 连接失败"));
        assert!(app.obs_checked_at.is_some());
        let disconnected = app.runner.microphone_context();
        assert_eq!(disconnected["state"], "unknown");
        assert_eq!(disconnected["freshness"], "unavailable");
        assert!(disconnected["age_ms"].is_null());

        let mut terminal = Terminal::new(TestBackend::new(120, 2)).unwrap();
        let palette = app.config.palette;
        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, palette))
            .unwrap();
        let rendered = (0..terminal.backend().buffer().area.width)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>()
            .replace(' ', "");

        assert!(!rendered.contains("OBS"), "{rendered}");

        app.handle_ui_event(UiEvent::ObsStatus(Ok(ObsStatus {
            current_scene: "直播".into(),
            stream: crate::obs::StreamState::Live,
            microphone: MicrophoneState::Unmuted,
            compatibility_warning: None,
        })));
        assert_eq!(app.runner.microphone_context()["state"], "unmuted");
        assert_eq!(
            app.runner.microphone_context()["speech_activity"],
            "unknown"
        );
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

    #[tokio::test]
    async fn mouse_and_arrow_history_navigation_preserve_the_draft() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..8 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("消息 {index}"));
            event.id = index.to_string();
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
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
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 6"
        );
        assert!(!app.selection_active);
        app.handle_mouse(wheel_down);
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.selection_active);

        app.selection_active = true;
        app.selected = 3;
        app.handle_mouse(wheel_up);
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 3"
        );
        assert!(!app.selection_active);
        let mut arrival = DanmuEvent::new(DanmuEventKind::Danmu, "新到达的消息");
        arrival.timestamp = Utc::now() + chrono::Duration::seconds(1);
        app.ingest_event(arrival);
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 3"
        );
        assert_eq!(app.unread_live_count, 1);

        for _ in 0..20 {
            app.handle_mouse(wheel_up);
        }
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 0"
        );
        for _ in 0..20 {
            app.handle_mouse(wheel_down);
        }
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_live_count, 0);
        app.input = "草稿🙂保留".into();
        app.input.set_cursor(2);
        let (tx, _rx) = mpsc::channel(1);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 7"
        );
        assert!(!app.selection_active);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.input, "草稿🙂保留");
        assert_eq!(app.input.cursor(), 2);
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
    async fn keyboard_palette_restores_unicode_draft_and_direct_page_closes_once() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, mut rx) = mpsc::channel(8);
        app.input = "甲🙂乙".into();
        app.input.set_cursor(1);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_paste("模型");
        let index = app
            .command_suggestions()
            .iter()
            .position(|s| s.completion == "/ai model")
            .unwrap();
        for _ in 0..index {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap();
        }
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert_eq!(app.input, "甲🙂乙");
        assert_eq!(app.input.cursor(), 1);
        assert!(app.assistant_panel.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(app.assistant_panel.is_none());
        app.command("/ai model", tx.clone()).await.unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.assistant_panel.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn palette_opens_optional_argument_editor_and_preserves_unicode_parameters() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.open_commands();
        let (tx, _rx) = mpsc::channel(1);
        app.input.replace("/scene".into());
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(
            app.assistant_panel.is_none(),
            "Tab must not activate the editor"
        );
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(
            app.assistant_panel
                .as_ref()
                .is_some_and(assistant::Panel::is_editing)
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        app.open_commands();
        app.input.replace("/find 档案 甲".into());
        // Exact typed arguments use the existing raw-command route, not completion replacement.
        assert!(
            matches!(app.handle_slash_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), SlashKeyAction::Submit(raw) if raw == "/find 档案 甲")
        );
    }

    #[tokio::test]
    async fn slash_obs_reports_progress_and_quit_exits() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(4);

        app.command("/obs", tx.clone()).await.unwrap();
        assert!(matches!(app.notice_level, NoticeLevel::Progress));
        app.command("/quit", tx).await.unwrap();
        assert!(app.quit_requested);
    }

    #[tokio::test]
    async fn explicit_display_setting_persists_even_when_runtime_override_already_matches() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.config.save_value("show_time", true.into()).unwrap();
        app.show_time = false;
        let (tx, _rx) = mpsc::channel(1);
        app.command("/display time off", tx).await.unwrap();
        let saved: toml::Value =
            toml::from_str(&std::fs::read_to_string(&app.config.config_path).unwrap()).unwrap();
        assert_eq!(saved["show_time"].as_bool(), Some(false));
    }
}
