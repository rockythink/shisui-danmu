pub(crate) mod accounts;
pub(crate) mod assistant;
pub mod commands;
pub(crate) mod diagnostics;
pub mod error;
pub mod events;
pub(crate) mod send;
pub mod settings;
use crate::{
    bilibili::{
        AccountClient, AccountStatus, BilibiliClient, BilibiliClientEvent, LoginPoll,
        RoomLiveStatus, RoomSnapshot, cross_origin_duplicate, masked_name_matches,
        normalized_message, segment_message, usable_author_id,
    },
    bridge::{self, Bridge},
    delivery::{Outcome, SendQueue, Transport},
    domain::{DanmuEvent, DanmuEventKind, DanmuEventOrigin, DanmuSession, DanmuSessionEndReason},
    obs::{MicrophoneLevel, MicrophoneState, ObsController, ObsStatus},
    persistence::SessionJournal,
    storage::StoragePaths,
};
#[cfg(feature = "tui")]
use crate::{
    config::TerminalConfig,
    terminal::{
        self, HelpState, SecretTarget, StartupGate, StartupView, activity, input::EditorInput,
    },
};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local, Utc};
pub use commands::*;
use error::{RuntimeFailure, RuntimeFailureKind};
pub use events::*;
use send::RuntimeTransport;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};
use unicode_segmentation::UnicodeSegmentation;
pub(crate) const DELIVERY_NOTICE_LIFETIME: Duration = Duration::from_secs(15);
pub(crate) const DELIVERY_ECHO_WINDOW: chrono::Duration = chrono::Duration::seconds(90);
pub(crate) struct ChatgptOperation {
    pub(crate) token: uuid::Uuid,
    pub(crate) account_id: Option<String>,
    pub(crate) scene_id: String,
    pub(crate) account_generation: u64,
    pub(crate) task: tokio::task::JoinHandle<()>,
}
impl Drop for ChatgptOperation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) struct MainLogin {
    pub(crate) token: uuid::Uuid,
    pub(crate) task: tokio::task::JoinHandle<()>,
}
impl Drop for MainLogin {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) enum CoreEvent {
    OperationDone {
        operation_id: uuid::Uuid,
        result: Result<OperationValue>,
    },
    Connected {
        generation: u64,
        result: Result<(RoomSnapshot, AccountStatus, Vec<DanmuEvent>)>,
    },
    Client {
        generation: u64,
        event: BilibiliClientEvent,
    },
    Level(Option<MicrophoneLevel>),
    ResumeChecked {
        scope: (String, u64),
        result: Result<()>,
    },
    Scoped {
        generation: u64,
        event: Box<CoreEvent>,
    },
    Background(std::pin::Pin<Box<dyn Future<Output = ()> + Send>>),
    ChatgptLogin {
        token: uuid::Uuid,
        result: std::result::Result<
            crate::runner::subscription::PendingLogin,
            crate::runner::subscription::SubscriptionError,
        >,
    },
    ChatgptSignOut {
        token: uuid::Uuid,
        result: std::result::Result<
            crate::runner::subscription::SignOutOutcome,
            crate::runner::subscription::SubscriptionError,
        >,
    },
    ChatgptModels {
        token: uuid::Uuid,
        result: std::result::Result<Vec<crate::runner::responses::ModelChoice>, String>,
    },
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
        url: String,
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
        room_id: String,
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
    tx: &mpsc::Sender<CoreEvent>,
    room: &RoomSnapshot,
    generation: u64,
) -> bool {
    let (online, likes) = tokio::join!(
        account.current_online_viewers(&room.room_id, &room.broadcaster_id),
        account.current_likes(&room.room_id),
    );
    tx.send(CoreEvent::Scoped {
        generation,
        event: Box::new(CoreEvent::OnlineViewers(
            online.map_err(|error| error.to_string()),
        )),
    })
    .await
    .is_ok()
        && tx
            .send(CoreEvent::Scoped {
                generation,
                event: Box::new(CoreEvent::Likes(likes.map_err(|error| error.to_string()))),
            })
            .await
            .is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryStatus {
    Idle,
    Sending,
    AwaitingEcho,
    Verifying,
    Delivered,
    Uncertain,
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingDelivery {
    pub(crate) id: String,
    pub(crate) content: String,
    pub(crate) broadcaster_name: String,
    pub(crate) broadcaster_id: String,
    pub(crate) submitted_at: DateTime<Utc>,
    pub(crate) room_id: Option<String>,
}

impl PendingDelivery {
    pub(crate) fn new(
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
            room_id: None,
        }
    }

    pub(crate) fn matches(&self, event: &DanmuEvent) -> bool {
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

    pub(crate) fn canonicalize(&self, event: &mut DanmuEvent) {
        event.username = Some(self.broadcaster_name.clone());
        event.author_id = Some(self.broadcaster_id.clone());
    }
}

#[derive(Debug)]
pub(crate) struct ActiveDelivery {
    pub(crate) delivery: PendingDelivery,
    pub(crate) confirmation: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeLevel {
    Info,
    Success,
    Warning,
    Error,
    Progress,
}

impl NoticeLevel {
    pub(crate) fn lifetime(self) -> Option<Duration> {
        match self {
            Self::Info | Self::Success => Some(Duration::from_secs(3)),
            Self::Warning | Self::Error => Some(DELIVERY_NOTICE_LIFETIME),
            Self::Progress => None,
        }
    }
}

impl CoreEvent {
    pub(crate) fn success(message: impl Into<String>) -> Self {
        Self::Notice {
            message: message.into(),
            level: NoticeLevel::Success,
        }
    }

    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self::Notice {
            message: message.into(),
            level: NoticeLevel::Error,
        }
    }
}

pub(crate) fn operation_notice(result: Result<String>) -> CoreEvent {
    match result {
        Ok(message) => CoreEvent::success(message),
        Err(error) => CoreEvent::error(error.to_string()),
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) enum StopFlow {
    Confirm { stop_selected: bool },
    Countdown { deadline: Instant },
    Stopping,
}

pub(crate) struct RuntimeState {
    #[cfg(feature = "tui")]
    pub(crate) config: TerminalConfig,
    pub(crate) client: BilibiliClient,
    pub(crate) account: AccountClient,
    pub(crate) assistant_accounts: accounts::AssistantAccounts,
    pub(crate) manual_send_queue: SendQueue,
    pub(crate) independent_send_queue: SendQueue,
    pub(crate) bridge: Bridge,
    pub(crate) local_transport: Option<std::sync::Arc<assistant::LocalTransport>>,
    pub(crate) live_scope: Option<String>,
    pub(crate) candidate_selected: Option<(String, String)>,
    #[cfg(feature = "tui")]
    pub(crate) candidate_edit: Option<crate::terminal::agent::CandidateEdit>,
    #[cfg(feature = "tui")]
    pub(crate) review_frame: Option<crate::terminal::agent::ReviewFrame>,
    pub(crate) runner: crate::runner::Runner,
    pub(crate) assistant_failure: Option<String>,
    #[cfg(feature = "tui")]
    pub(crate) assistant_panel: Option<crate::terminal::assistant::Panel>,
    pub(crate) settings_operation: Option<uuid::Uuid>,
    pub(crate) chatgpt_operation: Option<ChatgptOperation>,
    pub(crate) ui_tx: Option<mpsc::Sender<CoreEvent>>,
    pub(crate) profile: assistant::ProfileState,
    pub(crate) obs: ObsController,
    pub(crate) journal: SessionJournal,
    pub(crate) session: DanmuSession,
    pub(crate) room: Option<RoomSnapshot>,
    pub(crate) room_updated_at: Option<DateTime<Local>>,
    pub(crate) connection: String,
    pub(crate) watched: Option<u64>,
    pub(crate) likes: Option<u64>,
    pub(crate) online_viewers: Option<u64>,
    #[cfg(feature = "tui")]
    pub(crate) input: EditorInput,
    #[cfg(feature = "tui")]
    pub(crate) slash_selection: usize,
    #[cfg(feature = "tui")]
    pub(crate) command_search_draft: Option<EditorInput>,
    #[cfg(feature = "tui")]
    pub(crate) secret_draft: Option<EditorInput>,
    #[cfg(feature = "tui")]
    pub(crate) help: Option<HelpState>,
    #[cfg(feature = "tui")]
    pub(crate) selected: usize,
    #[cfg(feature = "tui")]
    pub(crate) scroll_offset: usize,
    #[cfg(feature = "tui")]
    pub(crate) last_user_activity: Instant,
    #[cfg(feature = "tui")]
    pub(crate) activity: activity::ActivityNotices,
    pub(crate) notice: String,
    pub(crate) notice_deadline: Option<Instant>,
    pub(crate) notice_is_delivery: bool,
    pub(crate) notice_level: NoticeLevel,
    pub(crate) delivery_status: DeliveryStatus,
    pub(crate) delivery_status_deadline: Option<Instant>,
    #[cfg(feature = "tui")]
    pub(crate) layout_chat: bool,
    #[cfg(feature = "tui")]
    pub(crate) show_name: bool,
    #[cfg(feature = "tui")]
    pub(crate) show_time: bool,
    pub(crate) account_status: AccountStatus,
    pub(crate) stop_flow: Option<StopFlow>,
    #[cfg(feature = "tui")]
    pub(crate) login_qr: Option<Vec<String>>,
    pub(crate) main_login: Option<MainLogin>,
    #[cfg(feature = "tui")]
    pub(crate) secret_target: Option<SecretTarget>,
    #[cfg(feature = "tui")]
    pub(crate) selection_active: bool,
    pub(crate) quit_requested: bool,
    #[cfg(feature = "tui")]
    pub(crate) unread_live_count: u64,
    pub(crate) obs_status: Option<ObsStatus>,
    pub(crate) microphone_level: Option<MicrophoneLevel>,
    pub(crate) obs_error: Option<String>,
    pub(crate) obs_checked_at: Option<DateTime<Local>>,
    pub(crate) pending_deliveries: VecDeque<ActiveDelivery>,
    pub(crate) confirmed_deliveries: VecDeque<PendingDelivery>,
    pub(crate) last_realtime_at: Option<DateTime<Local>>,
    pub(crate) last_live_danmu_at: Option<DateTime<Local>>,
    #[cfg(feature = "tui")]
    pub(crate) animation_tick: u64,
    #[cfg(feature = "tui")]
    pub(crate) startup_gate: Option<StartupGate>,
    #[cfg(feature = "tui")]
    pub(crate) startup_view: StartupView,
    #[cfg(feature = "tui")]
    pub(crate) startup_started: Instant,
    pub(crate) resume_pending: bool,
    pub(crate) login_url: Option<String>,
    mode: RuntimeMode,
    storage: StoragePaths,
    pub(crate) effects: Vec<HostEffect>,
    pub(crate) current_operation: uuid::Uuid,
    operations: std::collections::HashMap<uuid::Uuid, OperationState>,
    tasks: tokio::task::JoinSet<()>,
    connection_tasks: tokio::task::JoinSet<()>,
    connection_stop: Option<watch::Sender<bool>>,
    connection_generation: u64,
    stop_token: Option<(uuid::Uuid, Instant)>,
    stop_operation: Option<uuid::Uuid>,
    ingestion_live: bool,
    pub(crate) new_message_count: u64,
    message_keys: HashMap<RowIdentity, DisplayKey>,
    message_key_epoch: u64,
    next_message_key: u64,
    models: Vec<ModelRecord>,
    endpoint: Option<bridge::wire::Server>,
    _lock: Option<std::fs::File>,
    pending_changes: Vec<RuntimeChange>,
    operation_tasks: HashMap<uuid::Uuid, tokio::task::AbortHandle>,
    send_operations: HashMap<uuid::Uuid, crate::delivery::Permit>,
    connect_operation: Option<uuid::Uuid>,
    restore_scope: Option<(String, u64)>,
    restore_verified: bool,
    pub(crate) events_dirty: bool,
    obs_tasks: tokio::task::JoinSet<()>,
    obs_stop: Option<watch::Sender<bool>>,
    pub(crate) obs_allowed: bool,
    storage_error: Option<String>,
}

impl RuntimeState {
    pub(crate) fn check_assistant_connection_change(&self) -> Result<()> {
        anyhow::ensure!(!self.resume_pending, "请先暂停助手、取消待续用，再修改连接");
        anyhow::ensure!(
            self.chatgpt_operation.is_none(),
            "请先完成或取消本机订阅操作"
        );
        self.runner.check_connection_change(&self.bridge)
    }

    pub(crate) fn chatgpt_sender(&self) -> Result<mpsc::Sender<CoreEvent>> {
        self.ui_tx
            .clone()
            .ok_or_else(|| anyhow!("界面事件通道尚未准备"))
    }

    pub(crate) fn begin_chatgpt_login(&mut self, account_id: Option<&str>) -> Result<()> {
        self.check_assistant_connection_change()?;
        let tx = self.chatgpt_sender()?;
        let login = self.runner.begin_chatgpt_login(&self.bridge, account_id)?;
        let token = self.operation_token();
        self.effects.push(HostEffect::OpenBrowser {
            operation_id: token,
            url: login.url().to_owned(),
        });
        let task = tokio::spawn(async move {
            let result = login.finish().await;
            let _ = tx.send(CoreEvent::ChatgptLogin { token, result }).await;
        });
        self.chatgpt_operation = Some(ChatgptOperation {
            token,
            task,
            account_id: self.runner.settings.api.chatgpt_account.clone(),
            scene_id: self.bridge.session_id(),
            account_generation: self.assistant_accounts.generation(),
        });
        self.set_notice(
            "请在官方浏览器完成 ChatGPT 授权；Esc取消，原账号不变",
            NoticeLevel::Progress,
        );
        Ok(())
    }

    pub(crate) fn begin_chatgpt_sign_out(&mut self, account_id: &str) -> Result<()> {
        self.check_assistant_connection_change()?;
        let tx = self.chatgpt_sender()?;
        let path = self
            .runner
            .begin_chatgpt_sign_out(&self.bridge, account_id)?;
        let account_id = account_id.to_owned();
        let token = self.operation_token();
        let task = tokio::spawn(async move {
            let result = crate::runner::subscription::sign_out(&path, &account_id).await;
            let _ = tx.send(CoreEvent::ChatgptSignOut { token, result }).await;
        });
        self.chatgpt_operation = Some(ChatgptOperation {
            token,
            task,
            account_id: self.runner.settings.api.chatgpt_account.clone(),
            scene_id: self.bridge.session_id(),
            account_generation: self.assistant_accounts.generation(),
        });
        self.set_notice(
            "正在退出所选 ChatGPT 账号；本地清除与远端撤销分别报告",
            NoticeLevel::Progress,
        );
        Ok(())
    }

    pub(crate) fn begin_chatgpt_models(&mut self) -> Result<()> {
        self.check_assistant_connection_change()?;
        let tx = self.chatgpt_sender()?;
        let mut session = self.runner.begin_chatgpt_models(&self.bridge)?;
        let token = self.operation_token();
        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(180), async {
                let client = crate::runner::responses::client().map_err(|e| e.to_string())?;
                let header = session.header().await.map_err(|e| e.to_string())?;
                crate::runner::responses::list_models(&client, header)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|_| Err("ChatGPT模型目录读取超时".into()));
            let _ = tx.send(CoreEvent::ChatgptModels { token, result }).await;
        });
        self.chatgpt_operation = Some(ChatgptOperation {
            token,
            task,
            account_id: self.runner.settings.api.chatgpt_account.clone(),
            scene_id: self.bridge.session_id(),
            account_generation: self.assistant_accounts.generation(),
        });
        self.set_notice("正在读取所选账号的模型目录；Esc取消", NoticeLevel::Progress);
        Ok(())
    }

    pub(crate) fn cancel_chatgpt_operation(&mut self) {
        if let Some(operation) = self.chatgpt_operation.take() {
            self.runner.cancel_chatgpt_operation();
            self.operations
                .insert(operation.token, OperationState::Cancelled);
            self.pending_changes.push(RuntimeChange::Operation {
                operation_id: operation.token,
                state: OperationState::Cancelled,
                value: OperationValue::None,
            });
        }
    }

    pub(crate) fn take_chatgpt_operation(&mut self, token: uuid::Uuid) -> bool {
        if !self
            .chatgpt_operation
            .as_ref()
            .is_some_and(|op| op.token == token)
        {
            return false;
        }
        let operation = self.chatgpt_operation.take().expect("已核对操作token");
        if operation.account_id != self.runner.settings.api.chatgpt_account
            || operation.scene_id != self.bridge.session_id()
            || operation.account_generation != self.assistant_accounts.generation()
            || self.bridge.status()["active"] != true
        {
            self.runner.cancel_chatgpt_operation();
            self.set_notice(
                "账号或场次范围已改变；旧订阅结果未应用",
                NoticeLevel::Warning,
            );
            return false;
        }
        true
    }

    pub(crate) fn handle_client_event(&mut self, event: BilibiliClientEvent) {
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

    pub(crate) fn handle_ui_event(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::ChatgptLogin { token, result } => {
                if !self.take_chatgpt_operation(token) {
                    return;
                }
                let result = if self.resume_pending {
                    self.runner.cancel_chatgpt_operation();
                    Err(anyhow!("待续用状态已改变；登录结果未提交"))
                } else {
                    self.runner.finish_chatgpt_login(&self.bridge, result)
                };
                let result = result.and_then(|()| self.refresh_chatgpt_panel());
                match result {
                    Ok(()) => {
                        self.finish_operation(token, Ok(OperationValue::None));
                        self.current_operation = uuid::Uuid::new_v4();
                        if let Err(error) = self.begin_chatgpt_models() {
                            self.set_notice(error.to_string(), NoticeLevel::Error);
                        }
                    }
                    Err(error) => {
                        self.set_notice(error.to_string(), NoticeLevel::Error);
                        self.finish_operation(token, Err(error));
                    }
                }
            }
            CoreEvent::ChatgptSignOut { token, result } => {
                if !self.take_chatgpt_operation(token) {
                    return;
                }
                let result = self.runner.complete_chatgpt_sign_out(&self.bridge, result);
                let cache = self.refresh_chatgpt_panel();
                let result = result.and(cache);
                if let Err(error) = &result {
                    self.set_notice(error.to_string(), NoticeLevel::Error);
                } else {
                    self.set_notice(self.runner.note.clone(), NoticeLevel::Info);
                }
                self.finish_operation(token, result.map(|()| OperationValue::None));
            }
            CoreEvent::ChatgptModels { token, result } => {
                if !self.take_chatgpt_operation(token) {
                    return;
                }
                self.runner.finish_chatgpt_models();
                let result = self
                    .check_assistant_connection_change()
                    .map_err(|error| error.to_string())
                    .and(result)
                    .and_then(|models| {
                        self.show_chatgpt_models(models)
                            .map_err(|error| error.to_string())
                    });
                match result {
                    Ok(()) => {
                        self.set_notice(
                            "账号模型目录已更新；请选择模型后显式启动",
                            NoticeLevel::Success,
                        );
                        self.finish_operation(token, Ok(OperationValue::None));
                    }
                    Err(error) => {
                        self.set_notice(
                            format!("{error}；未启动助手，可手动重新读取账号模型"),
                            NoticeLevel::Error,
                        );
                        self.finish_operation(token, Err(anyhow!(error)));
                    }
                }
            }
            CoreEvent::SettingSaved { token, result } => self.finish_setting_save(token, result),
            CoreEvent::OperationDone {
                operation_id,
                result,
            } => self.finish_operation(operation_id, result),
            CoreEvent::Connected { generation, result } => self.finish_connect(generation, result),
            CoreEvent::Scoped { generation, event } => {
                if generation == self.connection_generation {
                    self.handle_ui_event(*event);
                }
            }
            CoreEvent::Client { generation, event } => {
                if generation == self.connection_generation {
                    self.handle_client_event(event);
                }
            }
            CoreEvent::Background(task) => {
                self.obs_tasks.spawn(task);
            }
            CoreEvent::Level(level) => self.microphone_level = level,
            CoreEvent::ResumeChecked { scope, result } => {
                if self.resume_pending
                    && self.restore_scope.as_ref() == Some(&scope)
                    && scope
                        == (
                            self.bridge.session_id(),
                            self.assistant_accounts.generation(),
                        )
                {
                    match result {
                        Ok(()) => self.restore_verified = true,
                        Err(error) => {
                            self.resume_pending = false;
                            self.restore_scope = None;
                            self.set_notice(format!("自动发送未恢复：{error}"), NoticeLevel::Error);
                        }
                    }
                }
            }
            CoreEvent::LocalEcho(text) => {
                let mut event = DanmuEvent::new(DanmuEventKind::Danmu, text);
                event.username = Some("本机发送".into());
                event.author_id = Some("local-host".into());
                self.ingest_event(event);
            }

            CoreEvent::DeliveryAccepted => self.set_delivery_status(DeliveryStatus::AwaitingEcho),
            CoreEvent::DeliveryNotice(message) => self.set_delivery_notice(message),
            CoreEvent::ObsStopDone(result) => {
                self.stop_flow = None;
                self.stop_token = None;
                if let Some(operation_id) = self.stop_operation.take() {
                    self.finish_operation(
                        operation_id,
                        result
                            .clone()
                            .map(|()| OperationValue::None)
                            .map_err(anyhow::Error::msg),
                    );
                }
                self.handle_ui_event(operation_notice(
                    result
                        .map(|()| "OBS 已停止推流".to_owned())
                        .map_err(anyhow::Error::msg),
                ));
            }
            CoreEvent::AssistantAccount(event) => {
                let ready = matches!(&event, accounts::AccountEvent::Ready { .. });
                let unavailable = matches!(&event, accounts::AccountEvent::Unavailable { .. });
                let completion = match &event {
                    accounts::AccountEvent::Ready { token, .. } => Some((*token, None)),
                    accounts::AccountEvent::Failed { token, message } => {
                        Some((*token, Some(message.clone())))
                    }
                    _ => None,
                };
                if let Some(mut message) = self.assistant_accounts.apply_event(event) {
                    if unavailable {
                        self.resume_pending = false;
                        self.bridge.identity_changed();
                        self.runner.pause(&self.bridge);
                        #[cfg(feature = "tui")]
                        {
                            self.review_frame = None;
                        }
                        if let Err(error) = self.assistant_accounts.forget_automatic() {
                            message.push_str(&format!("；清除记住的授权失败：{error}"));
                        }
                    } else {
                        self.refresh_login_projection();
                    }
                    if let Some((token, Some(error))) = &completion {
                        self.finish_operation(*token, Err(anyhow!(error.clone())));
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
                                #[cfg(feature = "tui")]
                                {
                                    self.review_frame = None;
                                }
                                self.set_notice(message, NoticeLevel::Success);
                                if let Some((token, _)) = completion {
                                    self.finish_operation(token, Ok(OperationValue::None));
                                }
                            }
                            Err(error) => {
                                self.set_notice(error.to_string(), NoticeLevel::Error);
                                if let Some((token, _)) = completion {
                                    self.finish_operation(token, Err(error));
                                }
                            }
                        }
                        // Keep the visited account page and its return stack.
                    }
                }
            }
            CoreEvent::Notice { message, level } => {
                self.set_notice(message, level);
            }
            CoreEvent::LoginQr { token, url } => {
                if self
                    .main_login
                    .as_ref()
                    .is_some_and(|login| login.token == token)
                {
                    self.login_url = Some(url);
                    self.refresh_login_projection();
                    self.set_notice("请使用主账号扫码登录", NoticeLevel::Progress);
                }
            }
            CoreEvent::LoginDone {
                token,
                account,
                status,
            } => {
                if self
                    .main_login
                    .as_ref()
                    .is_some_and(|login| login.token == token)
                {
                    self.main_login.take();
                    self.login_url = None;
                    self.refresh_login_projection();
                    let result = (|| -> Result<OperationValue> {
                        let path = self
                            .account
                            .session_path()
                            .context("主账号凭据目录不可用")?;
                        account.persist_to(path.to_path_buf())?;
                        self.account_status = status;
                        self.manual_send_queue.pause();
                        self.manual_send_queue.resume();
                        self.bridge.identity_changed();
                        self.runner.pause(&self.bridge);
                        self.assistant_accounts.main_changed()?;
                        #[cfg(feature = "tui")]
                        {
                            self.review_frame = None;
                        }
                        self.set_notice(
                            "主账号登录成功；助手仍暂停，未授权发送",
                            NoticeLevel::Success,
                        );
                        Ok(OperationValue::None)
                    })();
                    self.finish_operation(token, result);
                }
            }
            CoreEvent::LoginFailed { token, message } => {
                if self
                    .main_login
                    .as_ref()
                    .is_some_and(|login| login.token == token)
                {
                    self.main_login.take();
                    self.login_url = None;
                    self.refresh_login_projection();
                    self.finish_operation(token, Err(anyhow!(message)));
                }
            }
            CoreEvent::ObsStatus(result) => {
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
            CoreEvent::RoomSnapshot(snapshot) => {
                self.room = Some(snapshot);
                self.room_updated_at = Some(Local::now());
                self.sync_assistant_room();
            }
            CoreEvent::BroadcasterProfile {
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
            CoreEvent::OnlineViewers(result) => match result {
                Ok(viewers) => self.online_viewers = viewers,
                Err(error) => {
                    self.set_notice(
                        format!("在线人数刷新失败，继续显示上次成功值：{error}"),
                        NoticeLevel::Error,
                    );
                }
            },
            CoreEvent::Likes(result) => match result {
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
            CoreEvent::DeliveryStarted {
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
            CoreEvent::AssistantDelivery { session_id, record } => {
                if let Err(error) = self.journal.reply_record(&session_id, &record) {
                    self.storage_error = Some(error.to_string());
                    let message = self.fail_assistant(format!(
                        "发送结果归档失败，助手已暂停；真实发送结果不变且不重发：{error}"
                    ));
                    self.set_notice(message, NoticeLevel::Error);
                }
            }
            CoreEvent::DeliveryHistory { events, room_id } => {
                for mut event in events {
                    if room_id == self.session.room_id {
                        self.ingest_event(event);
                    } else {
                        self.match_delivery(&mut event, Some(&room_id));
                    }
                }
                if !self.pending_deliveries.is_empty() {
                    self.set_delivery_status(DeliveryStatus::Verifying);
                }
            }
            CoreEvent::DeliveryEchoMissing { delivery_ids } => {
                if self
                    .pending_deliveries
                    .iter()
                    .any(|active| delivery_ids.contains(&active.delivery.id))
                {
                    self.set_delivery_status(DeliveryStatus::Uncertain);
                    self.set_delivery_notice("未确认送达；不自动重发，详情见助手");
                }
            }
            CoreEvent::DeliveryExpired { delivery_id } => {
                self.clear_delivery(&delivery_id);
            }
            CoreEvent::DeliveryTimedOut { delivery_ids } => {
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
            CoreEvent::DeliveryRejected { content, message } => {
                self.set_delivery_status(DeliveryStatus::Failed);
                self.set_delivery_notice(format!("{message}；内容：「{content}」"));
            }
            CoreEvent::DeliveryCompleted => {
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

    pub(crate) fn ingest_event(&mut self, mut event: DanmuEvent) {
        self.events_dirty = true;
        if event.kind == DanmuEventKind::RoomStatus
            && event.origin == DanmuEventOrigin::Live
            && let Some(room) = self.room.as_mut()
        {
            room.live_status = RoomLiveStatus::Offline;
            self.bridge.end();
        }
        let live_arrival = event.origin != DanmuEventOrigin::Archived
            && matches!(
                event.kind,
                DanmuEventKind::Danmu | DanmuEventKind::Superchat
            );
        if !self.match_delivery(&mut event, None) {
            return;
        }

        if reconcile_cross_origin_event(&mut self.session.recent_events, &event) {
            return;
        }
        let is_new_logical_event = !self
            .session
            .recent_events
            .iter()
            .any(|existing| existing.id == event.id);
        #[cfg(feature = "tui")]
        let anchors = self.presentation_anchors();
        if self.session.ingest(event.clone()) {
            if is_new_logical_event {
                if event.origin != DanmuEventOrigin::Archived
                    && (self.ingestion_live || !self.runner.running)
                {
                    self.bridge.ingest(event.clone());
                }
                if let Err(error) = self.runner.record_history(&self.session.room_id, &event) {
                    self.set_notice(
                        format!("历史索引未更新；原件仍单独保存：{error}"),
                        NoticeLevel::Error,
                    );
                }
            }
            if let Err(error) = self.journal.event(&self.session, &event) {
                self.storage_error = Some(error.to_string());
                self.set_notice(
                    format!("直播原始归档或工作区副本未完整保存：{error}"),
                    NoticeLevel::Error,
                );
            }
            // Only viewers speaking counts as unread; entries, likes, gifts and other activity
            // would keep the badge lit in any busy room without anything to read.
            if is_new_logical_event
                && live_arrival
                && self.ingestion_live
                && matches!(event.kind, DanmuEventKind::Danmu | DanmuEventKind::Superchat)
            {
                self.new_message_count = self.new_message_count.saturating_add(1);
            }
            #[cfg(feature = "tui")]
            self.restore_presentation_anchors(
                anchors,
                &event,
                is_new_logical_event && live_arrival,
            );
        }
    }

    pub(crate) fn clear_delivery(&mut self, delivery_id: &str) -> bool {
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

    pub(crate) fn set_notice(&mut self, message: impl Into<String>, level: NoticeLevel) {
        self.set_notice_at(message, level, Instant::now());
    }

    pub(crate) fn set_delivery_notice(&mut self, message: impl Into<String>) {
        self.notice = message.into();
        self.notice_deadline = Some(Instant::now() + DELIVERY_NOTICE_LIFETIME);
        self.notice_is_delivery = true;
        self.notice_level = NoticeLevel::Warning;
    }

    pub(crate) fn set_notice_at(
        &mut self,
        message: impl Into<String>,
        level: NoticeLevel,
        now: Instant,
    ) {
        self.notice = message.into();
        self.pending_changes.push(RuntimeChange::Notice {
            message: self.notice.clone(),
            is_error: level == NoticeLevel::Error,
        });
        self.notice_deadline = level.lifetime().map(|lifetime| now + lifetime);
        self.notice_is_delivery = false;
        self.notice_level = level;
    }

    pub(crate) fn set_delivery_status(&mut self, status: DeliveryStatus) {
        self.set_delivery_status_at(status, Instant::now());
    }

    pub(crate) fn set_delivery_status_at(&mut self, status: DeliveryStatus, now: Instant) {
        self.delivery_status = status;
        self.delivery_status_deadline = match status {
            DeliveryStatus::Delivered => Some(now + Duration::from_secs(3)),
            DeliveryStatus::Uncertain | DeliveryStatus::Failed => {
                Some(now + DELIVERY_NOTICE_LIFETIME)
            }
            _ => None,
        };
    }

    pub(crate) fn expire_notice_at(&mut self, now: Instant) {
        #[cfg(feature = "tui")]
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

    pub(crate) fn advance_stop_at(&mut self, now: Instant, tx: &mpsc::Sender<CoreEvent>) {
        if self.stop_token.is_some_and(|(_, expiry)| now >= expiry)
            && !matches!(self.stop_flow, Some(StopFlow::Stopping))
        {
            self.cancel_stop_request();
            self.set_notice("停止推流确认已过期；未发送停止请求", NoticeLevel::Warning);
            return;
        }
        if !matches!(self.stop_flow, Some(StopFlow::Countdown { deadline }) if now >= deadline) {
            return;
        }
        // Leave Countdown before spawning: later ticks must never send a second stop.
        self.stop_flow = Some(StopFlow::Stopping);
        let obs = self.obs.clone();
        let tx = tx.clone();
        self.tasks.spawn(async move {
            let result = obs.stop_stream().await.map_err(|error| error.to_string());
            let _ = tx.send(CoreEvent::ObsStopDone(result)).await;
        });
    }

    pub(crate) fn cancel_main_login(&mut self) {
        if let Some(login) = self.main_login.take() {
            if self.operations.get(&login.token) == Some(&OperationState::Accepted) {
                self.operations
                    .insert(login.token, OperationState::Cancelled);
                self.pending_changes.push(RuntimeChange::Operation {
                    operation_id: login.token,
                    state: OperationState::Cancelled,
                    value: OperationValue::None,
                });
            }
            self.login_url = None;
            self.refresh_login_projection();
        }
    }

    pub(crate) fn start_login(&mut self, tx: mpsc::Sender<CoreEvent>) {
        self.cancel_main_login();
        self.assistant_accounts.cancel_login();
        self.bridge.identity_changed();
        self.runner.pause(&self.bridge);
        #[cfg(feature = "tui")]
        {
            self.review_frame = None;
        }
        self.login_url = None;
        self.refresh_login_projection();
        let token = self.operation_token();
        let account = self.account.staged();
        let task = tokio::spawn(async move {
            let result: Result<AccountStatus> = async {
                let challenge =
                    tokio::time::timeout(Duration::from_secs(10), account.login_challenge())
                        .await??;
                let url = challenge.url.to_string();
                tx.send(CoreEvent::LoginQr { token, url })
                    .await
                    .map_err(|_| anyhow!("核心已关闭，登录操作未完成"))?;
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
                Ok(status) => CoreEvent::LoginDone {
                    token,
                    account,
                    status,
                },
                Err(error) => CoreEvent::LoginFailed {
                    token,
                    message: format!("主账号登录失败，原账号未更改：{error}"),
                },
            };
            let _ = tx.send(event).await;
        });
        self.main_login = Some(MainLogin { token, task });
    }
}
pub(crate) fn reconcile_cross_origin_event(
    events: &mut [DanmuEvent],
    incoming: &DanmuEvent,
) -> bool {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    Live,
    Local,
    Replay,
}

pub struct RuntimeOptions {
    pub storage: StoragePaths,
    pub workspace_default: PathBuf,
    pub mode: RuntimeMode,
    pub external_instance: Option<PathBuf>,
    pub obs_passwords: Arc<dyn crate::obs::ObsPasswordStore>,
}

type ShutdownResult = std::result::Result<(), Arc<str>>;
#[cfg(feature = "tui")]
type StateWork = Box<
    dyn for<'a> FnOnce(
            &'a mut RuntimeState,
        ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send,
>;
enum Request {
    Execute(RuntimeCommand, oneshot::Sender<Result<CommandResult>>),
    Query(RuntimeQuery, oneshot::Sender<Result<QueryResult>>),
    Observe(oneshot::Sender<RuntimeSubscription>),
    Shutdown,
    #[cfg(feature = "tui")]
    Presentation(StateWork),
    #[cfg(any(test, feature = "test-support"))]
    Inject(Vec<DanmuEvent>, bool, oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct RuntimeHandle {
    requests: mpsc::Sender<Request>,
    shutdown: watch::Receiver<Option<ShutdownResult>>,
    closing: Arc<std::sync::atomic::AtomicBool>,
}

impl RuntimeHandle {
    pub async fn execute(&self, command: RuntimeCommand) -> Result<CommandResult> {
        anyhow::ensure!(
            !self.closing.load(std::sync::atomic::Ordering::Acquire),
            RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已关闭")
        );
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Request::Execute(command, tx))
            .await
            .map_err(|_| RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已关闭"))?;
        rx.await.map_err(|_| {
            RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已停止，操作结果不可用")
        })?
    }
    pub async fn query(&self, query: RuntimeQuery) -> Result<QueryResult> {
        anyhow::ensure!(
            !self.closing.load(std::sync::atomic::Ordering::Acquire),
            RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已关闭")
        );
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Request::Query(query, tx))
            .await
            .map_err(|_| RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已关闭"))?;
        rx.await.map_err(|_| {
            RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已停止，查询结果不可用")
        })?
    }
    pub async fn observe(&self) -> Result<RuntimeSubscription> {
        anyhow::ensure!(
            !self.closing.load(std::sync::atomic::Ordering::Acquire),
            RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已关闭")
        );
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Request::Observe(tx))
            .await
            .map_err(|_| RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已关闭"))?;
        rx.await.map_err(|_| {
            RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已停止，无法订阅").into()
        })
    }
    pub async fn shutdown(&self) -> Result<()> {
        let mut result = self.shutdown.clone();
        if !self.closing.swap(true, std::sync::atomic::Ordering::AcqRel) {
            let _ = self.requests.send(Request::Shutdown).await;
        }
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value.map_err(|message| anyhow!(message.to_string()));
            }
            result.changed().await.map_err(|_| {
                RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层意外停止，结束结果不可用")
            })?;
        }
    }
    #[cfg(feature = "tui")]
    pub(crate) async fn presentation<T: Send + 'static>(
        &self,
        work: impl for<'a> FnOnce(
            &'a mut RuntimeState,
        ) -> std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>
        + Send
        + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Request::Presentation(Box::new(move |state| {
                Box::pin(async move {
                    let value = work(state).await;
                    let _ = tx.send(value);
                })
            })))
            .await
            .map_err(|_| anyhow!("运行层已关闭"))?;
        rx.await.map_err(|_| anyhow!("运行层已停止"))
    }
    #[cfg(any(test, feature = "test-support"))]
    pub async fn inject_events(&self, events: Vec<DanmuEvent>, realtime: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Request::Inject(events, realtime, tx))
            .await
            .map_err(|_| RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已关闭"))?;
        rx.await
            .map_err(|_| RuntimeFailure::new(RuntimeFailureKind::Closed, "运行层已停止").into())
    }
}

/// Prepares explicit local storage and the serial owner. No platform/model/OBS connection
/// is attempted until an explicit command. Subscriptions are downstream of this owner.
pub async fn open(options: RuntimeOptions) -> Result<RuntimeHandle> {
    let external = options.external_instance.clone();
    let mut state = tokio::task::spawn_blocking(move || RuntimeState::prepare(options))
        .await
        .context("运行层初始化任务失败")??;
    if let Some(root) = external {
        anyhow::ensure!(state.mode != RuntimeMode::Replay, "回放模式禁止外部端点");
        state.endpoint = Some(bridge::wire::serve(&root, state.bridge.clone()).await?);
    }
    Ok(spawn_owner(state))
}

fn spawn_owner(mut state: RuntimeState) -> RuntimeHandle {
    let (requests, receiver) = mpsc::channel(64);
    let (events, event_receiver) = mpsc::channel(512);
    state.ui_tx = Some(events);
    if state.mode == RuntimeMode::Local {
        state.local_transport = Some(Arc::new(assistant::LocalTransport {
            outcome: std::sync::atomic::AtomicU8::new(0),
            log: state.storage.support_dir.join("local-deliveries.jsonl"),
            tx: state.ui_tx.as_ref().expect("core event channel").clone(),
        }));
        state.bridge.available(true);
    }
    let (shutdown_tx, shutdown) = watch::channel(None);
    let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = RuntimeHandle {
        requests,
        shutdown,
        closing: closing.clone(),
    };
    tokio::spawn(async move {
        let result = run_owner(state, receiver, event_receiver)
            .await
            .map_err(|error| Arc::<str>::from(format!("{error:#}")));
        closing.store(true, std::sync::atomic::Ordering::Release);
        let _ = shutdown_tx.send(Some(result));
    });
    handle
}

async fn run_owner(
    mut state: RuntimeState,
    mut requests: mpsc::Receiver<Request>,
    mut events: mpsc::Receiver<CoreEvent>,
) -> Result<()> {
    let (changes, _) = tokio::sync::broadcast::channel::<events::SequencedChange>(2048);
    let (watermark, _) = watch::channel((0_u64, 0_u64));
    let mut sequence = 0_u64;
    let mut previous: Option<ObservationCache> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(Request::Execute(command, reply)) => {
                    let result = state.execute_command(command).await;
                    let _ = reply.send(result);
                }
                Some(Request::Query(query, reply)) => state.query_command(query, reply),
                Some(Request::Observe(reply)) => {
                    publish_changes(&mut state, &changes, &watermark, &mut sequence, &mut previous);
                    let receiver = changes.subscribe();
                    let snapshot = state.snapshot();
                    let subscription = RuntimeSubscription {
                        initial: tokio::sync::Mutex::new(Some(RuntimeBatch { sequence, new_message_count: state.new_message_count, snapshot: Some(snapshot), changes: Vec::new() })),
                        receiver: tokio::sync::Mutex::new(receiver),
                        watermark: watermark.subscribe(),
                        closed: std::sync::atomic::AtomicBool::new(false),
                        wake: Arc::new(tokio::sync::Notify::new()),
                    };
                    let _ = reply.send(subscription);
                }
                Some(Request::Shutdown) | None => break,
                #[cfg(feature = "tui")]
                Some(Request::Presentation(work)) => work(&mut state).await,
                #[cfg(any(test, feature = "test-support"))]
                Some(Request::Inject(batch, realtime, done)) => {
                    let context = state.ingestion_live;
                    state.ingestion_live = realtime && state.mode != RuntimeMode::Replay;
                    for event in batch { state.ingest_event(event); }
                    state.ingestion_live = context;
                    let _ = done.send(());
                }
            },
            event = events.recv() => if let Some(event) = event { state.handle_ui_event(event); },
            Some(result) = state.tasks.join_next(), if !state.tasks.is_empty() => {
                if let Err(error) = result { state.set_notice(format!("受管操作任务失败：{error}"), NoticeLevel::Error); }
            },
            Some(result) = state.connection_tasks.join_next(), if !state.connection_tasks.is_empty() => {
                if let Err(error) = result { state.set_notice(format!("连接任务失败：{error}"), NoticeLevel::Error); }
            },
            Some(result) = state.obs_tasks.join_next(), if !state.obs_tasks.is_empty() => {
                if let Err(error) = result { state.set_notice(format!("后台任务失败：{error}"), NoticeLevel::Error); }
            },
            _ = tick.tick() => {
                let tx = state.ui_tx.as_ref().expect("core event channel").clone();
                state.advance_bridge(tx.clone());
                state.expire_notice_at(Instant::now());
                state.advance_stop_at(Instant::now(), &tx);
                state.advance_resume();
                publish_changes(&mut state, &changes, &watermark, &mut sequence, &mut previous);
            }
        }
    }
    state.quit_requested = true;
    state.manual_send_queue.pause();
    state.independent_send_queue.pause();
    state.cancel_stop_request();
    state.cancel_main_login();
    state.assistant_accounts.cancel_login();
    state.cancel_chatgpt_operation();
    state.runner.pause(&state.bridge);
    state.runner.shutdown(&state.bridge).await;
    // Do not abort a POST or its delivery result. Queue generations/permits cancel only
    // not-yet-issued segments; consume matcher/archival completions while joining.
    let mut task_error = None;
    while !state.tasks.is_empty() {
        tokio::select! {
            event = events.recv() => if let Some(event) = event { state.handle_ui_event(event); },
            result = state.tasks.join_next() => if let Some(Err(error)) = result {
                task_error = Some(anyhow!("结束操作任务失败：{error}"));
            },
        }
    }
    while let Ok(event) = events.try_recv() {
        state.handle_ui_event(event);
    }
    if let Some(stop) = state.connection_stop.take() {
        let _ = stop.send(true);
    }
    state.connection_tasks.abort_all();
    while state.connection_tasks.join_next().await.is_some() {}
    if let Some(stop) = state.obs_stop.take() {
        let _ = stop.send(true);
    }
    state.obs_tasks.abort_all();
    while state.obs_tasks.join_next().await.is_some() {}
    state.bridge.end();
    state.endpoint = None;
    let routing = state
        .runner
        .record_routing(&state.bridge, &state.journal, &state.session, true);
    state
        .session
        .end(Utc::now(), DanmuSessionEndReason::Completed);
    let journal = if state.session.room_id.is_empty() {
        Ok(())
    } else {
        state.journal.end(&state.session)
    };
    publish_changes(
        &mut state,
        &changes,
        &watermark,
        &mut sequence,
        &mut previous,
    );
    journal?;
    routing?;
    if let Some(error) = state.storage_error.take() {
        return Err(anyhow!(error));
    }
    if let Some(error) = task_error {
        return Err(error);
    }
    Ok(())
}

impl RuntimeState {
    pub(crate) fn prepare(options: RuntimeOptions) -> Result<Self> {
        for path in [
            &options.storage.support_dir,
            &options.storage.config_file,
            &options.storage.account_session,
            &options.storage.obs_configuration,
            &options.storage.sessions_dir,
            &options.workspace_default,
        ] {
            anyhow::ensure!(path.is_absolute(), "运行层路径必须为绝对路径");
            anyhow::ensure!(
                !path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
                "运行层路径不能含上级目录"
            );
            crate::persistence::check_path(path)?;
            if let Ok(metadata) = std::fs::symlink_metadata(path) {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "运行层路径不能是符号链接"
                );
            }
        }
        let runtime_root = options
            .storage
            .config_file
            .parent()
            .context("配置路径没有父目录")?;
        bridge::wire::private_root(runtime_root)?;
        bridge::wire::private_root(&options.storage.support_dir)?;
        for file in [
            &options.storage.account_session,
            &options.storage.config_file,
            &options.storage.obs_configuration,
            &options.storage.config_file.with_file_name("assistant.json"),
            &options
                .storage
                .config_file
                .with_file_name("assistant-account.json"),
        ] {
            crate::workspace_config::check_file(file)?;
        }
        if let Ok(metadata) = std::fs::metadata(&options.storage.account_session) {
            anyhow::ensure!(
                metadata.len() <= 256 * 1024,
                "账号凭据文件超过读取上限；原件保留，请重新登录"
            );
        }
        let lock_path = runtime_root.join("runtime.lock");
        if let Ok(metadata) = std::fs::symlink_metadata(&lock_path) {
            anyhow::ensure!(!metadata.file_type().is_symlink(), "运行层锁不能是符号链接");
        }
        let mut lock_options = std::fs::OpenOptions::new();
        lock_options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lock_options.mode(0o600);
        }
        let lock = lock_options
            .open(lock_path)
            .context("打开运行层独占锁失败")?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                anyhow::Error::new(RuntimeFailure::new(
                    RuntimeFailureKind::Busy,
                    "该运行层数据目录已由另一个实例打开",
                ))
            } else {
                anyhow::Error::new(error).context("运行层独占锁失败")
            }
        })?;
        let account = AccountClient::new(options.storage.account_session.clone())?;
        let account_status = account.cached_status()?;
        let client = BilibiliClient::new(options.storage.account_session.clone())?;
        let assistant_accounts =
            accounts::AssistantAccounts::load(&account, &options.storage.config_file)?;
        assistant_accounts.check_loaded()?;
        let configuration = crate::obs::ObsConfiguration::load(&options.storage.obs_configuration)?;
        let obs_allowed = options.mode == RuntimeMode::Live;
        #[cfg(any(test, feature = "test-support"))]
        let obs_allowed = obs_allowed
            || (options.mode == RuntimeMode::Local
                && crate::test_support::is_fake_obs_endpoint(
                    &configuration.host,
                    configuration.port,
                ));
        let obs = ObsController::new(
            configuration,
            options.storage.obs_configuration.clone(),
            options.obs_passwords,
        );
        let runner =
            crate::runner::Runner::load(&options.storage.config_file, &options.workspace_default);
        if let Some(error) = &runner.load_error {
            return Err(anyhow!(error.clone()));
        }
        let journal = SessionJournal::new(options.storage.sessions_dir.clone());
        let session = DanmuSession::new(if options.mode == RuntimeMode::Live {
            ""
        } else {
            "local"
        });
        if options.mode != RuntimeMode::Live {
            journal.start(&session)?;
        }
        #[cfg(feature = "tui")]
        let config = terminal::presentation_config(&options.storage)?;
        Ok(Self {
            #[cfg(feature = "tui")]
            config,
            client,
            account,
            assistant_accounts,
            manual_send_queue: SendQueue::default(),
            independent_send_queue: SendQueue::default(),
            bridge: Bridge::new(options.mode != RuntimeMode::Live),
            local_transport: None,
            live_scope: None,
            candidate_selected: None,
            #[cfg(feature = "tui")]
            candidate_edit: None,
            #[cfg(feature = "tui")]
            review_frame: None,
            runner,
            assistant_failure: None,
            #[cfg(feature = "tui")]
            assistant_panel: None,
            settings_operation: None,
            chatgpt_operation: None,
            ui_tx: None,
            profile: assistant::ProfileState::default(),
            obs,
            journal,
            session,
            room: None,
            room_updated_at: None,
            connection: "未连接".into(),
            watched: None,
            likes: None,
            online_viewers: None,
            #[cfg(feature = "tui")]
            input: EditorInput::default(),
            #[cfg(feature = "tui")]
            slash_selection: 0,
            #[cfg(feature = "tui")]
            command_search_draft: None,
            #[cfg(feature = "tui")]
            secret_draft: None,
            #[cfg(feature = "tui")]
            help: None,
            #[cfg(feature = "tui")]
            selected: 0,
            #[cfg(feature = "tui")]
            scroll_offset: 0,
            #[cfg(feature = "tui")]
            last_user_activity: Instant::now(),
            #[cfg(feature = "tui")]
            activity: activity::ActivityNotices::default(),
            notice: String::new(),
            notice_deadline: None,
            notice_is_delivery: false,
            notice_level: NoticeLevel::Info,
            delivery_status: DeliveryStatus::Idle,
            delivery_status_deadline: None,
            #[cfg(feature = "tui")]
            layout_chat: false,
            #[cfg(feature = "tui")]
            show_name: true,
            #[cfg(feature = "tui")]
            show_time: true,
            account_status,
            stop_flow: None,
            #[cfg(feature = "tui")]
            login_qr: None,
            main_login: None,
            #[cfg(feature = "tui")]
            secret_target: None,
            #[cfg(feature = "tui")]
            selection_active: false,
            quit_requested: false,
            #[cfg(feature = "tui")]
            unread_live_count: 0,
            obs_status: None,
            microphone_level: None,
            obs_error: None,
            obs_checked_at: None,
            pending_deliveries: VecDeque::new(),
            confirmed_deliveries: VecDeque::new(),
            last_realtime_at: None,
            last_live_danmu_at: None,
            #[cfg(feature = "tui")]
            animation_tick: 0,
            #[cfg(feature = "tui")]
            startup_gate: None,
            #[cfg(feature = "tui")]
            startup_view: StartupView::new(),
            #[cfg(feature = "tui")]
            startup_started: Instant::now(),
            resume_pending: false,
            login_url: None,
            mode: options.mode,
            storage: options.storage,
            effects: Vec::new(),
            current_operation: uuid::Uuid::nil(),
            operations: HashMap::new(),
            tasks: tokio::task::JoinSet::new(),
            connection_tasks: tokio::task::JoinSet::new(),
            connection_stop: None,
            connection_generation: 0,
            stop_token: None,
            ingestion_live: options.mode == RuntimeMode::Local,
            new_message_count: 0,
            message_keys: HashMap::new(),
            message_key_epoch: 0,
            next_message_key: 1,
            models: Vec::new(),
            endpoint: None,
            _lock: Some(lock),
            pending_changes: Vec::new(),
            operation_tasks: HashMap::new(),
            send_operations: HashMap::new(),
            connect_operation: None,
            restore_scope: None,
            restore_verified: false,
            events_dirty: true,
            obs_tasks: tokio::task::JoinSet::new(),
            obs_stop: None,
            obs_allowed,
            stop_operation: None,
            storage_error: None,
        })
    }

    fn state_record(&self) -> RuntimeStateRecord {
        RuntimeStateRecord {
            session_id: self.session.id.clone(),
            room_id: self.session.room_id.clone(),
            room: self.room.clone(),
            connection: self.connection.clone(),
            watched: self.watched,
            likes: self.likes,
            online_viewers: self.online_viewers,
            main_account: self.account_status.clone(),
            assistant_account: self.assistant_accounts.status(&self.account_status).clone(),
            identity_generation: self.assistant_accounts.generation(),
            assistant_running: self.runner.running,
            automatic_permission: self.bridge.status()["sending_enabled"] == true,
            resume_pending: self.resume_pending,
            assistant_note: self.runner.note.clone(),
            assistant_failure: self.assistant_failure.clone(),
            assistant_search: events::search_state(self.runner.last_search),
            assistant_returned_model: self.runner.returned_model.clone(),
            obs: self.obs_status.clone(),
            microphone_level: self.microphone_level,
            obs_error: self.obs_error.clone(),
            stop_confirmation: self.stop_token.map(|(token, _)| token),
            stop_seconds_remaining: match self.stop_flow {
                Some(StopFlow::Countdown { deadline }) => Some(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .as_secs()
                        .saturating_add(1),
                ),
                _ => None,
            },
            login_url: self.login_url.clone(),
            assistant_login_url: self.assistant_accounts.qr_url().map(str::to_owned),
            featured_message_id: self
                .session
                .featured_event
                .as_ref()
                .map(|event| event.id.clone()),
        }
    }
    fn snapshot(&mut self) -> RuntimeSnapshot {
        self.message_key_epoch = self.message_key_epoch.wrapping_add(1);
        let epoch = self.message_key_epoch;
        let mut records = Vec::with_capacity(self.session.recent_events.len());
        for (identity, event) in identified_rows(&self.session.recent_events) {
            let key = self.message_keys.entry(identity).or_insert_with(|| {
                let value = self.next_message_key;
                self.next_message_key = self
                    .next_message_key
                    .checked_add(1)
                    .expect("message key exhausted");
                DisplayKey { value, seen: epoch }
            });
            key.seen = epoch;
            records.push(EventRecord {
                message_key: key.value,
                event: event.clone(),
                assistant_state: self.bridge.mark(&event.id),
            });
        }
        self.message_keys.retain(|_, key| key.seen == epoch);
        RuntimeSnapshot {
            state: self.state_record(),
            events: records,
            candidates: self.reply_records(true),
            deliveries: self.reply_records(false),
            new_message_count: self.new_message_count,
        }
    }

    fn reply_records(&self, pending: bool) -> Vec<CandidateRecord> {
        self.bridge.inspect_replies(|replies| {
            let mut records: Vec<_> = replies
                .values()
                .filter(|reply| (reply.state == bridge::Execution::AwaitingApproval) == pending)
                .cloned()
                .map(|reply| events::candidate(reply, self.assistant_accounts.generation()))
                .collect();
            records.sort_by_key(|record| record.order);
            records
        })
    }
    fn replies_match(&self, records: &[CandidateRecord], pending: bool) -> bool {
        self.bridge.inspect_replies(|replies| {
            let generation = self.assistant_accounts.generation();
            let mut count = 0;
            for reply in replies
                .values()
                .filter(|reply| (reply.state == bridge::Execution::AwaitingApproval) == pending)
            {
                count += 1;
                if !records
                    .iter()
                    .any(|record| record.matches(reply, generation))
                {
                    return false;
                }
            }
            count == records.len()
        })
    }
    fn state_matches(&self, state: &RuntimeStateRecord) -> bool {
        state.session_id == self.session.id
            && state.room_id == self.session.room_id
            && state.room == self.room
            && state.connection == self.connection
            && state.watched == self.watched
            && state.likes == self.likes
            && state.online_viewers == self.online_viewers
            && state.main_account == self.account_status
            && &state.assistant_account == self.assistant_accounts.status(&self.account_status)
            && state.identity_generation == self.assistant_accounts.generation()
            && state.assistant_running == self.runner.running
            && state.automatic_permission == self.bridge.sending_enabled()
            && state.resume_pending == self.resume_pending
            && state.assistant_note == self.runner.note
            && state.assistant_failure == self.assistant_failure
            && state.assistant_search == events::search_state(self.runner.last_search)
            && state.assistant_returned_model == self.runner.returned_model
            && state.obs.as_ref().map(events::obs_fields)
                == self.obs_status.as_ref().map(events::obs_fields)
            && state.microphone_level == self.microphone_level
            && state.obs_error == self.obs_error
            && state.stop_confirmation == self.stop_token.map(|(token, _)| token)
            && state.stop_seconds_remaining
                == match self.stop_flow {
                    Some(StopFlow::Countdown { deadline }) => Some(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .as_secs()
                            .saturating_add(1),
                    ),
                    _ => None,
                }
            && state.login_url == self.login_url
            && state.assistant_login_url.as_deref() == self.assistant_accounts.qr_url()
            && state.featured_message_id.as_deref()
                == self
                    .session
                    .featured_event
                    .as_ref()
                    .map(|event| event.id.as_str())
    }

    fn refresh_login_projection(&mut self) {
        #[cfg(feature = "tui")]
        {
            self.login_qr = self
                .login_url
                .as_deref()
                .or_else(|| self.assistant_accounts.qr_url())
                .and_then(|url| terminal::login_qr_lines(url).ok());
        }
    }
    pub(crate) fn refresh_chatgpt_panel(&mut self) -> Result<()> {
        #[cfg(feature = "tui")]
        if let Some(panel) = &mut self.assistant_panel {
            panel.accounts = self.runner.chatgpt_accounts()?;
            panel.clear_chatgpt_models();
        }
        Ok(())
    }
    pub(crate) fn show_chatgpt_models(
        &mut self,
        models: Vec<crate::runner::responses::ModelChoice>,
    ) -> Result<()> {
        self.check_assistant_connection_change()?;
        anyhow::ensure!(!models.is_empty(), "此账号暂无可选模型；可手动重新读取目录");
        if !self.runner.settings.api.model.is_empty()
            && !models
                .iter()
                .any(|model| model.slug == self.runner.settings.api.model)
        {
            let mut settings = self.runner.settings.clone();
            settings.api.model.clear();
            self.runner.save(settings, &self.bridge)?;
        }
        self.models = models
            .iter()
            .map(|model| ModelRecord {
                model_id: model.slug.clone(),
                display_name: model.display_name.clone(),
            })
            .collect();
        #[cfg(feature = "tui")]
        self.project_models(models)?;
        Ok(())
    }
    fn advance_resume(&mut self) {
        if !self.resume_pending
            || !self.restore_verified
            || self.runner.history_loading()
            || self.bridge.status()["available"] != true
        {
            return;
        }
        let Some((session, generation)) = self.restore_scope.take() else {
            return;
        };
        self.resume_pending = false;
        let result = self
            .restore_automatic(&session, generation)
            .and_then(|()| self.runner.start(&self.bridge, false));
        if let Err(error) = result {
            let message = self.fail_assistant(format!("助手未启用：{error}"));
            self.set_notice(message, NoticeLevel::Error);
        }
    }
}

