use super::*;
use crate::bridge::AI_PREFIX;
use crate::delivery::{Cause, Delivery};
use std::sync::atomic::{AtomicU8, Ordering};
pub(crate) fn assistant_segments(text: &str) -> Vec<String> {
    let mut body = text.trim();
    while let Some(rest) = body.strip_prefix(AI_PREFIX.trim_end()) {
        body = rest.trim_start();
    }
    if body.is_empty() {
        return vec![AI_PREFIX.trim_end().into()];
    }
    segment_message(
        body,
        crate::bilibili::SEND_SEGMENT_LIMIT - AI_PREFIX.graphemes(true).count(),
    )
    .into_iter()
    .map(|segment| format!("{AI_PREFIX}{segment}"))
    .collect()
}
/// Explicit local-only transport; no network clients or credential access.
pub(crate) struct LocalTransport {
    pub(crate) outcome: AtomicU8,
    pub(crate) log: PathBuf,
    pub(crate) tx: mpsc::Sender<CoreEvent>,
}
impl Transport for LocalTransport {
    async fn send_confirm(&self, text: &str, reply_to: Option<&str>) -> Delivery {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mode = self.outcome.load(Ordering::Relaxed);
        let outcome = match mode {
            0 | 6 => Outcome::Confirmed,
            1 => Outcome::Uncertain,
            3 => Outcome::Uncertain,
            _ => Outcome::Rejected,
        };
        let record = serde_json::json!({"transport":"local","text":text,"reply_to":reply_to,"outcome":format!("{outcome:?}"),"at":Utc::now()});
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let write = (|| -> std::io::Result<()> {
            let mut file = options.open(&self.log)?;
            std::io::Write::write_all(&mut file, format!("{record}\n").as_bytes())
        })();
        if write.is_err() {
            return Outcome::Rejected.into();
        }
        if outcome == Outcome::Confirmed {
            let _ = self.tx.send(CoreEvent::LocalEcho(text.into())).await;
        }
        match mode {
            3 => {
                let mut delivery = Delivery::new(
                    outcome,
                    Cause::EchoMissing,
                    "受控响应已收到但无回显；疑似被吞，不推断具体词；改写可能重复",
                );
                delivery.diagnosis.response_received = true;
                delivery
            }
            4 | 5 => {
                self.outcome
                    .compare_exchange(5, 0, Ordering::Relaxed, Ordering::Relaxed)
                    .ok();
                Delivery::new(outcome, Cause::ContentRejected, "受控平台明确内容拒绝")
            }
            6 => {
                self.outcome.store(5, Ordering::Relaxed);
                outcome.into()
            }
            _ => outcome.into(),
        }
    }
}
struct GuardedTransport<'a, T> {
    pub(crate) inner: &'a T,
    pub(crate) bridge: &'a bridge::Bridge,
    pub(crate) reply: Option<&'a bridge::Reply>,
}
impl<T: Transport> Transport for GuardedTransport<'_, T> {
    async fn send_confirm(&self, text: &str, reply_to: Option<&str>) -> Delivery {
        if let Some(reply) = self.reply
            && let Some(word) = self.bridge.blocked_word(reply, &reply.text)
        {
            return Delivery::new(
                Outcome::Rejected,
                Cause::KnownWord,
                format!("命中用户配置的已知词「{word}」；未向平台发送，不认定为平台本次屏蔽证据"),
            );
        }
        self.inner.send_confirm(text, reply_to).await
    }
}
#[derive(Default)]
pub(crate) struct ProfileState {
    pub(crate) room_id: String,
    pub(crate) user_id: String,
    pub(crate) value: Option<crate::bilibili::PublicProfile>,
    pub(crate) error: Option<String>,
    pub(crate) checked_at: Option<Instant>,
    pub(crate) pending: bool,
}

impl RuntimeState {
    pub(crate) fn fail_assistant(&mut self, message: String) -> String {
        let message = self.revoke_assistant(message);
        self.assistant_failure = Some(message.clone());
        message
    }

    fn revoke_assistant(&mut self, mut message: String) -> String {
        self.resume_pending = false;
        self.cancel_chatgpt_operation();
        self.runner.pause(&self.bridge);
        for (operation, result) in [
            ("保存助手停用状态", self.runner.remember_enabled(false)),
            (
                "清除自动发送授权",
                self.assistant_accounts.forget_automatic(),
            ),
        ] {
            if let Err(error) = result {
                message.push_str(&format!("；{operation}失败：{error}"));
            }
        }
        message
    }

