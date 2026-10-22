#[cfg(test)]
use crate::runtime::assistant::LocalTransport;
use crate::runtime::assistant::assistant_segments;
#[cfg(test)]
use std::sync::atomic::AtomicU8;
#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;

use super::*;
use std::{path::Path, sync::Arc};

pub(crate) struct ReviewFrame {
    pub(crate) reply: bridge::Reply,
    pub(crate) generation: u64,
    pub(crate) visible: bool,
    pub(crate) complete: bool,
    pub(crate) delivery: bool,
    pub(crate) expanded: bool,
    pub(crate) width: u16,
    pub(crate) seen_rows: usize,
}

pub(crate) struct CandidateEdit {
    pub(crate) generation: u64,
    pub(crate) session: String,
    pub(crate) caller: String,
    pub(crate) request: String,
    pub(crate) draft: EditorInput,
}

impl RuntimeState {
    pub(crate) fn clear_review_surface(&mut self) {
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
    pub(crate) fn prepare_review(&mut self, expanded: bool) {
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
    pub(crate) fn reviewed_reply(&self, require_complete: bool) -> Result<&bridge::Reply> {
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
    pub(crate) fn approve_review(&mut self) -> Result<()> {
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
    pub(crate) fn review_all_candidates(&mut self, approve: bool) -> Result<()> {
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

    pub(crate) fn discard_review(&mut self) -> Result<()> {
        let reply = self.reviewed_reply(false)?;
        self.bridge
            .decide(&reply.caller, &reply.request_id, false)?;
        self.review_frame = None;
        self.set_notice("回复已丢弃", NoticeLevel::Info);
        Ok(())
    }
    pub(crate) fn edit_review(&mut self) -> Result<()> {
        self.reviewed_reply(false)?;
        self.begin_candidate_edit()?;
        self.review_frame = None;
        Ok(())
    }
    pub(crate) fn review_key(&mut self, key: KeyEvent) -> bool {
        if key.code != KeyCode::Enter || key.modifiers != KeyModifiers::SHIFT {
            return false;
        }
        if key.kind != crossterm::event::KeyEventKind::Press {
            return true;
        }
        if self.secret_target.is_some()
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
    pub(crate) fn draw_review_body(
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
    pub(crate) fn selected_candidate_text(&self) -> String {
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
    pub(crate) fn begin_candidate_edit(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate_edit.is_none(),
            "正在编辑回复；Enter保存或Esc取消"
        );
        anyhow::ensure!(
            self.secret_target.is_none() && self.stop_flow.is_none() && self.login_qr.is_none(),
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
    pub(crate) fn finish_candidate_edit(&mut self) {
        if let Some(edit) = self.candidate_edit.take() {
            self.input = edit.draft;
            self.candidate_selected = Some((edit.caller, edit.request));
            self.slash_selection = 0;
        }
    }
    pub(crate) fn save_candidate_edit(&mut self) {
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
    pub(crate) fn select_candidate(&mut self) {
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

    pub(crate) fn next_candidate(&mut self) {
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
pub(crate) fn review_height(app: &RuntimeState) -> u16 {
    u16::from(
        app.review_frame.as_ref().is_some_and(|r| r.delivery) || app.bridge.candidate_count() != 0,
    )
}

pub(crate) fn draw_review(frame: &mut ratatui::Frame, area: Rect, app: &mut RuntimeState) {
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
        && app.secret_target.is_none()
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
/// Fresh local-only host: never discovers credentials or production paths.
pub async fn run_local(root: &Path, assistant: bool) -> Result<()> {
    bridge::wire::private_root(root)?;
    anyhow::ensure!(
        std::fs::read_dir(root)?.next().is_none(),
        "本地验证必须使用空的私有目录，不读取既有账号或配置"
    );
    let paths = replay::paths(root);
    let handle = crate::runtime::open(crate::runtime::RuntimeOptions {
        obs_passwords: Arc::new(crate::obs::FileObsPasswordStore::new(
            paths.obs_configuration.clone(),
            false,
        )),
        storage: paths,
        workspace_default: root.join("workspace"),
        mode: crate::runtime::RuntimeMode::Local,
        external_instance: Some(root.to_path_buf()),
    })
    .await?;
    let result: Result<()> = async {
        std::fs::OpenOptions::new().create_new(true).write(true).open(root.join("local-started"))?;
        handle.presentation(move |state| Box::pin(async move {
            if assistant { state.open_assistant()?; }
            state.connection = "本地隔离 · 无房间 /event 姓名 正文".into();
            state.set_notice("LOCAL · /event NAME TEXT · /local confirmed|uncertain|rejected · /session new|end · /quit", NoticeLevel::Info);
            Ok::<_, anyhow::Error>(())
        })).await??;
        drive(handle.clone(), true).await
    }.await;
    let stopped = handle.shutdown().await;
    let evidence = crate::storage::write_private_atomic(
        &root.join("local-stopped.json"),
        &serde_json::to_vec_pretty(
            &serde_json::json!({"pid":std::process::id(),"endpoint_removed":!root.join("instance.json").exists(),"production_access":false}),
        )?,
    );
    result.and(stopped).and(evidence)
}