// Legacy archives can contain empty/reused platform IDs. Timestamp and occurrence
// disambiguate those rows without using mutable author, origin or body as identity.
#[derive(Clone, PartialEq, Eq, Hash)]
struct RowIdentity {
    id: String,
    timestamp: DateTime<Utc>,
    occurrence: usize,
}
struct DisplayKey {
    value: u64,
    seen: u64,
}
fn identified_rows(events: &[DanmuEvent]) -> impl Iterator<Item = (RowIdentity, &DanmuEvent)> {
    let mut occurrences = HashMap::new();
    events.iter().map(move |event| {
        let occurrence = occurrences
            .entry((event.id.as_str(), event.timestamp))
            .or_insert(0);
        let identity = RowIdentity {
            id: event.id.clone(),
            timestamp: event.timestamp,
            occurrence: *occurrence,
        };
        *occurrence += 1;
        (identity, event)
    })
}

struct ObservedRow {
    key: u64,
    event: DanmuEvent,
    assistant_state: Option<bridge::Mark>,
    seen: u64,
}
struct ObservationCache {
    state: RuntimeStateRecord,
    rows: HashMap<RowIdentity, ObservedRow>,
    candidates: Vec<CandidateRecord>,
    deliveries: Vec<CandidateRecord>,
    epoch: u64,
    bridge_changes: bridge::BridgeChanges,
}

