use super::settings::{AssistantSettings, RuntimeSettings};
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    Main,
    Assistant,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyPreset {
    Quiet,
    TextQa,
    Thanks,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyActivity {
    Cautious,
    Balanced,
    Active,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Json,
}
#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    Connect {
        room_id: String,
    },
    Disconnect,
    CancelOperation {
        operation_id: Uuid,
    },
    SendManual {
        text: String,
        reply_to_message_id: Option<String>,
        draft_revision: u64,
    },
    Account(AccountCommand),
    Room(RoomCommand),
    Assistant(AssistantCommand),
    Review(ReviewCommand),
    Obs(ObsCommand),
    FeatureMessage {
        message_id: Option<String>,
    },
    SetExternalEndpoint {
        root: Option<PathBuf>,
    },
    ExportSession {
        session_id: String,
        format: ExportFormat,
        destination: PathBuf,
    },
}
#[derive(Debug, Clone)]
pub enum AccountCommand {
    BeginLogin { identity: Identity },
    CancelLogin,
    SignOut { identity: Identity },
    UseMainForAssistant,
    UseIndependentAssistant,
}
#[derive(Debug, Clone)]
pub enum RoomCommand {
    SetTitle { text: String },
    SetCover { path: PathBuf },
}
#[derive(Debug, Clone)]
pub enum AssistantCommand {
    Start,
    Pause,
    Stop,
    ProcessVisible { message_ids: Vec<String> },
    SetAutomatic { enabled: bool },
    CancelPendingResume,
    SetRange { activity: ReplyActivity },
    SetTopic { text: String },
    ApplyPreset { preset: ReplyPreset },
    SaveSettings { settings: AssistantSettings },
    SetApiKey { value: String },
    ClearApiKey,
    BeginChatgptLogin { account_id: Option<String> },
    SignOutChatgpt { account_id: String },
    SelectChatgptAccount { account_id: String },
    RefreshModels,
    SelectModel { model_id: String },
    RepairHistory,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedCandidate {
    pub session: String,
    pub caller: String,
    pub request_id: String,
    pub revision: u64,
    pub identity_generation: u64,
}
#[derive(Debug, Clone)]
pub enum ReviewCommand {
    Edit {
        expected: ReviewedCandidate,
        text: String,
    },
    Approve {
        expected: Vec<ReviewedCandidate>,
    },
    Discard {
        expected: ReviewedCandidate,
    },
}
#[derive(Debug, Clone)]
pub enum ObsCommand {
    Connect,
    Refresh,
    SaveConfiguration {
        configuration: crate::obs::ObsConfiguration,
    },
    SetPassword {
        password: String,
    },
    SetMicrophone {
        name: String,
    },
    SetMuted {
        muted: bool,
    },
    SetScene {
        name: String,
    },
    StartStream,
    RequestStop,
    ConfirmStop {
        token: Uuid,
    },
    CancelStop {
        token: Uuid,
    },
}
#[derive(Debug, Clone)]
pub enum RuntimeQuery {
    Snapshot,
    Diagnostics,
    Settings,
    Candidates,
    SubscriptionAccounts,
    Models,
    ObsScenes,
    ObsInputs,
    SearchArchive {
        query: String,
        room_id: Option<String>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    },
    LoadSession {
        session_id: String,
    },
}
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub operation_id: Uuid,
    pub state: OperationState,
    pub value: OperationValue,
    pub effects: Vec<HostEffect>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationState {
    Accepted,
    Completed,
    Failed { reason: String },
    Cancelled,
}
#[derive(Debug, Clone)]
pub enum OperationValue {
    None,
    StopConfirmation {
        token: Uuid,
    },
    Receipt {
        message: String,
    },
    RoomUpdate {
        receipt: crate::bilibili::RoomUpdateReceipt,
    },
    ManualDelivery {
        draft_revision: u64,
        delivery: crate::delivery::Delivery,
    },
}
#[derive(Debug, Clone)]
pub enum HostEffect {
    OpenBrowser { operation_id: Uuid, url: String },
}
#[derive(Debug, Clone)]
pub enum QueryResult {
    Snapshot(Box<super::events::RuntimeSnapshot>),
    Diagnostics(super::events::RuntimeDiagnostics),
    Settings(RuntimeSettings),
    Candidates(Vec<super::events::CandidateRecord>),
    SubscriptionAccounts(Vec<super::events::SubscriptionAccountRecord>),
    Models(Vec<super::events::ModelRecord>),
    ObsScenes(Vec<String>),
    ObsInputs(Vec<String>),
    Sessions(Vec<crate::domain::DanmuSession>),
    Session(crate::domain::DanmuSession),
}

use super::*;

impl RuntimeState {
    pub(crate) fn operation_token(&mut self) -> Uuid {
        if self.current_operation.is_nil() || self.operations.contains_key(&self.current_operation)
        {
            self.current_operation = Uuid::new_v4();
        }
        let token = self.current_operation;
        self.operations.insert(token, OperationState::Accepted);
        token
    }
    pub(crate) fn finish_operation(&mut self, operation_id: Uuid, result: Result<OperationValue>) {
        self.operation_tasks.remove(&operation_id);
        self.send_operations.remove(&operation_id);
        if self.operations.get(&operation_id) == Some(&OperationState::Cancelled) {
            return;
        }
        let (state, value) = match result {
            Ok(value) => (OperationState::Completed, value),
            Err(error) => (
                OperationState::Failed {
                    reason: error.to_string(),
                },
                OperationValue::None,
            ),
        };
        if let OperationState::Failed { reason } = &state {
            self.set_notice(reason.clone(), NoticeLevel::Error);
        } else if let OperationValue::Receipt { message } = &value {
            self.set_notice(message.clone(), NoticeLevel::Success);
        }
        self.operations.insert(operation_id, state.clone());
        self.pending_changes.push(RuntimeChange::Operation {
            operation_id,
            state,
            value,
        });
    }
    fn spawn_operation(
        &mut self,
        operation_id: Uuid,
        work: impl Future<Output = Result<OperationValue>> + Send + 'static,
    ) {
        let tx = self.ui_tx.as_ref().expect("core event channel").clone();
        let task = self.tasks.spawn(async move {
            let result = work.await;
            let _ = tx
                .send(CoreEvent::OperationDone {
                    operation_id,
                    result,
                })
                .await;
        });
        self.operation_tasks.insert(operation_id, task);
    }
    pub(crate) async fn execute_command(
        &mut self,
        command: RuntimeCommand,
    ) -> Result<CommandResult> {
        let operation_id = Uuid::new_v4();
        self.current_operation = operation_id;
        let mut accepted = false;
        let mut value = OperationValue::None;
        let result: Result<()> = async {
            match command {
                RuntimeCommand::Connect { room_id } => {
                    self.begin_connect(room_id, operation_id)?;
                    accepted = self.mode != RuntimeMode::Local;
                }
                RuntimeCommand::Disconnect => self.disconnect()?,
                RuntimeCommand::CancelOperation {
                    operation_id: target,
                } => {
                    let state = self.operations.get(&target).with_context(|| {
                        RuntimeFailure::new(RuntimeFailureKind::Stale, "操作不存在或已过期")
                    })?;
                    anyhow::ensure!(
                        *state == OperationState::Accepted,
                        RuntimeFailure::new(RuntimeFailureKind::Stale, "操作已结束，不能取消",)
                    );
                    // A POST already issued is never aborted or reclassified as unsent.
                    if let Some(permit) = self.send_operations.get(&target) {
                        permit.cancel();
                    } else {
                        if let Some(task) = self.operation_tasks.remove(&target) {
                            task.abort();
                        }
                        if self.stop_operation == Some(target) {
                            anyhow::ensure!(
                                !matches!(self.stop_flow, Some(StopFlow::Stopping)),
                                RuntimeFailure::new(
                                    RuntimeFailureKind::Busy,
                                    "停止请求已发出，不能声称已取消"
                                )
                            );
                            self.cancel_stop_request();
                        }
                        if self
                            .main_login
                            .as_ref()
                            .is_some_and(|login| login.token == target)
                        {
                            self.cancel_main_login();
                        }
                        if self
                            .chatgpt_operation
                            .as_ref()
                            .is_some_and(|login| login.token == target)
                        {
                            self.cancel_chatgpt_operation();
                        }
                        if self.connect_operation == Some(target) {
                            self.disconnect()?;
                        }
                        if self.assistant_accounts.pending_token() == Some(target) {
                            self.assistant_accounts.cancel_login();
                            self.refresh_login_projection();
                        }
                        if self.operations.get(&target) != Some(&OperationState::Cancelled) {
                            self.operations.insert(target, OperationState::Cancelled);
                            self.pending_changes.push(RuntimeChange::Operation {
                                operation_id: target,
                                state: OperationState::Cancelled,
                                value: OperationValue::None,
                            });
                        }
                    }
                }
                RuntimeCommand::SendManual {
                    text,
                    reply_to_message_id,
                    draft_revision,
                } => {
                    anyhow::ensure!(
                        self.mode != RuntimeMode::Replay,
                        RuntimeFailure::new(
                            RuntimeFailureKind::PermissionDenied,
                            "回放模式不能发送",
                        )
                    );
                    anyhow::ensure!(
                        !text.trim().is_empty()
                            && text.len() <= 65536
                            && !text.chars().any(|ch| ch.is_control() && ch != '\n'),
                        RuntimeFailure::new(
                            RuntimeFailureKind::InvalidInput,
                            "人工消息为空、过长或含控制字符"
                        )
                    );
                    let target = reply_to_message_id
                        .as_ref()
                        .map(|id| {
                            let event = self
                                .session
                                .recent_events
                                .iter()
                                .find(|event| &event.id == id)
                                .with_context(|| {
                                    RuntimeFailure::new(
                                        RuntimeFailureKind::Stale,
                                        "回复对象已离开当前消息集合",
                                    )
                                })?;
                            usable_author_id(event.author_id.as_deref())
                                .map(str::to_owned)
                                .with_context(|| {
                                    RuntimeFailure::new(
                                        RuntimeFailureKind::InvalidInput,
                                        "回复对象没有可靠UID",
                                    )
                                })
                        })
                        .transpose()?;
                    let segments = segment_message(&text, crate::bilibili::SEND_SEGMENT_LIMIT);
                    self.enqueue_manual(segments, target, draft_revision, operation_id);
                    accepted = true;
                }
                RuntimeCommand::Account(command) => {
                    accepted = self.account_command(command)?;
                }
                RuntimeCommand::Room(command) => {
                    anyhow::ensure!(
                        self.accounts_allowed(),
                        RuntimeFailure::new(
                            RuntimeFailureKind::PermissionDenied,
                            "本地与回放模式禁止直播间修改",
                        )
                    );
                    let account = self.account.snapshot()?;
                    let room = self.session.room_id.clone();
                    self.spawn_operation(operation_id, async move {
                        let receipt = match command {
                            RoomCommand::SetTitle { text } => {
                                account.update_room_title(&room, &text).await?
                            }
                            RoomCommand::SetCover { path } => {
                                account.update_room_cover(&room, &path).await?
                            }
                        };
                        Ok(OperationValue::RoomUpdate { receipt })
                    });
                    accepted = true;
                }
                RuntimeCommand::Assistant(command) => {
                    anyhow::ensure!(
                        self.mode != RuntimeMode::Replay
                            || !matches!(
                                command,
                                AssistantCommand::Start
                                    | AssistantCommand::ProcessVisible { .. }
                                    | AssistantCommand::SetAutomatic { enabled: true }
                                    | AssistantCommand::BeginChatgptLogin { .. }
                                    | AssistantCommand::SignOutChatgpt { .. }
                                    | AssistantCommand::RefreshModels
                            ),
                        RuntimeFailure::new(
                            RuntimeFailureKind::PermissionDenied,
                            "回放模式禁止模型与账号网络操作"
                        )
                    );
                    match command {
                        AssistantCommand::Start => self.start_assistant(false)?,
                        AssistantCommand::Pause => self.pause_assistant(),
                        AssistantCommand::Stop => self.stop_assistant(),
                        AssistantCommand::ProcessVisible { message_ids } => {
                            self.runner.process_visible(&self.bridge, &message_ids)?;
                            self.assistant_failure = None;
                        }
                        AssistantCommand::SetAutomatic { enabled } => {
                            self.set_automatic_permission(enabled)?
                        }
                        AssistantCommand::CancelPendingResume => {
                            self.resume_pending = false;
                            self.restore_scope = None;
                        }
                        AssistantCommand::SetRange { activity } => {
                            let activity = match activity {
                                ReplyActivity::Cautious => {
                                    crate::runner::settings::ReplyActivity::Cautious
                                }
                                ReplyActivity::Balanced => {
                                    crate::runner::settings::ReplyActivity::Balanced
                                }
                                ReplyActivity::Active => {
                                    crate::runner::settings::ReplyActivity::Active
                                }
                            };
                            self.runner.set_reply_activity_override(activity);
                        }
                        AssistantCommand::SetTopic { text } => {
                            self.runner.set_topic_override(text)?
                        }
                        AssistantCommand::ApplyPreset { preset } => {
                            let mut settings = self.runner.settings.clone();
                            match preset {
                                ReplyPreset::Quiet => crate::runner::settings::ReplyPreset::Quiet,
                                ReplyPreset::TextQa => crate::runner::settings::ReplyPreset::TextQa,
                                ReplyPreset::Thanks => crate::runner::settings::ReplyPreset::Thanks,
                            }
                            .apply(&mut settings);
                            self.runner.save(settings, &self.bridge)?;
                        }
                        AssistantCommand::SaveSettings { settings } => {
                            let mut settings = settings.into_core()?;
                            self.check_assistant_connection_change()?;
                            // Editing a preference document never grants or remembers permission.
                            settings.automatic = self.runner.settings.automatic;
                            settings.resume_on_start = self.runner.settings.resume_on_start;
                            self.runner.save(settings, &self.bridge)?;
                        }
                        AssistantCommand::SetApiKey { value } => {
                            self.check_assistant_connection_change()?;
                            self.runner.save_api_key(&value, &self.bridge)?;
                        }
                        AssistantCommand::ClearApiKey => {
                            self.check_assistant_connection_change()?;
                            self.runner.clear_api_key(&self.bridge)?;
                        }
                        AssistantCommand::BeginChatgptLogin { account_id } => {
                            self.begin_chatgpt_login(account_id.as_deref())?;
                            accepted = true;
                        }
                        AssistantCommand::SignOutChatgpt { account_id } => {
                            self.begin_chatgpt_sign_out(&account_id)?;
                            accepted = true;
                        }
                        AssistantCommand::RefreshModels => {
                            self.begin_chatgpt_models()?;
                            accepted = true;
                        }
                        AssistantCommand::SelectChatgptAccount { account_id } => {
                            self.check_assistant_connection_change()?;
                            anyhow::ensure!(
                                self.runner
                                    .chatgpt_accounts()?
                                    .iter()
                                    .any(|account| account.id == account_id),
                                RuntimeFailure::new(
                                    RuntimeFailureKind::Stale,
                                    "所选订阅账号不存在"
                                )
                            );
                            let mut settings = self.runner.settings.clone();
                            settings.api.chatgpt_account = Some(account_id);
                            settings.api.model.clear();
                            self.runner.save(settings, &self.bridge)?;
                            self.models.clear();
                        }
                        AssistantCommand::SelectModel { model_id } => {
                            self.check_assistant_connection_change()?;
                            if self.runner.settings.api.protocol
                                == crate::runner::settings::ApiProtocol::ChatgptSubscription
                            {
                                anyhow::ensure!(
                                    self.models.iter().any(|model| model.model_id == model_id),
                                    RuntimeFailure::new(
                                        RuntimeFailureKind::InvalidInput,
                                        "模型不属于当前账号目录"
                                    )
                                );
                            }
                            let mut settings = self.runner.settings.clone();
                            settings.api.model = model_id;
                            self.runner.save(settings, &self.bridge)?;
                        }
                        AssistantCommand::RepairHistory => {
                            self.runner.repair_history(&self.journal)?
                        }
                    }
                }
                RuntimeCommand::Review(command) => self.review_command(command)?,
                RuntimeCommand::Obs(command) => {
                    anyhow::ensure!(
                        self.obs_allowed,
                        RuntimeFailure::new(
                            RuntimeFailureKind::PermissionDenied,
                            "本地与回放模式禁止真实OBS操作",
                        )
                    );
                    let obs = self.obs.clone();
                    if matches!(command, ObsCommand::Connect | ObsCommand::Refresh) {
                        self.start_obs_monitor();
                    }
                    #[cfg(any(test, feature = "test-support"))]
                    if self.mode == RuntimeMode::Local
                        && let ObsCommand::SaveConfiguration { configuration } = &command
                    {
                        anyhow::ensure!(
                            crate::test_support::is_fake_obs_endpoint(
                                &configuration.host,
                                configuration.port
                            ),
                            RuntimeFailure::new(
                                RuntimeFailureKind::PermissionDenied,
                                "本地模式仅允许已安装的隔离OBS端点"
                            )
                        );
                    }
                    match command {
                        ObsCommand::RequestStop => {
                            anyhow::ensure!(
                                self.stop_token.is_none(),
                                RuntimeFailure::new(RuntimeFailureKind::Busy, "已有停止推流确认",)
                            );
                            let token = Uuid::new_v4();
                            self.stop_token =
                                Some((token, Instant::now() + Duration::from_secs(60)));
                            self.stop_flow = Some(StopFlow::Confirm {
                                stop_selected: false,
                            });
                            value = OperationValue::StopConfirmation { token };
                        }
                        ObsCommand::ConfirmStop { token } => {
                            self.confirm_stop_request(token, Instant::now(), operation_id)?;
                            accepted = true;
                        }
                        ObsCommand::CancelStop { token } => {
                            anyhow::ensure!(
                                self.stop_token
                                    .is_some_and(|(expected, _)| expected == token),
                                RuntimeFailure::new(
                                    RuntimeFailureKind::Stale,
                                    "停止推流确认已失效"
                                )
                            );
                            anyhow::ensure!(
                                !matches!(self.stop_flow, Some(StopFlow::Stopping)),
                                RuntimeFailure::new(
                                    RuntimeFailureKind::Busy,
                                    "停止请求已发出，不能声称已取消"
                                )
                            );
                            self.cancel_stop_request();
                        }
                        command => {
                            let tx = self.ui_tx.as_ref().expect("core event channel").clone();
                            self.spawn_operation(operation_id, async move {
                                let message = match command {
                                    ObsCommand::Connect | ObsCommand::Refresh => {
                                        let status = obs
                                            .fetch_status()
                                            .await
                                            .map_err(|error| error.to_string());
                                        let _ = tx.send(CoreEvent::ObsStatus(status.clone())).await;
                                        let status = status.map_err(anyhow::Error::msg)?;
                                        format!(
                                            "OBS 场景：{}；推流：{:?}；麦克风：{:?}{}",
                                            status.current_scene,
                                            status.stream,
                                            status.microphone,
                                            status
                                                .compatibility_warning
                                                .map(|warning| format!("；{warning}"))
                                                .unwrap_or_default()
                                        )
                                    }
                                    ObsCommand::SaveConfiguration { configuration } => {
                                        obs.update_configuration(configuration).await?;
                                        "OBS配置已保存".into()
                                    }
                                    ObsCommand::SetPassword { password } => {
                                        obs.set_password(&password).await?;
                                        "OBS密码已保存".into()
                                    }
                                    ObsCommand::SetMicrophone { name } => {
                                        obs.set_microphone_name(name).await?;
                                        "OBS麦克风输入已保存".into()
                                    }
                                    ObsCommand::SetMuted { muted } => {
                                        obs.set_microphone_muted(muted).await?;
                                        if muted {
                                            "麦克风已静音"
                                        } else {
                                            "麦克风已取消静音"
                                        }
                                        .into()
                                    }
                                    ObsCommand::SetScene { name } => {
                                        obs.switch_scene(&name).await?;
                                        format!("已切换OBS场景：{name}")
                                    }
                                    ObsCommand::StartStream => {
                                        obs.start_stream().await?;
                                        "OBS 已开始推流".into()
                                    }
                                    ObsCommand::RequestStop
                                    | ObsCommand::ConfirmStop { .. }
                                    | ObsCommand::CancelStop { .. } => {
                                        unreachable!("handled before spawning")
                                    }
                                };
                                Ok(OperationValue::Receipt { message })
                            });
                            accepted = true;
                        }
                    }
                }
                RuntimeCommand::FeatureMessage { message_id } => {
                    if let Some(id) = &message_id {
                        anyhow::ensure!(
                            self.session
                                .recent_events
                                .iter()
                                .any(|event| &event.id == id),
                            RuntimeFailure::new(
                                RuntimeFailureKind::Stale,
                                "重点消息已离开当前消息集合"
                            )
                        );
                    }
                    self.session.feature(message_id.as_deref());
                    self.journal.snapshot(&self.session)?;
                }
                RuntimeCommand::SetExternalEndpoint { root } => {
                    anyhow::ensure!(
                        self.mode != RuntimeMode::Replay,
                        RuntimeFailure::new(
                            RuntimeFailureKind::PermissionDenied,
                            "回放模式禁止外部端点",
                        )
                    );
                    let replacement = match root {
                        Some(root) => Some(bridge::wire::serve(&root, self.bridge.clone()).await?),
                        None => None,
                    };
                    self.endpoint = replacement;
                }
                RuntimeCommand::ExportSession {
                    session_id,
                    format,
                    destination,
                } => {
                    validate_session_id(&session_id)?;
                    anyhow::ensure!(
                        destination.is_absolute(),
                        RuntimeFailure::new(
                            RuntimeFailureKind::InvalidInput,
                            "导出目标必须为绝对路径",
                        )
                    );
                    let journal = self.journal.clone();
                    self.spawn_operation(operation_id, async move {
                        tokio::task::spawn_blocking(move || -> Result<OperationValue> {
                            let session =
                                journal.latest_session(&session_id)?.with_context(|| {
                                    RuntimeFailure::new(RuntimeFailureKind::Stale, "场次不存在")
                                })?;
                            let (json, markdown) = crate::persistence::serialize_exports(&session)?;
                            let data = match format {
                                ExportFormat::Json => json,
                                ExportFormat::Markdown => markdown,
                            };
                            if let Ok(metadata) = std::fs::symlink_metadata(&destination) {
                                anyhow::ensure!(
                                    !metadata.file_type().is_symlink(),
                                    RuntimeFailure::new(
                                        RuntimeFailureKind::InvalidInput,
                                        "导出目标不能是符号链接"
                                    )
                                );
                            }
                            crate::storage::write_private_atomic(&destination, &data)?;
                            Ok(OperationValue::None)
                        })
                        .await
                        .context("导出任务失败")?
                    });
                    accepted = true;
                }
            }
            Ok(())
        }
        .await;
        let state = if accepted {
            OperationState::Accepted
        } else {
            OperationState::Completed
        };
        match result {
            Ok(()) => {
                self.operations.insert(operation_id, state.clone());
                if self.operations.len() > 2048
                    && let Some(old) = self.operations.iter().find_map(|(id, state)| {
                        (*id != operation_id && *state != OperationState::Accepted).then_some(*id)
                    })
                {
                    self.operations.remove(&old);
                }
                Ok(CommandResult {
                    operation_id,
                    state,
                    value,
                    effects: std::mem::take(&mut self.effects),
                })
            }
            Err(error) => {
                self.effects.clear();
                self.operations.insert(
                    operation_id,
                    OperationState::Failed {
                        reason: error.to_string(),
                    },
                );
                Err(error)
            }
        }
    }
    fn review_command(&mut self, command: ReviewCommand) -> Result<()> {
        fn resolve(state: &RuntimeState, expected: &ReviewedCandidate) -> Result<bridge::Reply> {
            anyhow::ensure!(
                expected.identity_generation == state.assistant_accounts.generation(),
                RuntimeFailure::new(RuntimeFailureKind::Stale, "发送身份已改变，请重新查看候选")
            );
            state
                .bridge
                .candidates()
                .into_iter()
                .find(|reply| {
                    reply.session == expected.session
                        && reply.caller == expected.caller
                        && reply.request_id == expected.request_id
                        && reply.revision == expected.revision
                })
                .with_context(|| {
                    RuntimeFailure::new(RuntimeFailureKind::Stale, "候选已改变或已处理，请重新查看")
                })
        }
        match command {
            ReviewCommand::Edit { expected, text } => {
                let reply = resolve(self, &expected)?;
                self.bridge.edit_candidate(
                    &reply.session,
                    &reply.caller,
                    &reply.request_id,
                    &text,
                )?;
            }
            ReviewCommand::Approve { expected } => {
                anyhow::ensure!(
                    !expected.is_empty(),
                    RuntimeFailure::new(RuntimeFailureKind::InvalidInput, "暂无待审回复",)
                );
                let replies = expected
                    .iter()
                    .map(|expected| resolve(self, expected))
                    .collect::<Result<Vec<_>>>()?;
                self.bridge.approve_candidates(&replies)?;
            }
            ReviewCommand::Discard { expected } => {
                let reply = resolve(self, &expected)?;
                self.bridge
                    .decide(&reply.caller, &reply.request_id, false)?;
            }
        }
        Ok(())
    }