    pub(crate) fn sync_assistant_room(&mut self) {
        if let Some(room) = &self.room {
            if self.profile.user_id != room.broadcaster_id || self.profile.room_id != room.room_id {
                self.profile = ProfileState {
                    room_id: room.room_id.clone(),
                    user_id: room.broadcaster_id.clone(),
                    ..ProfileState::default()
                };
            }
            self.runner
                .set_room_context(Some(crate::runner::RoomContext {
                    room_id: room.room_id.clone(),
                    broadcaster_id: room.broadcaster_id.clone(),
                    title: room.title.clone(),
                    area: room.area.clone(),
                    broadcaster_name: room.broadcaster_name.clone(),
                    profile: self.profile.value.as_ref().map(|p| p.description.clone()),
                    profile_source: self.profile.value.as_ref().map(|p| p.source_url.clone()),
                }));
        } else {
            self.runner.set_room_context(None);
        }
    }

    pub(crate) fn refresh_assistant_profile(&mut self, tx: &mpsc::Sender<CoreEvent>) {
        if self.bridge.is_local()
            || !self.runner.settings.use_profile
            || self.profile.pending
            || !self.runner.running
        {
            return;
        }
        let Some(room) = &self.room else {
            return;
        };
        let age = if self.profile.error.is_some() {
            60
        } else {
            15 * 60
        };
        if self
            .profile
            .checked_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(age))
        {
            return;
        }
        let room_id = room.room_id.clone();
        let user_id = room.broadcaster_id.clone();
        self.profile.pending = true;
        let client = self.client.clone();
        let tx = tx.clone();
        let generation = self.connection_generation;
        self.tasks.spawn(async move {
            let result = client
                .broadcaster_profile(&user_id)
                .await
                .map_err(|e| e.to_string());
            let _ = tx
                .send(CoreEvent::Scoped {
                    generation,
                    event: Box::new(CoreEvent::BroadcasterProfile {
                        room_id,
                        user_id,
                        result,
                    }),
                })
                .await;
        });
    }

    pub(crate) fn advance_bridge(&mut self, tx: mpsc::Sender<CoreEvent>) {
        if self.local_transport.is_none() {
            let live = self.room.as_ref().is_some_and(RoomSnapshot::is_live);
            let scope = self
                .room
                .as_ref()
                .filter(|r| r.is_live())
                .map(|r| format!("{}:{:?}", r.room_id, r.live_started_at));
            if live
                && (!self.bridge.is_active()
                    || self
                        .live_scope
                        .as_ref()
                        .is_some_and(|old| Some(old) != scope.as_ref()))
            {
                self.bridge.new_session();
            }
            if live {
                self.live_scope = scope;
            } else if self.live_scope.take().is_some() {
                self.bridge.end();
            }
            self.bridge
                .available(live && self.connection.starts_with("已连接") && !self.quit_requested);
        }
        self.runner
            .set_author(match self.assistant_accounts.status(&self.account_status) {
                AccountStatus::SignedIn { user_id, .. } => Some(user_id),
                _ => None,
            });
        self.refresh_assistant_profile(&tx);
        let runner_was_running = self.runner.running;
        self.runner.tick(&self.bridge);
        let runner_stop = (runner_was_running && !self.runner.running && self.runner.failed())
            .then(|| self.runner.note.clone());
        if let Some(journal) = self.runner.take_loaded_history_journal() {
            self.journal = journal;
        }
        if let Some(message) = self.runner.take_history_load_notice() {
            self.set_notice(message, NoticeLevel::Error);
        }
        if let Some(result) = self.runner.take_history_repair_notice() {
            match result {
                Ok(message) => self.set_notice(message, NoticeLevel::Success),
                Err(message) => self.set_notice(message, NoticeLevel::Error),
            }
        }
        let mut fatal = runner_stop
            .as_ref()
            .map(|reason| format!("AI异常停止：{reason}；/diag 查看"));
        let mut archive_failed = false;
        if let Err(error) =
            self.runner
                .record_routing(&self.bridge, &self.journal, &self.session, false)
        {
            archive_failed = true;
            self.storage_error = Some(error.to_string());
            let detail = format!("消息分流归档失败：{error}");
            fatal
                .get_or_insert_with(String::new)
                .push_str(&format!("；{detail}"));
        }
        if let Some(record) = self.runner.round_record.take() {
            match self.journal.reply_record(&self.session.id, &record) {
                Ok(()) if record["outcome"] == "rejected" => {
                    self.set_notice(self.runner.note.clone(), NoticeLevel::Warning);
                }
                Ok(()) => {}
                Err(error) => {
                    archive_failed = true;
                    self.storage_error = Some(error.to_string());
                    fatal
                        .get_or_insert_with(String::new)
                        .push_str(&format!("；轮次诊断归档失败：{error}"));
                }
            }
        }
        if let Some(reason) = runner_stop
            && let Err(error) = self.journal.assistant_stopped(&self.session, &reason)
        {
            self.storage_error = Some(error.to_string());
            fatal
                .get_or_insert_with(String::new)
                .push_str(&format!("；停止原因归档失败：{error}"));
        }
        if let Some(reason) = fatal {
            let message = self.fail_assistant(reason);
            self.set_notice(message, NoticeLevel::Error);
        }
        #[cfg(feature = "tui")]
        self.select_candidate();
        if archive_failed {
            return;
        }
        while let Some(job) = self.bridge.take_ready() {
            let segments = assistant_segments(&job.reply.text);
            self.enqueue(segments, Some(job), tx.clone());
        }
    }

    pub(crate) fn enqueue(
        &mut self,
        segments: Vec<String>,
        job: Option<bridge::Job>,
        tx: mpsc::Sender<CoreEvent>,
    ) {
        self.enqueue_delivery(segments, job, tx, None);
    }

    pub(crate) fn enqueue_manual(
        &mut self,
        segments: Vec<String>,
        reply_to: Option<String>,
        draft_revision: u64,
        operation_id: uuid::Uuid,
    ) {
        let permit = crate::delivery::Permit::new(DateTime::<Utc>::MAX_UTC);
        self.send_operations.insert(operation_id, permit.clone());
        self.enqueue_delivery(
            segments,
            None,
            self.ui_tx.as_ref().expect("core event channel").clone(),
            Some((reply_to, draft_revision, operation_id, permit)),
        );
    }

    fn enqueue_delivery(
        &mut self,
        segments: Vec<String>,
        job: Option<bridge::Job>,
        tx: mpsc::Sender<CoreEvent>,
        manual: Option<(Option<String>, u64, uuid::Uuid, crate::delivery::Permit)>,
    ) {
        let account = if job.is_some() {
            self.assistant_accounts.send_identity(&self.account_status)
        } else {
            accounts::SendIdentity::manual(self.account.clone())
        };
        // Select from the frozen identity, including unavailable independent identities.
        // Never let an independent account fall back to the manual lane.
        let queue = if account.is_independent() {
            &self.independent_send_queue
        } else {
            &self.manual_send_queue
        }
        .clone();
        let generation = queue.generation();
        let transport = RuntimeTransport {
            account,
            client: self.client.clone(),
            room: self.session.room_id.clone(),
            tx: tx.clone(),
            job: job.as_ref().map(|j| j.reply.clone()),
            bridge: self.bridge.clone(),
            queue: queue.clone(),
            manual_permit: manual.as_ref().map(|(_, _, _, permit)| permit.clone()),
        };
        let local = self.local_transport.clone();
        let bridge = self.bridge.clone();
        let archive = job.as_ref().map(|_| {
            let sender_uid = match self.assistant_accounts.status(&self.account_status) {
                AccountStatus::SignedIn { user_id, .. } => Some(user_id.clone()),
                _ => None,
            };
            (self.session.id.clone(), sender_uid)
        });
        self.tasks.spawn(async move {
            let permit = job
                .as_ref()
                .map(|job| &job.permit)
                .or_else(|| manual.as_ref().map(|(_, _, _, permit)| permit));
            let reply_to = job
                .as_ref()
                .and_then(|job| job.reply.reply_to.as_ref())
                .map(|target| target.user_id.as_str())
                .or_else(|| {
                    manual
                        .as_ref()
                        .and_then(|(target, _, _, _)| target.as_deref())
                });
            let reply = job.as_ref().map(|j| &j.reply);
            let delivery = if queue.generation() != generation {
                Outcome::Cancelled.into()
            } else if let Some(local) = local {
                queue
                    .send(
                        &GuardedTransport {
                            inner: &*local,
                            bridge: &bridge,
                            reply,
                        },
                        &segments,
                        permit,
                        reply_to,
                    )
                    .await
            } else {
                queue
                    .send(
                        &GuardedTransport {
                            inner: &transport,
                            bridge: &bridge,
                            reply,
                        },
                        &segments,
                        permit,
                        reply_to,
                    )
                    .await
            };
            let outcome = delivery.outcome;
            let notice = (outcome != Outcome::Confirmed).then(|| {
                if job.is_some() {
                    format!(
                        "{} · {:?}；详情见助手",
                        if outcome == Outcome::Uncertain {
                            "未确认送达，不自动重发"
                        } else {
                            "发送未完成"
                        },
                        delivery.diagnosis.cause,
                    )
                } else {
                    format!(
                        "人工{}，可发新消息：{}；本次不重发",
                        match outcome {
                            Outcome::Uncertain => "发送未确认",
                            Outcome::Rejected => "发送被拒绝",
                            _ => "发送已取消",
                        },
                        delivery.diagnosis.detail,
                    )
                }
            });
            let record = job
                .as_ref()
                .zip(archive)
                .map(
                    |(job, (session_id, sender_uid))| CoreEvent::AssistantDelivery {
                        session_id,
                        record: serde_json::json!({
                            "stage": "send_completed",
                            "bridge_session": job.reply.session,
                            "caller": job.reply.caller,
                            "request_id": job.reply.request_id,
                            "message_id": job.reply.message_id,
                            "sender_uid": sender_uid,
                            "outcome": delivery.outcome,
                            "diagnosis": delivery.diagnosis,
                            "confirmed_segments": delivery.confirmed_segments,
                            "unconfirmed_segments": delivery.unconfirmed_segments,
                        }),
                    },
                );
            let manual_result = if let Some((_, draft_revision, operation_id, _)) = manual {
                Some((
                    operation_id,
                    OperationValue::ManualDelivery {
                        draft_revision,
                        delivery,
                    },
                ))
            } else {
                if let Some(job) = job {
                    bridge.complete(&job.reply, delivery);
                }
                None
            };
            let _ = tx
                .send(match notice {
                    Some(message) => CoreEvent::DeliveryNotice(message),
                    None => CoreEvent::DeliveryCompleted,
                })
                .await;
            if let Some(record) = record {
                let _ = tx.send(record).await;
            }
            if let Some((operation_id, value)) = manual_result {
                let _ = tx
                    .send(CoreEvent::OperationDone {
                        operation_id,
                        result: Ok(value),
                    })
                    .await;
            }
        });
        self.set_delivery_status(DeliveryStatus::Sending);
    }

    pub(crate) fn set_automatic_permission(&mut self, enabled: bool) -> Result<()> {
        let mut settings = self.runner.settings.clone();
        settings.automatic = enabled;
        if !enabled {
            self.bridge.permission(false)?;
            let saved = self.runner.save(settings, &self.bridge);
            let forgotten = self.assistant_accounts.forget_automatic();
            saved?;
            forgotten?;
            return Ok(());
        }
        anyhow::ensure!(self.bridge.is_active(), "当前场次已结束");
        if !self.bridge.is_local()
            && let Err(error) = self
                .assistant_accounts
                .validate_automatic_identity(&self.session.room_id, &self.account_status)
        {
            return Err(self.reject_automatic_identity(error));
        }
        settings.resume_on_start = true;
        self.runner.save(settings, &self.bridge)?;
        if !self.bridge.is_local()
            && let Err(error) = self
                .assistant_accounts
                .remember_automatic(&self.session.room_id, &self.account_status)
        {
            return Err(self.reject_automatic_identity(error));
        }
        self.bridge.permission(true)?;
        Ok(())
    }

    pub(crate) fn reject_automatic_identity(&mut self, error: anyhow::Error) -> anyhow::Error {
        let message = self.fail_assistant(error.to_string());
        #[cfg(feature = "tui")]
        let _ = self.open_settings_section("account");
        self.set_notice(message.clone(), NoticeLevel::Error);
        anyhow::anyhow!(message)
    }

    pub(crate) fn restore_automatic(
        &mut self,
        expected_session: &str,
        expected_generation: u64,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.bridge.is_local()
                && self.bridge.is_active()
                && self.bridge.session_id() == expected_session
                && self.assistant_accounts.generation() == expected_generation
                && self.runner.settings.automatic
                && self.runner.settings.resume_on_start
                && self
                    .assistant_accounts
                    .automatic_matches(&self.session.room_id, &self.account_status),
            "房间、发送身份或自动设置已改变，未恢复自动发送"
        );
        let identity = self.assistant_accounts.send_identity(&self.account_status);
        anyhow::ensure!(
            identity.valid() && identity.account().is_some(),
            "当前发送身份已不可用"
        );
        self.bridge.permission(true)?;
        Ok(())
    }

    pub(crate) fn pause_assistant(&mut self) {
        let message = self.revoke_assistant("助手已暂停并撤权".into());
        if message != "助手已暂停并撤权" {
            self.assistant_failure = Some(message.clone());
            self.set_notice(message, NoticeLevel::Error);
        }
    }
}

impl RuntimeState {
    pub(crate) fn start_assistant(&mut self, visible: bool) -> Result<()> {
        anyhow::ensure!(self.mode != RuntimeMode::Replay, "回放模式禁止调用模型");
        anyhow::ensure!(
            !self.runner.connection_busy(),
            "请先完成或取消账号/模型操作"
        );
        self.resume_pending = false;
        self.restore_scope = None;
        self.runner.settings.ready()?;
        self.runner.start(&self.bridge, visible)?;
        self.assistant_failure = None;
        self.set_notice(
            "AI已在本机就绪，等待新消息；尚未验证远端鉴权",
            NoticeLevel::Info,
        );
        Ok(())
    }
    pub(crate) fn stop_assistant(&mut self) {
        self.pause_assistant();
        self.runner.stop(&self.bridge);
    }
}