fn publish_changes(
    state: &mut RuntimeState,
    sender: &tokio::sync::broadcast::Sender<events::SequencedChange>,
    watermark: &watch::Sender<(u64, u64)>,
    sequence: &mut u64,
    previous: &mut Option<ObservationCache>,
) {
    let count = state.new_message_count;
    let mut publish = |change| {
        *sequence = sequence
            .checked_add(1)
            .expect("observation sequence exhausted");
        watermark.send_replace((*sequence, count));
        let _ = sender.send(events::SequencedChange {
            sequence: *sequence,
            count,
            change,
        });
    };
    let cache = previous.get_or_insert_with(|| ObservationCache {
        state: state.state_record(),
        rows: HashMap::new(),
        candidates: Vec::new(),
        deliveries: Vec::new(),
        epoch: 0,
        bridge_changes: state.bridge.observe_changes(),
    });
    let marks_changed = cache.bridge_changes.take_changed();
    if cache.state.session_id != state.session.id {
        for row in cache.rows.values() {
            publish(RuntimeChange::Remove {
                message_key: row.key,
            });
        }
        cache.rows.clear();
        state.message_keys.clear();
        state.events_dirty = true;
    }
    if state.events_dirty {
        cache.epoch = cache.epoch.wrapping_add(1);
        for (identity, event) in identified_rows(&state.session.recent_events) {
            if let Some(row) = cache.rows.get_mut(&identity) {
                row.seen = cache.epoch;
                if row.event != *event {
                    row.event.clone_from(event);
                    row.assistant_state = state.bridge.mark(&event.id);
                    publish(RuntimeChange::Upsert(EventRecord {
                        message_key: row.key,
                        event: event.clone(),
                        assistant_state: row.assistant_state,
                    }));
                }
            } else {
                let key = state
                    .message_keys
                    .entry(identity.clone())
                    .or_insert_with(|| {
                        let value = state.next_message_key;
                        state.next_message_key = state
                            .next_message_key
                            .checked_add(1)
                            .expect("message key exhausted");
                        DisplayKey {
                            value,
                            seen: state.message_key_epoch,
                        }
                    })
                    .value;
                cache.rows.insert(
                    identity,
                    ObservedRow {
                        key,
                        event: event.clone(),
                        assistant_state: state.bridge.mark(&event.id),
                        seen: cache.epoch,
                    },
                );
                publish(RuntimeChange::Upsert(EventRecord {
                    message_key: key,
                    event: event.clone(),
                    assistant_state: state.bridge.mark(&event.id),
                }));
            }
        }
        cache.rows.retain(|id, row| {
            if row.seen != cache.epoch {
                publish(RuntimeChange::Remove {
                    message_key: row.key,
                });
                state.message_keys.remove(id);
                false
            } else {
                true
            }
        });
        state.events_dirty = false;
    }
    if marks_changed {
        for row in cache.rows.values_mut() {
            let mark = state.bridge.mark(&row.event.id);
            if row.assistant_state != mark {
                row.assistant_state = mark;
                publish(RuntimeChange::Upsert(EventRecord {
                    message_key: row.key,
                    event: row.event.clone(),
                    assistant_state: mark,
                }));
            }
        }
    }
    if !state.state_matches(&cache.state) {
        cache.state = state.state_record();
        publish(RuntimeChange::State(Arc::new(cache.state.clone())));
    }
    if !state.replies_match(&cache.candidates, true) {
        cache.candidates = state.reply_records(true);
        publish(RuntimeChange::Candidates(cache.candidates.clone()));
    }
    if !state.replies_match(&cache.deliveries, false) {
        cache.deliveries = state.reply_records(false);
        publish(RuntimeChange::Deliveries(cache.deliveries.clone()));
    }
    for change in state.pending_changes.drain(..) {
        publish(change);
    }
}