    pub(crate) fn query_command(
        &mut self,
        query: RuntimeQuery,
        reply: oneshot::Sender<Result<QueryResult>>,
    ) {
        match query {
            RuntimeQuery::ObsScenes | RuntimeQuery::ObsInputs => {
                if !self.obs_allowed {
                    let _ = reply.send(Err(RuntimeFailure::new(
                        RuntimeFailureKind::PermissionDenied,
                        "本地与回放模式禁止真实OBS查询",
                    )
                    .into()));
                    return;
                }
                let obs = self.obs.clone();
                self.tasks.spawn(async move {
                    let value = match query {
                        RuntimeQuery::ObsScenes => {
                            obs.list_scenes().await.map(QueryResult::ObsScenes)
                        }
                        _ => obs.list_inputs().await.map(QueryResult::ObsInputs),
                    };
                    let _ = reply.send(value);
                });
            }
            RuntimeQuery::Settings => {
                let local = settings::AssistantSettings::from_core(&self.runner.settings)
                    .and_then(|assistant| Ok((assistant, self.runner.workspace_path()?)));
                let (assistant, resolved_workspace) = match local {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return;
                    }
                };
                let api_key = match &self.runner.key_status {
                    crate::runner::KeyStatus::Set => settings::ApiKeyStatus::Set,
                    crate::runner::KeyStatus::Unset => settings::ApiKeyStatus::Unset,
                    crate::runner::KeyStatus::Error(reason) => settings::ApiKeyStatus::Error {
                        reason: reason.clone(),
                    },
                };
                let obs = self.obs.clone();
                self.tasks.spawn(async move {
                    let value = RuntimeSettings {
                        assistant,
                        obs: obs.configuration().await,
                        resolved_workspace,
                        api_key,
                    };
                    let _ = reply.send(Ok(QueryResult::Settings(value)));
                });
            }
            RuntimeQuery::SearchArchive {
                query,
                room_id,
                from,
                to,
            } => {
                let journal = self.journal.clone();
                self.tasks.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || -> Result<QueryResult> {
                        let sessions = journal
                            .search(&query)?
                            .into_iter()
                            .filter(|session| {
                                room_id.as_ref().is_none_or(|room| room == &session.room_id)
                                    && from.is_none_or(|from| session.started_at >= from)
                                    && to.is_none_or(|to| session.started_at <= to)
                            })
                            .collect();
                        Ok(QueryResult::Sessions(sessions))
                    })
                    .await
                    .map_err(|error| anyhow!("归档检索任务失败：{error}"))
                    .and_then(|value| value);
                    let _ = reply.send(result);
                });
            }
            RuntimeQuery::LoadSession { session_id } => {
                let journal = self.journal.clone();
                self.tasks.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        validate_session_id(&session_id)?;
                        journal
                            .latest_session(&session_id)?
                            .map(QueryResult::Session)
                            .with_context(|| {
                                RuntimeFailure::new(RuntimeFailureKind::Stale, "场次不存在")
                            })
                    })
                    .await
                    .map_err(|error| anyhow!("档案读取任务失败：{error}"))
                    .and_then(|value| value);
                    let _ = reply.send(result);
                });
            }
            query => {
                let result = match query {
                    RuntimeQuery::Snapshot => Ok(QueryResult::Snapshot(Box::new(self.snapshot()))),
                    RuntimeQuery::Diagnostics => Ok(QueryResult::Diagnostics(RuntimeDiagnostics {
                        connection: self.connection.clone(),
                        assistant_note: self.runner.note.clone(),
                        assistant_details: super::diagnostics::diagnostics_details(self),
                        history_warning: self.runner.history_warning.clone(),
                        config_warning: self.runner.config_warning.clone(),
                        obs_error: self.obs_error.clone(),
                        operations: self
                            .operations
                            .iter()
                            .map(|(id, state)| (*id, state.clone()))
                            .collect(),
                    })),
                    RuntimeQuery::Candidates => {
                        Ok(QueryResult::Candidates(self.reply_records(true)))
                    }
                    RuntimeQuery::SubscriptionAccounts => {
                        self.runner.chatgpt_accounts().map(|accounts| {
                            QueryResult::SubscriptionAccounts(
                                accounts
                                    .into_iter()
                                    .map(|account| SubscriptionAccountRecord {
                                        id: account.id,
                                        label: account.label,
                                        email: account.email,
                                        signed_in: account.signed_in,
                                        plan_enabled: account.plan_enabled,
                                    })
                                    .collect(),
                            )
                        })
                    }
                    RuntimeQuery::Models => Ok(QueryResult::Models(self.models.clone())),
                    _ => unreachable!("asynchronous queries handled above"),
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn validate_session_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && Path::new(id).components().count() == 1
            && !matches!(id, "." | "..")
            && !id.contains(['/', '\\']),
        RuntimeFailure::new(RuntimeFailureKind::InvalidInput, "无效场次标识")
    );
    Ok(())
}
