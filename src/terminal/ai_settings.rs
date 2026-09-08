use super::*;
use crate::autoreply::{Provider, codex};
use tokio::sync::watch;

#[derive(Debug)]
pub(super) enum Event {
    Url {
        epoch: u64,
        url: String,
    },
    Finished {
        epoch: u64,
        result: std::result::Result<(codex::Account, Vec<codex::Model>), String>,
    },
}
#[derive(Default)]
pub(super) struct Settings {
    pub open: bool,
    pub status: String,
    pub account: codex::Account,
    pub models: Vec<codex::Model>,
    pub checked: bool,
    pub scroll: u16,
    busy: bool,
    epoch: u64,
    cancel: Option<watch::Sender<bool>>,
    url: Option<String>,
}
impl Drop for Settings {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            let _ = cancel.send(true);
        }
    }
}
impl Settings {
    pub fn ready(&self, config: &autoreply::Config) -> bool {
        match config.provider {
            Provider::Chatgpt => {
                self.account.signed_in
                    && self.models.iter().any(|model| {
                        Some(&model.model) == config.codex.model.as_ref()
                            && model
                                .validate_effort(config.codex.effort.as_deref().unwrap_or(""))
                                .is_ok()
                    })
            }
            Provider::Api => config.model.as_ref().is_some_and(|model| {
                !model.id.trim().is_empty()
                    && autoreply::provider::validate_url(&model.endpoint.url).is_ok()
                    && model.endpoint.key_env.starts_with("DANMU_")
                    && std::env::var(&model.endpoint.key_env).is_ok_and(|v| !v.is_empty())
            }),
        }
    }
    pub fn start(&mut self, action: &str, config: codex::Config, tx: mpsc::Sender<UiEvent>) {
        if self.busy {
            self.status = "连接操作进行中；/ai cancel-login 取消后再操作".into();
            return;
        }
        self.busy = true;
        self.checked = true;
        self.epoch += 1;
        self.url = None;
        self.status = format!("订阅连接：{action}…；不会发起推理");
        let epoch = self.epoch;
        let action = action.to_owned();
        let (cancel, cancelled) = watch::channel(false);
        self.cancel = Some(cancel);
        tokio::spawn(async move {
            let result: Result<(codex::Account, Vec<codex::Model>)> = async {
                let mut session = codex::Session::open(&config).await?;
                match action.as_str() {
                    "login" => {
                        let login = session.login_start().await?;
                        tx.send(UiEvent::AiSettings(Event::Url {
                            epoch,
                            url: login.url.clone(),
                        }))
                        .await?;
                        // Opening this verified official URL is an explicit TUI user action, never a model tool.
                        #[cfg(target_os = "macos")]
                        {
                            let _ = tokio::process::Command::new("/usr/bin/open")
                                .arg(&login.url)
                                .status()
                                .await;
                        }
                        #[cfg(target_os = "linux")]
                        {
                            let _ = tokio::process::Command::new("xdg-open")
                                .arg(&login.url)
                                .status()
                                .await;
                        }
                        let account = login.finish(cancelled).await?;
                        let mut session = codex::Session::open(&config).await?;
                        Ok((account, session.models().await?))
                    }
                    "logout" => {
                        session.logout().await?;
                        Ok((codex::Account::default(), vec![]))
                    }
                    _ => {
                        let account = session.account().await?;
                        let models = if account.signed_in {
                            session.models().await?
                        } else {
                            vec![]
                        };
                        Ok((account, models))
                    }
                }
            }
            .await;
            let _ = tx
                .send(UiEvent::AiSettings(Event::Finished {
                    epoch,
                    result: result.map_err(|e| e.to_string()),
                }))
                .await;
        });
    }
    pub fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::Url { epoch, url } if epoch == self.epoch => {
                self.url = Some(url);
                self.status = "等待浏览器完成官方登录；/ai cancel-login 取消（180秒）".into();
            }
            Event::Finished { epoch, result } if epoch == self.epoch => {
                self.busy = false;
                self.cancel = None;
                self.url = None;
                match result {
                    Ok((account, models)) => {
                        self.account = account;
                        self.models = models;
                        self.status = if self.account.signed_in {
                            "ChatGPT账号已确认；模型列表已刷新，真实推理尚需人工验证"
                        } else {
                            "未登录ChatGPT订阅；/ai login 发起应用独立登录"
                        }
                        .into();
                    }
                    Err(error) => {
                        self.account = codex::Account::default();
                        self.models.clear();
                        self.status = error;
                    }
                }
                return !self.account.signed_in;
            }
            _ => {}
        }
        false
    }
}
impl TerminalApp {
    pub(super) async fn persist_ai_safety_stop(&mut self) {
        let stopped = self.autoreply.as_ref().is_some_and(|reply| {
            let view = reply.view.borrow();
            reply.faulted || (view.generation == reply.generation && view.mode == ReplyMode::Paused)
        });
        if stopped && self.config.autoreply.mode != ReplyMode::Paused {
            self.config.autoreply.mode = ReplyMode::Paused;
            let config = self.config.clone();
            let result =
                tokio::task::spawn_blocking(move || config.save_autoreply(&config.autoreply)).await;
            if !matches!(result, Ok(Ok(()))) {
                self.set_notice(
                    "安全暂停未能保存；保持暂停，请检查配置目录写权限",
                    NoticeLevel::Error,
                );
            }
        }
    }
    async fn apply_ai_config(&mut self, config: autoreply::Config) -> Result<()> {
        // Invalidate immediately, before waiting on durable settings. A failed save stays safe.
        if let Some(reply) = self.autoreply.as_mut() {
            reply.discard();
        }
        let current = self.config.clone();
        let saved = config.clone();
        tokio::task::spawn_blocking(move || current.save_autoreply(&saved)).await??;
        self.config.autoreply = config.clone();
        if let Some(reply) = self.autoreply.as_mut() {
            reply.configure(config);
        }
        self.ai_settings.status = "设置已保存；旧候选已取消，新事件使用新配置".into();
        Ok(())
    }
    pub(super) async fn ai_command(
        &mut self,
        args: &[&str],
        tx: mpsc::Sender<UiEvent>,
    ) -> Result<()> {
        let mut config = self.config.autoreply.clone();
        match args {
            [] => {
                self.reply_panel = !self.reply_panel;
                return Ok(());
            }
            ["settings"] => {
                self.ai_settings.open = !self.ai_settings.open;
                return Ok(());
            }
            [action @ ("login" | "logout" | "models" | "status")] => {
                if *action == "logout" {
                    config.enabled = false;
                    self.apply_ai_config(config.clone()).await?;
                }
                self.ai_settings.open = true;
                self.ai_settings.start(action, config.codex, tx);
                return Ok(());
            }
            ["cancel-login"] => {
                if let Some(cancel) = &self.ai_settings.cancel {
                    let _ = cancel.send(true);
                }
                return Ok(());
            }
            ["provider", provider] => {
                config.provider = match *provider {
                    "chatgpt" => Provider::Chatgpt,
                    "api" => Provider::Api,
                    _ => anyhow::bail!("provider须为chatgpt或api"),
                };
                config.enabled = false; // Switching connections requires explicit re-enable.
                self.ai_settings.checked = false;
            }
            ["model", model] => {
                anyhow::ensure!(
                    autoreply::safe_text(model) && model.len() <= 200,
                    "模型ID无效"
                );
                match config.provider {
                    Provider::Chatgpt => {
                        let selected = self
                            .ai_settings
                            .models
                            .iter()
                            .find(|m| m.model == *model)
                            .ok_or_else(|| {
                            anyhow::anyhow!("模型不在账号可用列表中；先/ai models")
                        })?;
                        if config
                            .codex
                            .effort
                            .as_deref()
                            .is_none_or(|effort| selected.validate_effort(effort).is_err())
                        {
                            selected.validate_effort(&selected.default_reasoning_effort)?;
                            config.codex.effort = Some(selected.default_reasoning_effort.clone());
                        }
                        config.codex.model = Some(model.to_string());
                    }
                    Provider::Api => {
                        config
                            .model
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("请先配置API endpoint与DANMU_凭据变量"))?
                            .id = model.to_string()
                    }
                }
            }
            ["effort", effort] => {
                anyhow::ensure!(
                    config.provider == Provider::Chatgpt,
                    "此API适配器未声明思考强度支持，不假装应用设置"
                );
                let selected = self
                    .ai_settings
                    .models
                    .iter()
                    .find(|m| Some(&m.model) == config.codex.model.as_ref())
                    .ok_or_else(|| anyhow::anyhow!("先刷新账号模型并选择模型"))?;
                selected.validate_effort(effort)?;
                config.codex.effort = Some(effort.to_string());
            }
            ["enable"] => {
                match config.provider {
                    Provider::Chatgpt => {
                        anyhow::ensure!(
                            self.ai_settings.account.signed_in,
                            "先/ai login或/ai status确认应用账号"
                        );
                        let selected = self
                            .ai_settings
                            .models
                            .iter()
                            .find(|m| Some(&m.model) == config.codex.model.as_ref())
                            .ok_or_else(|| anyhow::anyhow!("先选择账号可用模型"))?;
                        selected.validate_effort(config.codex.effort.as_deref().unwrap_or(""))?;
                    }
                    Provider::Api => {
                        let model = config
                            .model
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("API模型尚未配置"))?;
                        autoreply::provider::validate_url(&model.endpoint.url)?;
                        anyhow::ensure!(
                            model.endpoint.key_env.starts_with("DANMU_")
                                && std::env::var(&model.endpoint.key_env)
                                    .is_ok_and(|v| !v.is_empty()),
                            "缺少专用API凭据环境变量"
                        );
                    }
                }
                config.enabled = true;
            }
            ["disable"] => config.enabled = false,
            ["suggest"] => config.mode = ReplyMode::Suggest,
            ["approve-mode"] => config.mode = ReplyMode::Approve,
            ["auto"] => config.mode = ReplyMode::Auto,
            ["pause"] => {
                config.mode = ReplyMode::Paused;
                self.send_queue.pause();
            }
            ["resume-send"] => {
                self.send_queue.resume();
                config.mode = ReplyMode::Suggest;
            }
            ["approve"] => {
                if let Some(reply) = self.autoreply.as_mut() {
                    reply.approve();
                }
                return Ok(());
            }
            ["discard"] => {
                if let Some(reply) = self.autoreply.as_mut() {
                    reply.discard();
                }
                return Ok(());
            }
            _ => anyhow::bail!(
                "/ai settings；login/status/models；provider chatgpt|api；model ID；effort VALUE；enable|disable；suggest|approve-mode|auto"
            ),
        }
        self.apply_ai_config(config).await
    }
}
pub(super) fn draw(frame: &mut ratatui::Frame, area: Rect, app: &TerminalApp) {
    if !app.ai_settings.open {
        return;
    }
    let settings = &app.ai_settings;
    let config = &app.config.autoreply;
    let mut lines = vec![
        format!(
            "总开关 {} · 模式 {}（独立设置）",
            if config.enabled { "开启" } else { "关闭" },
            config.mode.label()
        ),
        config.model_label(),
        "/ai settings或Esc关闭 · PageUp/PageDown滚动".into(),
        settings.status.clone(),
        format!(
            "账号 {} · 套餐 {} · {}",
            if settings.account.signed_in {
                "已确认"
            } else {
                "未确认"
            },
            settings.account.plan.as_deref().unwrap_or("未报告"),
            settings.account.quota
        ),
        "/ai login | cancel-login | logout | status | models".into(),
        "/ai provider chatgpt|api · /ai model <ID> · /ai effort <值>".into(),
        "/ai enable|disable · suggest|approve-mode|auto · pause".into(),
        "账号可用模型（先登录/刷新；列表并非真实推理已验证）：".into(),
    ];
    for model in &settings.models {
        let efforts = model
            .supported_reasoning_efforts
            .iter()
            .map(|v| v.reasoning_effort.as_str())
            .collect::<Vec<_>>()
            .join("/");
        lines.push(format!(
            "{} · {} · 强度 {} 默认{}",
            model.model, model.display_name, efforts, model.default_reasoning_effort
        ));
    }
    if let Some(url) = &settings.url {
        lines.push(format!("官方授权URL（仅本面板显示）：{url}"));
    }
    lines.push("/ai settings 关闭设置；API配置仍可用，订阅不自动回退API".into());
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .scroll((settings.scroll, 0))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("AI连接与回复设置"),
            ),
        area,
    );
}

#[cfg(test)]
mod tests;