impl RuntimeState {
    pub(crate) fn accounts_allowed(&self) -> bool {
        if self.mode == RuntimeMode::Live {
            return true;
        }
        #[cfg(any(test, feature = "test-support"))]
        if self.mode == RuntimeMode::Local
            && crate::test_support::is_fake_account_endpoint_installed()
        {
            return true;
        }
        false
    }
    pub(crate) fn stop_confirmation_token(&self) -> Option<uuid::Uuid> {
        self.stop_token.map(|(token, _)| token)
    }
    pub(crate) fn confirm_stop_request(
        &mut self,
        token: uuid::Uuid,
        now: Instant,
        operation_id: uuid::Uuid,
    ) -> Result<()> {
        anyhow::ensure!(
            self.stop_token
                .is_some_and(|(expected, expiry)| expected == token && now < expiry),
            "停止推流确认已过期"
        );
        anyhow::ensure!(
            matches!(self.stop_flow, Some(StopFlow::Confirm { .. })),
            "停止推流操作已确认或结束"
        );
        self.stop_flow = Some(StopFlow::Countdown {
            deadline: now + Duration::from_secs(3),
        });
        self.stop_operation = Some(operation_id);
        Ok(())
    }
    pub(crate) fn cancel_stop_request(&mut self) {
        if matches!(self.stop_flow, Some(StopFlow::Stopping)) {
            return;
        }
        self.stop_flow = None;
        self.stop_token = None;
        if let Some(operation_id) = self.stop_operation.take() {
            self.operations
                .insert(operation_id, OperationState::Cancelled);
            self.pending_changes.push(RuntimeChange::Operation {
                operation_id,
                state: OperationState::Cancelled,
                value: OperationValue::None,
            });
        }
    }
    fn start_obs_monitor(&mut self) {
        if self.obs_stop.is_some() {
            return;
        }
        let (stop, stopped) = watch::channel(false);
        self.obs_stop = Some(stop);
        let obs = self.obs.clone();
        let tx = self.ui_tx.as_ref().expect("core event channel").clone();
        let mut polling_stop = stopped.clone();
        self.obs_tasks.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = polling_stop.changed() => break,
                    _ = tick.tick() => {
                        let result = obs.fetch_status().await.map_err(|error| error.to_string());
                        if tx.send(CoreEvent::ObsStatus(result)).await.is_err() { break; }
                    }
                }
            }
        });
        let (levels, mut level_receiver) = watch::channel(None);
        let obs = self.obs.clone();
        self.obs_tasks.spawn(async move {
            obs.monitor_microphone_levels(levels, stopped).await;
        });
        let tx = self.ui_tx.as_ref().expect("core event channel").clone();
        self.obs_tasks.spawn(async move {
            while level_receiver.changed().await.is_ok() {
                let level = *level_receiver.borrow_and_update();
                if tx.send(CoreEvent::Level(level)).await.is_err() {
                    break;
                }
            }
        });
    }
}

