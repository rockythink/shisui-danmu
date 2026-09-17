#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;

use super::*;
use crate::bridge::AI_PREFIX;
use crate::delivery::{Cause, Delivery};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

pub(super) fn assistant_segments(text: &str) -> Vec<String> {
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

pub(super) struct ReviewFrame {
    reply: bridge::Reply,
    generation: u64,
    visible: bool,
    complete: bool,
    delivery: bool,
    expanded: bool,
    width: u16,
    seen_rows: usize,
}

pub(super) struct CandidateEdit {
    generation: u64,
    session: String,
    caller: String,
    request: String,
    draft: EditorInput,
}

/// Explicit local-only transport; no network clients or credential access.
pub(super) struct LocalTransport {
    outcome: AtomicU8,
    log: PathBuf,
    tx: mpsc::Sender<UiEvent>,
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
            let _ = self.tx.send(UiEvent::LocalEcho(text.into())).await;
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
    inner: &'a T,
    bridge: &'a bridge::Bridge,
    reply: Option<&'a bridge::Reply>,
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
pub(super) struct ProfileState {
    pub(super) room_id: String,
    pub(super) user_id: String,
    pub(super) value: Option<crate::bilibili::PublicProfile>,
    pub(super) error: Option<String>,
    pub(super) checked_at: Option<Instant>,
    pub(super) pending: bool,
}

impl TerminalApp {
    pub(super) fn clear_review_surface(&mut self) {
        if let Some(review) = &mut self.review_frame {
            review.visible = false;
            if review.delivery {
                match self.bridge.reply_state(&review.reply) {
                    Some(bridge::Execution::Confirmed) | None => self.review_frame = None,
                    Some(state) => review.reply.state = state,
                }
            }
        }
    }
    pub(super) fn prepare_review(&mut self, expanded: bool) {
        if let Some(review) = &mut self.review_frame
            && review.delivery
        {
            review.expanded = expanded;
            return;
        }
        let reply = self
            .bridge
            .selected_candidate(self.candidate_selected.as_ref());
        let Some(reply) = reply else {
            self.review_frame = None;
            return;
        };
        if !self
            .candidate_selected
            .as_ref()
            .is_some_and(|(caller, request)| {
                caller == &reply.caller && request == &reply.request_id
            })
        {
            self.candidate_selected = Some((reply.caller.clone(), reply.request_id.clone()));
        }
        let generation = self.assistant_accounts.generation();
        let unchanged = self.review_frame.as_ref().is_some_and(|shown| {
            shown.generation == generation
                && shown.reply.session == reply.session
                && shown.reply.caller == reply.caller
                && shown.reply.request_id == reply.request_id
                && shown.reply.revision == reply.revision
                && shown.reply.text == reply.text
                && shown.expanded == expanded
        });
        if !unchanged {
            self.review_frame = Some(ReviewFrame {
                reply,
                generation,
                visible: false,
                complete: false,
                delivery: false,
                expanded,
                width: 0,
                seen_rows: 0,
            });
        }
    }
    pub(super) fn reviewed_reply(&self, require_complete: bool) -> Result<&bridge::Reply> {
        let shown = self
            .review_frame
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("请先查看回复正文"))?;
        anyhow::ensure!(
            shown.visible && shown.generation == self.assistant_accounts.generation(),
            "发送身份或可见页面已改变，请重新查看候选"
        );
        anyhow::ensure!(
            !shown.delivery,
            "本条已批准或已处理，不能重复发送；请查看结果"
        );
        anyhow::ensure!(
            !require_complete || shown.complete,
            "正文尚未完整查看；请展开并从头滚动查看后发送"
        );
        anyhow::ensure!(
            self.bridge
                .candidates()
                .iter()
                .any(|r| r.session == shown.reply.session
                    && r.caller == shown.reply.caller
                    && r.request_id == shown.reply.request_id
                    && r.revision == shown.reply.revision
                    && r.text == shown.reply.text),
            "回复已改变或已处理，未对其他回复执行动作"
        );
        Ok(&shown.reply)
    }
    pub(super) fn approve_review(&mut self) -> Result<()> {
        let reply = self.reviewed_reply(true)?;
        self.bridge
            .approve_candidates(std::slice::from_ref(reply))?;
        if let Some(shown) = &mut self.review_frame {
            shown.delivery = true;
            shown.reply.state = bridge::Execution::Accepted;
            shown.visible = false;
            shown.complete = false;
        }
        self.set_notice(
            "本条已批准；不授予自动权限，等待发送结果",
            NoticeLevel::Info,
        );
        Ok(())
    }
    pub(super) fn review_all_candidates(&mut self, approve: bool) -> Result<()> {
        let shown = self
            .review_frame
            .as_ref()
            .ok_or_else(|| anyhow!("请先打开待审列表"))?;
        anyhow::ensure!(
            shown.visible
                && shown.generation == self.assistant_accounts.generation()
                && shown.reply.session == self.bridge.session_id(),
            "场次或发送身份已改变，请重新查看待审列表"
        );
        let candidates = self.bridge.candidates();
        anyhow::ensure!(!candidates.is_empty(), "暂无待审回复");
        if approve {
            self.bridge.approve_candidates(&candidates)?;
            if let Some(shown) = self.review_frame.as_mut()
                && !shown.delivery
            {
                shown.delivery = true;
                shown.reply.state = bridge::Execution::Accepted;
                shown.visible = false;
                shown.complete = false;
            }
        } else {
            for reply in &candidates {
                self.bridge
                    .decide(&reply.caller, &reply.request_id, false)?;
            }
            if self
                .review_frame
                .as_ref()
                .is_some_and(|shown| !shown.delivery)
            {
                self.review_frame = None;
            }
        }
        self.set_notice(
            format!(
                "已{} {} 条待审回复；不改变自动发送授权",
                if approve { "批准发送" } else { "丢弃" },
                candidates.len()
            ),
            NoticeLevel::Info,
        );
        Ok(())
    }

    pub(super) fn discard_review(&mut self) -> Result<()> {
        let reply = self.reviewed_reply(false)?;
        self.bridge
            .decide(&reply.caller, &reply.request_id, false)?;
        self.review_frame = None;
        self.set_notice("回复已丢弃", NoticeLevel::Info);
        Ok(())
    }
    pub(super) fn edit_review(&mut self) -> Result<()> {
        self.reviewed_reply(false)?;
        self.begin_candidate_edit()?;
        self.review_frame = None;
        Ok(())
    }
    pub(super) fn review_key(&mut self, key: KeyEvent) -> bool {
        if key.code != KeyCode::Enter || key.modifiers != KeyModifiers::SHIFT {
            return false;
        }
        if key.kind != crossterm::event::KeyEventKind::Press {
            return true;
        }
        if self.secret_mode
            || self.stop_flow.is_some()
            || self.login_qr.is_some()
            || (self.assistant_panel.is_some() && self.candidate_edit.is_none())
        {
            return true;
        }
        if self.candidate_edit.is_some() {
            self.save_candidate_edit();
            if self.candidate_edit.is_none() && self.assistant_panel.is_none() {
                let _ = self.open_candidate_review();
            }
            return true;
        }
        if self
            .review_frame
            .as_ref()
            .is_some_and(|shown| shown.visible && shown.delivery)
        {
            let _ = self.open_candidate_review();
            return true;
        }
        let result = match self.reviewed_reply(false) {
            Ok(_) if self.review_frame.as_ref().is_some_and(|r| r.complete) => {
                self.approve_review()
            }
            Ok(_) => self.open_candidate_review(),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            self.set_notice(error.to_string(), NoticeLevel::Warning);
        }
        true
    }
    pub(super) fn draw_review_body(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Rect,
        scroll: &mut u16,
    ) {
        self.prepare_review(true);
        let detail = self.selected_candidate_text();
        let lines: Vec<String> = detail
            .split('\n')
            .flat_map(|line| wrapped_input_lines(line, usize::from(area.width)))
            .collect();
        let end_limit = lines.len().saturating_sub(usize::from(area.height));
        *scroll = (*scroll).min(end_limit.min(usize::from(u16::MAX)) as u16);
        let start = usize::from(*scroll);
        let end = (start + usize::from(area.height)).min(lines.len());
        if let Some(shown) = self.review_frame.as_mut() {
            if shown.width != area.width {
                shown.width = area.width;
                shown.seen_rows = 0;
            }
            if start <= shown.seen_rows {
                shown.seen_rows = shown.seen_rows.max(end);
            }
            shown.visible = area.width > 0 && area.height > 0;
            shown.complete = shown.visible
                && shown.seen_rows >= lines.len()
                && lines
                    .iter()
                    .all(|line| UnicodeWidthStr::width(line.as_str()) <= usize::from(area.width));
        }
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(start)
                    .take(usize::from(area.height))
                    .map(Line::raw)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(self.config.palette.content)),
            area,
        );
    }
    pub(super) fn sync_assistant_room(&mut self) {
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
    fn refresh_assistant_profile(&mut self, tx: &mpsc::Sender<UiEvent>) {
        if self.bridge.is_local()
            || !self.runner.settings.use_profile
            || self.profile.pending
            || (self.assistant_panel.is_none() && !self.runner.running)
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
        tokio::spawn(async move {
            let result = client
                .broadcaster_profile(&user_id)
                .await
                .map_err(|e| e.to_string());
            let _ = tx
                .send(UiEvent::BroadcasterProfile {
                    room_id,
                    user_id,
                    result,
                })
                .await;
        });
    }
    pub(super) fn profile_summary(&self) -> String {
        if !self.runner.settings.use_profile {
            return "已关闭引用".into();
        }
        if let Some(profile) = &self.profile.value {
            return format!("来源 {} · {}", profile.source_url, profile.description);
        }
        if self.profile.error.is_some() {
            return "暂不可用，不影响启动".into();
        }
        if self.profile.pending {
            return "获取中，不影响启动".into();
        }
        if self.profile.checked_at.is_some() {
            "主页未填写公开简介".into()
        } else {
            "尚未获取，不影响启动".into()
        }
    }
    pub(super) fn selected_candidate_text(&self) -> String {
        if let Some(shown) = &self.review_frame
            && shown.delivery
        {
            return format!(
                "发送账号：{}\n{} / {}\n原对象：{}\n{}\n{}",
                self.assistant_accounts.label(&self.account_status),
                shown.reply.caller,
                shown.reply.request_id,
                original_target(&shown.reply),
                delivery_label(shown.reply.state),
                assistant_segments(&shown.reply.text).join("\n")
            );
        }
        self.bridge
            .candidates()
            .into_iter()
            .find(|r| {
                self.candidate_selected
                    .as_ref()
                    .is_some_and(|(c, q)| c == &r.caller && q == &r.request_id)
            })
            .map(|r| {
                let target = r.reply_to.as_ref().map_or_else(
                    || "普通回复 · 不@（未启用或原消息无有效UID）".into(),
                    |target| format!("原生@ {} · UID {}", target.username, target.user_id),
                );
                format!(
                    "发送账号：{}\n{} / {}\n原对象：{}\n{}\n{}\n{}",
                    self.assistant_accounts.label(&self.account_status),
                    r.caller,
                    r.request_id,
                    original_target(&r),
                    target,
                    assistant_segments(&r.text).join("\n"),
                    r.diagnosis.as_ref().map_or_else(String::new, |d| format!(
                        "改写自 {} · {}",
                        r.retry_of.as_deref().unwrap_or("—"),
                        d.detail
                    ))
                )
            })
            .unwrap_or_else(|| "没有需要确认的回复；人工输入不受影响".into())
    }
    pub(super) fn begin_candidate_edit(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate_edit.is_none(),
            "正在编辑回复；Enter保存或Esc取消"
        );
        anyhow::ensure!(
            !self.secret_mode && self.stop_flow.is_none() && self.login_qr.is_none(),
            "请先结束当前弹窗操作"
        );
        self.select_candidate();
        let reply = self
            .bridge
            .candidates()
            .into_iter()
            .find(|r| {
                self.candidate_selected
                    .as_ref()
                    .is_some_and(|(c, q)| c == &r.caller && q == &r.request_id)
            })
            .ok_or_else(|| anyhow::anyhow!("没有需要确认的回复可编辑"))?;
        let draft = std::mem::replace(&mut self.input, EditorInput::from(reply.text));
        self.candidate_edit = Some(CandidateEdit {
            generation: self.assistant_accounts.generation(),
            session: reply.session,
            caller: reply.caller,
            request: reply.request_id,
            draft,
        });
        self.slash_selection = 0;
        self.set_notice(
            "回复编辑：Enter仅保存，Esc取消；保存后重新确认正文",
            NoticeLevel::Info,
        );
        Ok(())
    }
    pub(super) fn finish_candidate_edit(&mut self) {
        if let Some(edit) = self.candidate_edit.take() {
            self.input = edit.draft;
            self.candidate_selected = Some((edit.caller, edit.request));
            self.slash_selection = 0;
        }
    }
    pub(super) fn save_candidate_edit(&mut self) {
        let Some(edit) = &self.candidate_edit else {
            return;
        };
        if edit.generation != self.assistant_accounts.generation() {
            self.set_notice(
                "发送身份已改变；Esc退出后重新确认，编辑内容保留",
                NoticeLevel::Warning,
            );
            return;
        }
        match self
            .bridge
            .edit_candidate(&edit.session, &edit.caller, &edit.request, &self.input)
        {
            Ok(()) => {
                self.finish_candidate_edit();
                self.set_notice(
                    "回复已保存，尚未发送；/review 查看，人工草稿已恢复",
                    NoticeLevel::Info,
                );
            }
            Err(e) => self.set_notice(
                format!("保存失败：{e}；编辑内容保留，Esc取消"),
                NoticeLevel::Error,
            ),
        }
    }
    pub(super) fn advance_bridge(&mut self, tx: mpsc::Sender<UiEvent>) {
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
        if let Err(error) =
            self.runner
                .record_routing(&self.bridge, &self.journal, &self.session, false)
        {
            self.runner.pause(&self.bridge);
            self.set_notice(
                format!("消息分流归档失败，助手已暂停：{error}"),
                NoticeLevel::Error,
            );
        }
        if let Some(record) = self.runner.round_record.take() {
            match self.journal.reply_record(&self.session.id, &record) {
                Ok(()) if record["outcome"] == "rejected" => {
                    self.set_notice(self.runner.note.clone(), NoticeLevel::Warning);
                }
                Ok(()) => {}
                Err(error) => {
                    self.runner.pause(&self.bridge);
                    self.set_notice(
                        format!("轮次诊断归档失败，助手已暂停并撤权：{error}"),
                        NoticeLevel::Error,
                    );
                }
            }
        }
        if let Some(reason) = runner_stop {
            let mut message = format!("AI异常停止：{reason}；/diag 查看");
            if let Err(error) =
                self.journal
                    .assistant_stopped(&self.session, self.runner.native_id(), &reason)
            {
                message.push_str(&format!("；停止原因归档失败：{error}"));
            }
            self.set_notice(message, NoticeLevel::Error);
        }
        self.select_candidate();
        while let Some(job) = self.bridge.take_ready() {
            let segments = assistant_segments(&job.reply.text);
            self.enqueue(segments, Some(job), tx.clone());
        }
    }
    pub(super) fn enqueue(
        &mut self,
        segments: Vec<String>,
        job: Option<bridge::Job>,
        tx: mpsc::Sender<UiEvent>,
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
        let transport = TerminalTransport {
            account,
            client: self.client.clone(),
            room: self.config.room_id.clone(),
            tx: tx.clone(),
            job: job.as_ref().map(|j| j.reply.clone()),
            bridge: self.bridge.clone(),
            queue: queue.clone(),
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
        tokio::spawn(async move {
            let permit = job.as_ref().map(|job| &job.permit);
            let reply_to = job
                .as_ref()
                .and_then(|job| job.reply.reply_to.as_ref())
                .map(|target| target.user_id.as_str());
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
                    |(job, (session_id, sender_uid))| UiEvent::AssistantDelivery {
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
            if let Some(job) = job {
                bridge.complete(&job.reply, delivery);
            }
            let _ = tx
                .send(match notice {
                    Some(message) => UiEvent::DeliveryNotice(message),
                    None => UiEvent::DeliveryCompleted,
                })
                .await;
            if let Some(record) = record {
                let _ = tx.send(record).await;
            }
        });
        self.set_delivery_status(DeliveryStatus::Sending);
    }
    fn select_candidate(&mut self) {
        match self
            .bridge
            .selected_candidate(self.candidate_selected.as_ref())
        {
            Some(reply)
                if !self
                    .candidate_selected
                    .as_ref()
                    .is_some_and(|(caller, request)| {
                        caller == &reply.caller && request == &reply.request_id
                    }) =>
            {
                self.candidate_selected = Some((reply.caller, reply.request_id));
            }
            None => self.candidate_selected = None,
            _ => {}
        }
    }

    pub(super) fn set_automatic_permission(&mut self, enabled: bool) -> Result<()> {
        let mut settings = self.runner.settings.clone();
        settings.automatic = enabled;
        if !enabled {
            self.bridge.permission(false)?;
            let saved = self.runner.save(settings, &self.bridge, false);
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
        self.runner.save(settings, &self.bridge, false)?;
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

    fn reject_automatic_identity(&mut self, error: anyhow::Error) -> anyhow::Error {
        self.resume_pending = false;
        self.runner.pause(&self.bridge);
        let mut message = error.to_string();
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
        let _ = self.open_settings_section("account");
        self.set_notice(message.clone(), NoticeLevel::Error);
        anyhow::anyhow!(message)
    }

    pub(super) fn restore_automatic(
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

    pub(super) fn pause_assistant(&mut self) {
        self.resume_pending = false;
        self.runner.pause(&self.bridge);
        let remembered = self.runner.remember_enabled(false);
        let forgotten = self.assistant_accounts.forget_automatic();
        if let Err(error) = remembered.and(forgotten) {
            self.set_notice(
                format!("助手已暂停；未能清除下次续用提醒：{error}"),
                NoticeLevel::Error,
            );
        }
    }

    pub(super) fn next_candidate(&mut self) {
        if let Some(shown) = &self.review_frame
            && shown.delivery
        {
            if matches!(
                shown.reply.state,
                bridge::Execution::Accepted | bridge::Execution::Sending
            ) {
                self.set_notice("本条仍在发送，请等待结果，不重发", NoticeLevel::Info);
                return;
            }
            self.review_frame = None;
            self.select_candidate();
            return;
        }
        let candidates = self.bridge.candidates();
        if !candidates.is_empty() {
            let index = self
                .candidate_selected
                .as_ref()
                .and_then(|(c, r)| {
                    candidates
                        .iter()
                        .position(|v| &v.caller == c && &v.request_id == r)
                })
                .map_or(0, |i| (i + 1) % candidates.len());
            let reply = &candidates[index];
            self.candidate_selected = Some((reply.caller.clone(), reply.request_id.clone()));
            self.review_frame = None;
        }
    }
}
pub(super) fn review_height(app: &TerminalApp) -> u16 {
    u16::from(
        app.review_frame.as_ref().is_some_and(|r| r.delivery) || app.bridge.candidate_count() != 0,
    )
}

pub(super) fn draw_review(frame: &mut ratatui::Frame, area: Rect, app: &mut TerminalApp) {
    if area.width == 0 || area.height == 0 || review_height(app) == 0 {
        return;
    }
    if app.assistant_panel.is_some() || app.command_search_draft.is_some() {
        return;
    }
    app.prepare_review(false);
    let Some(review) = app.review_frame.as_ref() else {
        return;
    };
    let segments = assistant_segments(&review.reply.text);
    let sender = match app.assistant_accounts.status(&app.account_status) {
        AccountStatus::SignedIn { display_name, .. } => display_name.as_str(),
        AccountStatus::SignedOut => "未登录",
    };
    let prefix = if review.delivery {
        format!(
            "{} · 待审 {} · {} → {} · ",
            delivery_label(review.reply.state),
            app.bridge.candidate_count(),
            sender,
            original_target(&review.reply)
        )
    } else {
        format!(
            "待审 {} · {} → {} · ",
            app.bridge.candidate_count(),
            sender,
            original_target(&review.reply)
        )
    };
    let body = segments.join(" / ");
    let send_hint = "  ⇧↵发送";
    let complete = !review.delivery
        && segments.len() == 1
        && !review.reply.text.contains(['\n', '\r'])
        && UnicodeWidthStr::width(prefix.as_str())
            + UnicodeWidthStr::width(body.as_str())
            + UnicodeWidthStr::width(send_hint)
            <= usize::from(area.width);
    let hint = if app.candidate_edit.is_some() {
        "  正在编辑 · 保存不发送"
    } else if review.delivery {
        "  ⇧↵查看结果"
    } else if complete {
        send_hint
    } else {
        "  ⇧↵展开"
    };
    let available = usize::from(area.width).saturating_sub(UnicodeWidthStr::width(hint));
    let text = fit_display_width(&format!("{prefix}{body}"), available);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(text, Style::default().fg(app.config.palette.time)),
            Span::styled(hint, Style::default().fg(app.config.palette.info)),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let visible = app.assistant_panel.is_none()
        && app.candidate_edit.is_none()
        && !app.secret_mode
        && app.stop_flow.is_none()
        && app.login_qr.is_none();
    if let Some(review) = app.review_frame.as_mut() {
        review.visible = visible;
        review.complete = complete;
    }
}

fn delivery_label(state: bridge::Execution) -> &'static str {
    match state {
        bridge::Execution::Accepted | bridge::Execution::Sending => "发送中 · 待回显",
        bridge::Execution::Uncertain => "未确认送达，不自动重发",
        bridge::Execution::Rejected => "发送被拒绝",
        bridge::Execution::Cancelled => "发送已取消",
        bridge::Execution::Confirmed => "已确认送达",
        bridge::Execution::AwaitingApproval => "需要确认的回复",
    }
}

fn original_target(reply: &bridge::Reply) -> String {
    let name = reply
        .original_message()
        .and_then(|m| m.username.as_deref())
        .unwrap_or("原消息");
    match &reply.reply_to {
        Some(target) => format!("@{}", target.username),
        None => format!("{name}（不@）"),
    }
}
/// Fresh, explicitly selected private root. This path never loads production configuration or clients.
pub async fn run_local(root: &Path, assistant: bool) -> Result<()> {
    let shutdown = shutdown_signal()?;
    tokio::pin!(shutdown);
    bridge::wire::private_root(root)?;
    anyhow::ensure!(
        std::fs::read_dir(root)?.next().is_none(),
        "本地验证必须使用空的私有目录，不读取既有账号或配置"
    );
    anyhow::ensure!(
        !root.join("local-started").exists(),
        "本地验证目录已使用；请指定新的私有目录，不覆盖证据"
    );
    let mut app = replay::app(root, DanmuSession::new("local"));
    app.config.instance = Some(root.to_path_buf());
    if assistant {
        app.open_assistant()?;
    }
    app.connection = "本地隔离 · 无房间 /event 姓名 正文".into();
    let (tx, mut rx) = mpsc::channel(128);
    let transport = Arc::new(LocalTransport {
        outcome: AtomicU8::new(0),
        log: root.join("local-deliveries.jsonl"),
        tx: tx.clone(),
    });
    app.local_transport = Some(transport.clone());
    let server = bridge::wire::serve(root, app.bridge.clone()).await?;
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("local-started"))?;
    let mut terminal = TerminalGuard::enter()?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    app.set_notice(
        "LOCAL · /event NAME TEXT · /local confirmed|uncertain|rejected · /session new|end · /quit",
        NoticeLevel::Info,
    );
    let loop_result: Result<()> = async {
    loop {
        app.advance_bridge(tx.clone());
        terminal.draw_app(&mut app)?;
        tokio::select! {
            _=&mut shutdown=>break,
            _=tick.tick()=> {app.animation_tick=app.animation_tick.wrapping_add(1);app.expire_notice_at(Instant::now());app.advance_history_at(Instant::now());}
            event=rx.recv()=>if let Some(event)=event {app.handle_ui_event(event);},
            event=events.next()=>{
                let Some(event)=event else {break;};
                let event=event?;
                match event {
                    Event::Key(key)=>{
                        if key.kind==crossterm::event::KeyEventKind::Press && key.code==KeyCode::Enter && key.modifiers.is_empty() && !app.selection_active && app.command_search_draft.is_none() && app.candidate_edit.is_none() && app.assistant_panel.is_none() && app.help.is_none() && app.stop_flow.is_none() && !app.secret_mode && app.login_qr.is_none() {
                            let text=app.input.take();
                            if let Some(rest)=text.strip_prefix("/event ") {
                                if let Some((name,text))=rest.split_once(' ') {
                                    let mut event=DanmuEvent::new(DanmuEventKind::Danmu,sanitize_input(text.into()));event.username=Some(sanitize_input(name.into()));event.author_id=Some(name.into());app.ingest_event(event);
                                } else {app.set_notice("/event 姓名 正文",NoticeLevel::Warning);}
                            } else if let Some(mode)=text.strip_prefix("/local ") {
                                match mode {"confirmed"=>transport.outcome.store(0,Ordering::Relaxed),"uncertain"=>transport.outcome.store(1,Ordering::Relaxed),"rejected"=>transport.outcome.store(2,Ordering::Relaxed),_=>app.set_notice("/local confirmed|uncertain|rejected",NoticeLevel::Warning)}
                            } else if text=="/session new" {
                                if app.session.ended_at.is_none() {
                                    app.session.end(Utc::now(), DanmuSessionEndReason::Completed);
                                    app.journal.end(&app.session)?;
                                }
                                app.bridge.new_session();
                                app.session=DanmuSession::new("local");
                                app.journal.start(&app.session)?;
                            } else if text=="/session end" {
                                app.bridge.end();
                                if app.session.ended_at.is_none() {
                                    app.session.end(Utc::now(), DanmuSessionEndReason::Completed);
                                    app.journal.end(&app.session)?;
                                }
                            }
                            else {
                                app.input.replace(text);
                                match app.handle_key(key,tx.clone()).await {Ok(true)=>break,Ok(false)=>{},Err(error)=>app.set_notice(error.to_string(),NoticeLevel::Error)}
                            }
                            if app.quit_requested {break;}
                        } else {match app.handle_key(key,tx.clone()).await {Ok(true)=>break,Ok(false)=>{},Err(error)=>app.set_notice(error.to_string(),NoticeLevel::Error)}}
                    }
                    Event::Paste(text)=>app.handle_paste(&text),
                    Event::Mouse(mouse) => app.handle_mouse(mouse),
                    _=>{}
                }
            }
        }
    }
    Ok(())
    }.await;
    let routing_log = app
        .runner
        .record_routing(&app.bridge, &app.journal, &app.session, true);
    app.runner.shutdown(&app.bridge).await;
    app.bridge.end();
    loop_result?;
    routing_log?;
    if app.session.ended_at.is_none() {
        app.session
            .end(Utc::now(), DanmuSessionEndReason::Completed);
        app.journal.end(&app.session)?;
    }
    drop(server);
    drop(terminal);
    std::fs::write(
        root.join("local-stopped.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"pid":std::process::id(),"endpoint_removed":!root.join("instance.json").exists(),"production_access":false}),
        )?,
    )?;
    Ok(())
}