impl RuntimeState {
    fn match_delivery(&mut self, event: &mut DanmuEvent, room_id: Option<&str>) -> bool {
        let room_id = room_id.unwrap_or(&self.session.room_id);
        let in_room = |delivery: &PendingDelivery| {
            delivery
                .room_id
                .as_deref()
                .is_none_or(|room| room == room_id)
        };
        let confirmed = self
            .pending_deliveries
            .iter()
            .position(|active| in_room(&active.delivery) && active.delivery.matches(event));
        self.confirmed_deliveries
            .retain(|delivery| event.timestamp <= delivery.submitted_at + DELIVERY_ECHO_WINDOW);
        if confirmed.is_none()
            && self
                .confirmed_deliveries
                .iter()
                .any(|delivery| in_room(delivery) && delivery.matches(event))
        {
            return false;
        }
        if let Some(index) = confirmed {
            let mut active = self
                .pending_deliveries
                .remove(index)
                .expect("matched pending delivery");
            active.delivery.canonicalize(event);
            self.confirmed_deliveries.push_back(active.delivery);
            while self.confirmed_deliveries.len() > 32 {
                self.confirmed_deliveries.pop_front();
            }
            if let Some(confirmation) = active.confirmation.take() {
                let _ = confirmation.send(());
            }
            // HTTP acceptance is still independently checked by RuntimeTransport.
            self.set_delivery_status(DeliveryStatus::Verifying);
        }
        true
    }
}

#[cfg(test)]
mod tests;

impl RuntimeState {
    pub(crate) fn recover_session(journal: &SessionJournal, room_id: &str) -> Result<DanmuSession> {
        let mut session = DanmuSession::new(room_id);
        session.preload_history(journal.recent_room_history(room_id, 240)?);
        Ok(session)
    }
}
