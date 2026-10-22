use super::settings::ExternalField;
use super::*;
use crate::runner::{
    KeyStatus,
    responses::ModelChoice,
    settings::{ApiProtocol, Persona, ReplyActivity, ReplyPreset},
    subscription::AccountInfo,
};
use crate::runtime::diagnostics::{account_label, diagnostics_details};
use anyhow::Context;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum Page {
    #[default]
    Home,
    Settings,
    Account,
    Obs,
    AiSettings,
    System,
    About,
    Models,
    Assistant,
    Materials,
    Advanced,
    ReplyRange,
    Themes,
    Presets,
    More,
    Candidates,
    Diagnostics,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Home => "AI",
            Self::Settings => "设置与操作",
            Self::Account => "B站账号与直播间",
            Self::Obs => "OBS",
            Self::AiSettings => "AI助手",
            Self::System => "系统与关于",
            Self::About => "关于弹幕台",
            Self::Models => "模型",
            Self::Assistant => "发送策略",
            Self::Materials => "人设与资料",
            Self::Advanced => "维护",
            Self::ReplyRange => "本场范围",
            Self::Themes => "主题",
            Self::Presets => "回复方案",
            Self::More => "更多",
            Self::Candidates => "待审回复",
            Self::Diagnostics => "诊断",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    External(ExternalField),
    Name,
    Topic,
    BlockedWords,
    ArchiveSearch,
    ApiBaseUrl,
    ApiModel,
}
impl Field {
    fn label(self) -> &'static str {
        match self {
            Self::External(field) => field.label(),
            Self::Name => "AI 名字",
            Self::Topic => "本场主题",
            Self::BlockedWords => "屏蔽词",
            Self::ArchiveSearch => "搜索归档",
            Self::ApiBaseUrl => "API 基址",
            Self::ApiModel => "模型名",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Run,
    Review,
    EditCandidate,
    ApproveCandidate,
    DiscardCandidate,
    NextCandidate,
    ApproveAllCandidates,
    DiscardAllCandidates,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Preset(ReplyPreset),
    Group(&'static str),
    Command(&'static str),
    Action(Action),
    Page(Page),
    ApiProtocol,
    ApiKey,
    ClearApiKey,
    ChatgptAccount,
    ChatgptLogin,
    ChatgptRelogin,
    ChatgptSignOut,
    ChatgptModels,
    ChatgptModel,
    Edit(Field),
    Permission,
    Activity(ReplyActivity),
    RestoreTopic,
    Persona,
    RepairHistory,
    WebSearch,
    Theme(usize),
    ReloadThemes,
    Login,
    Logout,
    ReuseMain,
    AssistantLogin,
    CancelAssistantLogin,
    SignOutIndependent,
    Visible,
    Stop,
}
#[derive(Clone, Copy)]
struct HistoryEntry {
    page: Page,
    row: usize,
    scroll: u16,
    list_offset: usize,
    expanded: bool,
}
pub(crate) struct Panel {
    page: Page,
    row: usize,
    editor: Option<(Field, EditorInput)>,
    choices: Option<Choices>,
    pub(crate) accounts: Vec<AccountInfo>,
    pub(crate) models: Vec<ModelChoice>,
    error: Option<String>,
    scroll: u16,
    history: Vec<HistoryEntry>,
    list_offset: usize,
    menu_height: usize,
    menu_hit: Option<(Page, Rect, usize)>,
    menu_rows: Vec<Row>,
    expanded: bool,
    pending_save: Option<uuid::Uuid>,
    obs_configuration: Option<crate::obs::ObsConfiguration>,
}
struct Choices {
    kind: ChoiceKind,
    values: Vec<String>,
    selected: usize,
}
#[derive(Clone, Copy)]
enum ChoiceKind {
    Sending,
    ApiProtocol,
    ClearApiKey,
    ChatgptAccount,
    ChatgptModel,
}
const API_PROTOCOLS: [ApiProtocol; 3] = [
    ApiProtocol::OpenaiChat,
    ApiProtocol::AnthropicMessages,
    ApiProtocol::ChatgptSubscription,
];
const SETTINGS_PAGES: [Page; 4] = [Page::Account, Page::Obs, Page::AiSettings, Page::System];

impl Panel {
    pub(crate) fn clear_chatgpt_models(&mut self) {
        self.models.clear();
        if self
            .choices
            .as_ref()
            .is_some_and(|choices| matches!(choices.kind, ChoiceKind::ChatgptModel))
        {
            self.choices = None;
        }
    }
    fn choose_chatgpt_model(&mut self, selected_model: &str) -> Result<()> {
        anyhow::ensure!(!self.models.is_empty(), "请重新读取此账号的可用模型");
        self.choices = Some(Choices {
            kind: ChoiceKind::ChatgptModel,
            values: self
                .models
                .iter()
                .map(|model| format!("{} · {}", model.display_name, model.slug))
                .collect(),
            selected: self
                .models
                .iter()
                .position(|model| model.slug == selected_model)
                .unwrap_or(0),
        });
        self.list_offset = 0;
        self.scroll = 0;
        Ok(())
    }
    fn new() -> Self {
        Self {
            page: Page::Home,
            row: 0,
            editor: None,
            choices: None,
            accounts: Vec::new(),
            models: Vec::new(),
            error: None,
            scroll: 0,
            history: Vec::new(),
            list_offset: 0,
            menu_height: 5,
            menu_hit: None,
            menu_rows: Vec::new(),
            expanded: false,
            pending_save: None,
            obs_configuration: None,
        }
    }
    fn navigate(&mut self, page: Page) {
        self.history.push(HistoryEntry {
            page: self.page,
            row: self.row,
            scroll: self.scroll,
            list_offset: self.list_offset,
            expanded: self.expanded,
        });
        self.page = page;
        self.menu_hit = None;
        self.row = 0;
        self.list_offset = 0;
        self.scroll = 0;
        self.expanded = matches!(page, Page::Diagnostics | Page::About);
        self.error = None;
    }
    fn back(&mut self) -> bool {
        let Some(HistoryEntry {
            page,
            row,
            scroll,
            list_offset: offset,
            expanded,
        }) = self.history.pop()
        else {
            return true;
        };
        self.page = page;
        self.menu_hit = None;
        self.row = row;
        self.scroll = scroll;
        self.list_offset = offset;
        self.expanded = expanded;
        self.error = None;
        false
    }
    fn is_settings(&self) -> bool {
        self.page == Page::Settings
            || self
                .history
                .iter()
                .any(|entry| entry.page == Page::Settings)
    }
    pub(crate) fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    fn rows(&self, app: &RuntimeState) -> Vec<Row> {
        match self.page {
            Page::Home => vec![
                Row::Action(Action::Run),
                Row::Permission,
                Row::Page(Page::ReplyRange),
                Row::Edit(Field::Topic),
                Row::Action(Action::Review),
                Row::Page(Page::Settings),
            ],
            Page::Settings => vec![
                Row::Group("设置"),
                Row::Page(Page::Account),
                Row::Page(Page::Obs),
                Row::Page(Page::AiSettings),
                Row::Page(Page::System),
                Row::Group("当前操作"),
                Row::Page(Page::Home),
                Row::Command("/pin"),
                Row::Edit(Field::ArchiveSearch),
                Row::Command("/commands"),
                Row::Command("/quit"),
            ],
            Page::AiSettings => vec![
                Row::Page(Page::Models),
                Row::Page(Page::Assistant),
                Row::Page(Page::Materials),
                Row::Page(Page::Advanced),
                Row::Group("本场操作"),
                Row::Page(Page::Home),
                Row::Page(Page::Account),
            ],
            Page::Obs => vec![
                Row::Group("连接配置"),
                Row::Edit(Field::External(ExternalField::ObsHost)),
                Row::Edit(Field::External(ExternalField::ObsPort)),
                Row::Command("/obs config password"),
                Row::Edit(Field::External(ExternalField::ObsMicrophone)),
                Row::Command("/obs connect"),
                Row::Group("当前控制"),
                Row::Edit(Field::External(ExternalField::ObsScene)),
                Row::Command("/mute"),
                Row::Command("/unmute"),
                Row::Command("/obs start"),
                Row::Command("/obs stop"),
            ],
            Page::System => vec![
                Row::Page(Page::About),
                Row::Page(Page::Diagnostics),
                Row::RepairHistory,
                Row::Command("/help"),
            ],
            Page::About => Vec::new(),
            Page::Account => {
                let mut rows = vec![
                    Row::Group("直播间"),
                    Row::Edit(Field::External(ExternalField::RoomTitle)),
                    Row::Edit(Field::External(ExternalField::RoomCover)),
                    Row::Group("人工发送"),
                ];
                if matches!(app.account_status, AccountStatus::SignedIn { .. }) {
                    rows.push(Row::Logout)
                } else {
                    rows.push(Row::Login)
                }
                rows.push(Row::Group("AI 发送"));
                if matches!(app.account_status, AccountStatus::SignedIn { .. }) {
                    rows.push(Row::ReuseMain);
                }
                rows.push(Row::AssistantLogin);
                if app.assistant_accounts.login_pending() {
                    rows.push(Row::CancelAssistantLogin)
                }
                if app
                    .assistant_accounts
                    .independent_user_id(&app.account_status)
                    .is_some()
                {
                    rows.push(Row::SignOutIndependent)
                }
                rows
            }
            Page::Models => {
                let mut rows = vec![Row::ApiProtocol];
                if app.runner.settings.api.protocol == ApiProtocol::ChatgptSubscription {
                    rows.extend([
                        Row::ChatgptAccount,
                        Row::ChatgptLogin,
                        Row::ChatgptRelogin,
                        Row::ChatgptSignOut,
                        Row::ChatgptModels,
                        Row::ChatgptModel,
                    ]);
                } else {
                    rows.extend([
                        Row::Edit(Field::ApiBaseUrl),
                        Row::Edit(Field::ApiModel),
                        Row::ApiKey,
                        Row::ClearApiKey,
                    ]);
                }
                rows
            }
            Page::Assistant => vec![
                Row::Page(Page::Presets),
                Row::WebSearch,
                Row::Edit(Field::BlockedWords),
            ],
            Page::Materials => vec![Row::Edit(Field::Name), Row::Persona],
            Page::Advanced => vec![Row::Visible, Row::Stop],
            Page::ReplyRange => [
                ReplyActivity::Cautious,
                ReplyActivity::Balanced,
                ReplyActivity::Active,
            ]
            .into_iter()
            .map(Row::Activity)
            .collect(),
            Page::Themes => (0..app.config.themes.entries().count())
                .map(Row::Theme)
                .chain([Row::ReloadThemes])
                .collect(),
            Page::Presets => ReplyPreset::ALL.into_iter().map(Row::Preset).collect(),
            Page::More => vec![
                Row::Page(Page::Obs),
                Row::Command("/diag"),
                Row::Command("/help"),
                Row::Command("/quit"),
            ],
            Page::Candidates if app.bridge.candidate_count() == 0 => Vec::new(),
            Page::Candidates => vec![
                Row::Action(Action::ApproveCandidate),
                Row::Action(Action::EditCandidate),
                Row::Action(Action::DiscardCandidate),
                Row::Action(Action::NextCandidate),
            ],
            Page::Diagnostics => vec![Row::RepairHistory],
        }
    }
}
impl RuntimeState {
    pub(crate) fn open_settings(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate_edit.is_none()
                && self.secret_target.is_none()
                && self.stop_flow.is_none()
                && self.login_qr.is_none(),
            "请先完成当前编辑或弹窗；人工草稿保留"
        );
        let mut panel = Panel::new();
        panel.page = Page::Settings;
        panel.row = 1;
        self.assistant_panel = Some(panel);
        Ok(())
    }
    fn open_direct(&mut self, page: Page) -> Result<()> {
        self.open_settings()?;
        let panel = self.assistant_panel.as_mut().expect("刚打开设置");
        panel.page = page;
        panel.row = 0;
        panel.history.clear();
        if page == Page::Models {
            let mut panel = self.assistant_panel.take().expect("刚打开设置");
            self.refresh_model_panel(&mut panel);
            self.assistant_panel = Some(panel);
        }
        Ok(())
    }
    pub(crate) fn open_archive_search(&mut self) -> Result<()> {
        self.open_settings()?;
        let mut panel = self.assistant_panel.take().expect("刚打开设置");
        panel.history.clear();
        panel.editor = Some((Field::ArchiveSearch, EditorInput::default()));
        self.assistant_panel = Some(panel);
        Ok(())
    }
    pub(crate) fn open_more(&mut self) -> Result<()> {
        self.open_direct(Page::More)
    }
    pub(crate) fn open_ai_range(&mut self) -> Result<()> {
        self.open_assistant()?;
        let p = self.assistant_panel.as_mut().unwrap();
        p.page = Page::ReplyRange;
        p.history.clear();
        Ok(())
    }
    pub(crate) fn open_ai_topic(&mut self) -> Result<()> {
        self.open_assistant()?;
        let mut p = self.assistant_panel.take().unwrap();
        p.page = Page::Home;
        p.history.clear();
        self.apply_assistant_row(Row::Edit(Field::Topic), &mut p)?;
        self.assistant_panel = Some(p);
        Ok(())
    }
    pub(crate) fn open_assistant(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate_edit.is_none()
                && self.secret_target.is_none()
                && self.stop_flow.is_none()
                && self.login_qr.is_none(),
            "请先完成当前编辑或弹窗；人工草稿保留"
        );
        self.runner.tick(&self.bridge);
        self.sync_assistant_room();
        self.assistant_panel = Some(Panel::new());
        Ok(())
    }
    pub(crate) fn open_diagnostics(&mut self) -> Result<()> {
        self.open_direct(Page::Diagnostics)
    }
    pub(crate) fn open_settings_section(&mut self, section: &str) -> Result<()> {
        let target = match section {
            "account" => Page::Account,
            "obs" => Page::Obs,
            "ai" => Page::AiSettings,
            "system" => Page::System,
            _ => anyhow::bail!("未知设置分类"),
        };
        self.open_direct(target)
    }
    pub(crate) fn open_ai_section(&mut self, section: &str) -> Result<()> {
        self.open_direct(match section {
            "model" => Page::Models,
            "replies" => Page::Assistant,
            "materials" => Page::Materials,
            "advanced" => Page::Advanced,
            _ => anyhow::bail!("未知AI设置"),
        })
    }
    pub(crate) fn open_about(&mut self) -> Result<()> {
        self.open_direct(Page::About)?;
        self.assistant_panel.as_mut().unwrap().expanded = true;
        Ok(())
    }
    fn external_setting_value(&self, field: ExternalField, panel: &Panel) -> String {
        match field {
            ExternalField::RoomTitle => self
                .room
                .as_ref()
                .map(|r| r.title.clone())
                .unwrap_or_default(),
            ExternalField::RoomCover => String::new(),
            ExternalField::ObsHost => panel
                .obs_configuration
                .as_ref()
                .map(|c| c.host.clone())
                .unwrap_or_default(),
            ExternalField::ObsPort => panel
                .obs_configuration
                .as_ref()
                .map(|c| c.port.to_string())
                .unwrap_or_default(),
            ExternalField::ObsMicrophone => panel
                .obs_configuration
                .as_ref()
                .map(|c| c.microphone_input_name.clone())
                .unwrap_or_default(),
            ExternalField::ObsScene => self
                .obs_status
                .as_ref()
                .map(|s| s.current_scene.clone())
                .unwrap_or_default(),
        }
    }
    pub(crate) async fn open_external_editor(
        &mut self,
        field: ExternalField,
        value: Option<&str>,
        tx: mpsc::Sender<CoreEvent>,
    ) -> Result<()> {
        anyhow::ensure!(
            self.local_transport.is_none(),
            "本地模式禁止真实账号与OBS操作"
        );
        self.open_direct(if field.is_obs() {
            Page::Obs
        } else {
            Page::Account
        })?;
        let mut panel = self.assistant_panel.take().unwrap();
        if field.is_obs() {
            panel.obs_configuration = Some(self.obs.configuration().await);
        }
        let result = self
            .apply_assistant_row(Row::Edit(Field::External(field)), &mut panel)
            .and_then(|_| {
                if let Some(value) = value {
                    panel.editor =
                        Some((Field::External(field), EditorInput::from(value.to_owned())));
                    self.save_assistant_editor(&mut panel, tx)?;
                }
                Ok(())
            });
        if let Err(error) = &result {
            panel.error = Some(error.to_string());
        }
        self.assistant_panel = Some(panel);
        result
    }
    pub(crate) async fn open_obs_settings(&mut self) -> Result<()> {
        self.open_direct(Page::Obs)?;
        self.assistant_panel.as_mut().unwrap().obs_configuration =
            Some(self.obs.configuration().await);
        Ok(())
    }
    pub(crate) fn project_setting_save(
        &mut self,
        token: uuid::Uuid,
        result: &std::result::Result<settings::SavedSetting, String>,
    ) {
        if let Some(panel) = self
            .assistant_panel
            .as_mut()
            .filter(|p| p.pending_save == Some(token))
        {
            panel.pending_save = None;
            match &result {
                Ok(saved) => {
                    panel.editor = None;
                    panel.error = None;
                    if let Some(config) = &saved.obs_configuration {
                        panel.obs_configuration = Some(config.clone());
                    }
                }
                Err(error) => panel.error = Some(format!("保存失败：{error}（内容保留）")),
            }
        }
    }
    pub(crate) fn configure_display(&mut self, setting: &str, value: &str) -> Result<()> {
        let (key, enabled) = match (setting, value) {
            ("names", "on" | "off") => ("show_name", value == "on"),
            ("time", "on" | "off") => ("show_time", value == "on"),
            ("layout", "chat" | "list") => ("chat_layout", value == "chat"),
            _ => anyhow::bail!("显示参数：names/time on|off，layout chat|list"),
        };
        self.config.save_value(key, enabled.into())?;
        match key {
            "show_name" => self.show_name = enabled,
            "show_time" => self.show_time = enabled,
            _ => self.layout_chat = enabled,
        }
        self.record_reading_config();
        self.set_notice("显示设置已保存", NoticeLevel::Success);
        Ok(())
    }
    pub(crate) fn open_theme_settings(&mut self) -> Result<()> {
        self.open_direct(Page::Themes)
    }
    pub(crate) fn open_candidate_review(&mut self) -> Result<()> {
        self.open_direct(Page::Candidates)
    }

    pub(crate) fn paste_assistant(&mut self, text: &str) {
        if let Some((_, editor)) = self
            .assistant_panel
            .as_mut()
            .filter(|p| p.choices.is_none() && p.pending_save.is_none())
            .and_then(|p| p.editor.as_mut())
        {
            if text.len() <= 4096 {
                editor.insert_paste(&sanitize_input(text.to_owned()), true);
            } else {
                self.set_notice("粘贴超过4096字节，未插入", NoticeLevel::Error);
            }
        }
    }
    fn apply_assistant_row(&mut self, row: Row, panel: &mut Panel) -> Result<bool> {
        if let Some(reason) = readonly_reason(row, self) {
            anyhow::bail!("只读：{reason}")
        }
        match row {
            Row::Group(_) => {}
            Row::Command(_) => anyhow::bail!("指令须经统一执行入口"),
            Row::Preset(preset) => {
                anyhow::ensure!(!self.runner.running, "请先暂停 AI");
                anyhow::ensure!(!self.runner.in_flight(), "等待当前回复结束后再换方案");
                let mut s = self.runner.settings.clone();
                preset.apply(&mut s);
                if s.web_search != self.runner.settings.web_search {
                    self.check_model_change()?;
                }
                if s != self.runner.settings {
                    self.runner.save(s, &self.bridge)?;
                }
                panel.back();
            }
            Row::Page(page) => {
                panel.navigate(page);
                if page == Page::Models {
                    self.refresh_model_panel(panel);
                }
                if panel
                    .rows(self)
                    .first()
                    .is_some_and(|row| readonly_reason(*row, self).is_some())
                {
                    self.move_assistant_selection(panel, true);
                }
                if page == Page::ReplyRange {
                    let current = self.runner.effective_reply_activity();
                    panel.row = panel
                        .rows(self)
                        .iter()
                        .position(|r| matches!(r, Row::Activity(activity) if *activity == current))
                        .unwrap_or(0)
                }
            }
            Row::ApiProtocol => {
                self.check_model_change()?;
                panel.choices = Some(Choices {
                    kind: ChoiceKind::ApiProtocol,
                    values: API_PROTOCOLS
                        .iter()
                        .map(|p| (*p).label().to_owned())
                        .collect(),
                    selected: API_PROTOCOLS
                        .iter()
                        .position(|p| *p == self.runner.settings.api.protocol)
                        .unwrap_or(0),
                });
            }
            Row::ApiKey => {
                self.begin_assistant_api_key()?;
            }
            Row::ClearApiKey => {
                self.check_model_change()?;
                panel.choices = Some(Choices {
                    kind: ChoiceKind::ClearApiKey,
                    values: vec!["保留 Key".into(), "清除 Key".into()],
                    selected: 0,
                });
            }
            Row::ChatgptAccount => {
                self.check_model_change()?;
                anyhow::ensure!(
                    !panel.accounts.is_empty(),
                    "没有已保存账号，请先 Continue with ChatGPT"
                );
                panel.choices = Some(Choices {
                    kind: ChoiceKind::ChatgptAccount,
                    values: panel.accounts.iter().map(account_choice_label).collect(),
                    selected: panel
                        .accounts
                        .iter()
                        .position(|a| {
                            Some(&a.id) == self.runner.settings.api.chatgpt_account.as_ref()
                        })
                        .unwrap_or(0),
                });
            }
            Row::ChatgptLogin => {
                self.begin_chatgpt_login(None)?;
            }
            Row::ChatgptRelogin => {
                let id = self
                    .runner
                    .settings
                    .api
                    .chatgpt_account
                    .clone()
                    .context("请先选择已保存账号")?;
                self.begin_chatgpt_login(Some(&id))?;
            }
            Row::ChatgptSignOut => {
                let id = self
                    .runner
                    .settings
                    .api
                    .chatgpt_account
                    .clone()
                    .context("请先选择账号")?;
                self.begin_chatgpt_sign_out(&id)?;
            }
            Row::ChatgptModels => {
                self.begin_chatgpt_models()?;
            }
            Row::ChatgptModel => {
                self.check_model_change()?;
                panel.choose_chatgpt_model(&self.runner.settings.api.model)?;
            }
            Row::Edit(field) => {
                let s = &self.runner.settings;
                let value = match field {
                    Field::ArchiveSearch => String::new(),
                    Field::External(field) => self.external_setting_value(field, panel),
                    Field::Name => s.name.clone(),
                    Field::ApiBaseUrl => s.api.base_url.clone(),
                    Field::ApiModel => s.api.model.clone(),
                    Field::BlockedWords => s.blocked_words.join("，"),
                    Field::Topic => self
                        .runner
                        .topic_override()
                        .unwrap_or(self.runner.effective_topic())
                        .to_owned(),
                };
                panel.editor = Some((field, EditorInput::from(value)));
            }
            Row::Theme(index) => {
                let id = self
                    .config
                    .themes
                    .entries()
                    .nth(index)
                    .context("主题列表已刷新")?
                    .0
                    .to_owned();
                if id != self.config.themes.selected() {
                    let (name, palette) = self.config.themes.select(&id)?;
                    self.config.theme_name = name;
                    self.config.palette = palette;
                    self.record_reading_config();
                }
                panel.back();
            }
            Row::ReloadThemes => {
                let (name, palette) = self.config.themes.reload()?;
                self.config.theme_name = name;
                self.config.palette = palette;
                self.record_reading_config()
            }
            Row::Login | Row::Logout => {
                self.account_command(if matches!(row, Row::Login) {
                    crate::runtime::AccountCommand::BeginLogin {
                        identity: crate::runtime::Identity::Main,
                    }
                } else {
                    crate::runtime::AccountCommand::SignOut {
                        identity: crate::runtime::Identity::Main,
                    }
                })?;
            }
            Row::ReuseMain | Row::SignOutIndependent => {
                self.account_command(if matches!(row, Row::ReuseMain) {
                    crate::runtime::AccountCommand::UseMainForAssistant
                } else {
                    crate::runtime::AccountCommand::SignOut {
                        identity: crate::runtime::Identity::Assistant,
                    }
                })?;
            }
            Row::AssistantLogin => {
                self.account_command(crate::runtime::AccountCommand::BeginLogin {
                    identity: crate::runtime::Identity::Assistant,
                })?;
            }
            Row::CancelAssistantLogin => {
                self.account_command(crate::runtime::AccountCommand::CancelLogin)?;
            }
            Row::Permission => {
                panel.choices = Some(Choices {
                    kind: ChoiceKind::Sending,
                    values: vec!["逐条发送".into(), "自动发送".into()],
                    selected: usize::from(self.runner.settings.automatic),
                });
            }
            Row::Activity(activity) => {
                if activity != self.runner.effective_reply_activity() {
                    self.runner.set_reply_activity_override(activity)
                }
                panel.back();
            }
            Row::Persona => {
                let mut s = self.runner.settings.clone();
                s.persona = match s.persona {
                    Persona::Assistant => Persona::Broadcaster,
                    Persona::Broadcaster => Persona::Assistant,
                };
                self.runner.save(s, &self.bridge)?
            }
            Row::RestoreTopic => self.runner.set_topic_override(String::new())?,
            Row::WebSearch => {
                self.check_model_change()?;
                let mut s = self.runner.settings.clone();
                s.web_search = !s.web_search;
                self.runner.save(s, &self.bridge)?;
            }
            Row::Action(Action::Run) => {
                if self.runner.running || self.resume_pending {
                    self.pause_assistant()
                } else {
                    self.start_assistant(false)?
                }
            }
            Row::Action(Action::Review) => panel.navigate(Page::Candidates),
            Row::Action(Action::NextCandidate) => {
                self.next_candidate();
                panel.scroll = 0
            }
            Row::Action(Action::EditCandidate) => {
                self.edit_review()?;
                return Ok(false);
            }
            Row::Action(Action::DiscardCandidate) => {
                self.discard_review()?;
                return Ok(true);
            }
            Row::Action(Action::ApproveCandidate) => {
                self.approve_review()?;
                return Ok(true);
            }
            Row::Action(Action::ApproveAllCandidates | Action::DiscardAllCandidates) => {
                self.review_all_candidates(matches!(
                    row,
                    Row::Action(Action::ApproveAllCandidates)
                ))?;
                return Ok(true);
            }
            Row::Visible => self.start_assistant(true)?,
            Row::Stop => {
                self.stop_assistant();
                return Ok(true);
            }
            Row::RepairHistory => self.runner.repair_history(&self.journal)?,
        }
        Ok(false)
    }
    fn save_assistant_editor(
        &mut self,
        panel: &mut Panel,
        tx: mpsc::Sender<CoreEvent>,
    ) -> Result<()> {
        let Some((field, editor)) = panel.editor.as_ref() else {
            return Ok(());
        };
        let field = *field;
        let value = editor.to_string();
        if let Field::External(field) = field {
            panel.pending_save = Some(self.start_external_setting(field, value, tx)?);
            panel.error = None;
            return Ok(());
        }
        if field == Field::ArchiveSearch {
            anyhow::ensure!(!value.trim().is_empty(), "请输入搜索关键词");
            self.search_archive(value.trim());
            panel.editor = None;
            panel.error = None;
            return Ok(());
        }
        if field == Field::Topic {
            self.runner.set_topic_override(value)?
        } else {
            let mut s = self.runner.settings.clone();
            match field {
                Field::Name => s.name = value,
                Field::ApiBaseUrl => {
                    self.check_model_change()?;
                    s.api.base_url = if value.trim().is_empty() {
                        String::new()
                    } else {
                        crate::runner::settings::normalize_base_url(&value)?
                    };
                }
                Field::ApiModel => {
                    self.check_model_change()?;
                    s.api.model = value.trim().to_owned();
                }
                Field::BlockedWords => {
                    s.blocked_words = value
                        .split([',', '，', ';', '；', '\n'])
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .collect()
                }
                _ => unreachable!(),
            }
            self.runner.save(s, &self.bridge)?
        }
        panel.editor = None;
        panel.error = None;
        self.set_notice("已保存", NoticeLevel::Success);
        Ok(())
    }
    fn select_assistant_choice(&mut self, panel: &mut Panel, index: usize) -> Result<()> {
        let choices = panel.choices.as_ref().context("选项已关闭")?;
        choices.values.get(index).context("选项已刷新")?;
        let kind = choices.kind;
        if matches!(kind, ChoiceKind::Sending) {
            let allow = index == 1;
            if allow != self.bridge.sending_enabled() || allow != self.runner.settings.automatic {
                self.set_automatic_permission(allow)?;
            }
        } else {
            self.check_model_change()?;
            let mut settings = self.runner.settings.clone();
            match kind {
                ChoiceKind::ApiProtocol => {
                    let protocol = *API_PROTOCOLS.get(index).context("连接选项已刷新")?;
                    if protocol != settings.api.protocol {
                        settings.api.protocol = protocol;
                        settings.api.base_url = protocol.default_base_url().to_owned();
                        settings.api.model.clear();
                        settings.api.chatgpt_account = None;
                        self.runner.save(settings, &self.bridge)?;
                        panel.models.clear();
                        self.refresh_model_panel(panel);
                    }
                }
                ChoiceKind::ClearApiKey => {
                    if index == 1 {
                        self.runner.clear_api_key(&self.bridge)?;
                    }
                }
                ChoiceKind::ChatgptAccount => {
                    let account = panel.accounts.get(index).context("账号列表已刷新")?;
                    if settings.api.chatgpt_account.as_deref() != Some(&account.id) {
                        settings.api.chatgpt_account = Some(account.id.clone());
                        settings.api.model.clear();
                        self.runner.save(settings, &self.bridge)?;
                        panel.models.clear();
                    }
                    panel.choices = None;
                    self.begin_chatgpt_models()?;
                }
                ChoiceKind::ChatgptModel => {
                    settings.api.model = panel
                        .models
                        .get(index)
                        .context("模型列表已刷新")?
                        .slug
                        .clone();
                    self.runner.save(settings, &self.bridge)?;
                }
                ChoiceKind::Sending => unreachable!(),
            }
        }
        panel.choices = None;
        Ok(())
    }
    fn check_model_change(&self) -> Result<()> {
        self.check_assistant_connection_change()
    }
    pub(crate) fn project_models(&mut self, models: Vec<ModelChoice>) -> Result<()> {
        let mut panel = self.assistant_panel.take().unwrap_or_else(Panel::new);
        if panel.page != Page::Models {
            panel.navigate(Page::Models);
        }
        self.refresh_model_panel(&mut panel);
        panel.editor = None;
        panel.error = None;
        panel.models = models;
        panel.choose_chatgpt_model(&self.runner.settings.api.model)?;
        self.assistant_panel = Some(panel);
        Ok(())
    }
    fn refresh_model_panel(&mut self, panel: &mut Panel) {
        self.runner.refresh_key_status();
        if self.runner.settings.api.protocol != ApiProtocol::ChatgptSubscription {
            return;
        }
        match self.runner.chatgpt_accounts() {
            Ok(accounts) => panel.accounts = accounts,
            Err(error) => {
                panel.accounts.clear();
                panel.error = Some(error.to_string());
            }
        }
    }
    fn move_assistant_selection(&self, panel: &mut Panel, forward: bool) {
        let rows = panel.rows(self);
        if rows.is_empty() {
            panel.row = 0;
            return;
        }
        let start = panel.row.min(rows.len() - 1);
        let mut next = start;
        loop {
            next = if forward {
                (next + 1) % rows.len()
            } else {
                (next + rows.len() - 1) % rows.len()
            };
            if readonly_reason(rows[next], self).is_none() || next == start {
                panel.row = next;
                break;
            }
        }
        panel.scroll = 0;
    }
    async fn activate_assistant_row(
        &mut self,
        index: usize,
        panel: &mut Panel,
        tx: mpsc::Sender<CoreEvent>,
    ) -> Result<bool> {
        let row = *panel
            .rows(self)
            .get(index)
            .context("菜单已变化，请重新操作")?;
        if let Row::Command(raw) = row {
            if raw == "/diag" {
                panel.navigate(Page::Diagnostics);
            } else {
                self.command(raw, tx).await?;
            }
            return Ok(matches!(raw, "/help" | "/quit"));
        }
        let close = self.apply_assistant_row(row, panel)?;
        Ok(close)
    }

    pub(crate) async fn assistant_key(
        &mut self,
        key: KeyEvent,
        tx: mpsc::Sender<CoreEvent>,
    ) -> bool {
        if self.candidate_edit.is_some() || self.secret_target.is_some() {
            return false;
        }
        let Some(mut panel) = self.assistant_panel.take() else {
            return false;
        };
        let result: Result<bool> = async {
            if panel.pending_save.is_some() {
                if key.code == KeyCode::Esc {
                    return Ok(true);
                }
                return Ok(false);
            }
            if panel.editor.is_none()
                && (key.code == KeyCode::F(1)
                    || matches!(key.code, KeyCode::Char('?')) && key.modifiers.is_empty())
            {
                panel.expanded = !panel.expanded;
                panel.scroll = 0;
                return Ok(false);
            }
            if panel.choices.is_some() {
                if key.code == KeyCode::Enter {
                    let selected = panel.choices.as_ref().unwrap().selected;
                    self.select_assistant_choice(&mut panel, selected)?
                } else {
                    let choices = panel.choices.as_mut().unwrap();
                    match key.code {
                        KeyCode::Esc => panel.choices = None,
                        KeyCode::Up | KeyCode::Left | KeyCode::BackTab => {
                            choices.selected =
                                (choices.selected + choices.values.len() - 1) % choices.values.len()
                        }
                        KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                            choices.selected = (choices.selected + 1) % choices.values.len()
                        }
                        KeyCode::Home => choices.selected = 0,
                        KeyCode::End => choices.selected = choices.values.len() - 1,
                        _ => {}
                    }
                }
                return Ok(false);
            }
            if matches!(panel.editor, Some((Field::Topic, _))) && key.code == KeyCode::F(2) {
                self.apply_assistant_row(Row::RestoreTopic, &mut panel)?;
                panel.editor = Some((
                    Field::Topic,
                    EditorInput::from(self.runner.effective_topic().to_owned()),
                ));
                return Ok(false);
            }
            if let Some((_, editor)) = panel.editor.as_mut() {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('u') => editor.clear(),
                        KeyCode::Char('a') => editor.move_to_start(),
                        KeyCode::Char('e') => editor.move_to_end(),
                        KeyCode::Char('w') => editor.delete_previous_word(),
                        KeyCode::Char('k') => editor.delete_to_end(),
                        _ => {}
                    }
                    return Ok(false);
                }
                match key.code {
                    KeyCode::Esc => {
                        panel.editor = None;
                        panel.error = None
                    }
                    KeyCode::Enter => self.save_assistant_editor(&mut panel, tx.clone())?,
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                        editor.insert_text(&c.to_string())
                    }
                    KeyCode::Backspace => editor.delete_before_cursor(),
                    KeyCode::Delete => editor.delete_at_cursor(),
                    KeyCode::Left => editor.move_left(),
                    KeyCode::Right => editor.move_right(),
                    KeyCode::Home => editor.move_to_start(),
                    KeyCode::End => editor.move_to_end(),
                    _ => {}
                }
                return Ok(false);
            }
            if panel.page == Page::Candidates && self.bridge.candidate_count() != 0 {
                match key.code {
                    KeyCode::Char('a' | 'd') if key.modifiers.is_empty() => {
                        return self.apply_assistant_row(
                            Row::Action(if key.code == KeyCode::Char('a') {
                                Action::ApproveAllCandidates
                            } else {
                                Action::DiscardAllCandidates
                            }),
                            &mut panel,
                        );
                    }
                    KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                        return self.apply_assistant_row(
                            Row::Action(Action::ApproveCandidate),
                            &mut panel,
                        );
                    }
                    KeyCode::Char('e') if key.modifiers.is_empty() => {
                        return self
                            .apply_assistant_row(Row::Action(Action::EditCandidate), &mut panel);
                    }
                    KeyCode::Delete => {
                        return self.apply_assistant_row(
                            Row::Action(Action::DiscardCandidate),
                            &mut panel,
                        );
                    }
                    KeyCode::Tab => {
                        self.next_candidate();
                        panel.scroll = 0;
                        return Ok(false);
                    }
                    KeyCode::PageDown => {
                        panel.scroll = panel.scroll.saturating_add(4);
                        return Ok(false);
                    }
                    KeyCode::PageUp => {
                        panel.scroll = panel.scroll.saturating_sub(4);
                        return Ok(false);
                    }
                    _ => {}
                }
            }
            let rows = panel.rows(self);
            panel.row = panel.row.min(rows.len().saturating_sub(1));
            match key.code {
                KeyCode::Esc => return Ok(panel.back()),
                KeyCode::Tab if SETTINGS_PAGES.contains(&panel.page) => {
                    let index = SETTINGS_PAGES
                        .iter()
                        .position(|page| *page == panel.page)
                        .unwrap();
                    panel.page = SETTINGS_PAGES[(index + 1) % SETTINGS_PAGES.len()];
                    panel.row = 0;
                    panel.list_offset = 0;
                    panel.scroll = 0;
                    panel.expanded = false;
                    panel.error = None;
                }
                KeyCode::BackTab if SETTINGS_PAGES.contains(&panel.page) => {
                    let index = SETTINGS_PAGES
                        .iter()
                        .position(|page| *page == panel.page)
                        .unwrap();
                    panel.page =
                        SETTINGS_PAGES[(index + SETTINGS_PAGES.len() - 1) % SETTINGS_PAGES.len()];
                    panel.row = 0;
                    panel.list_offset = 0;
                    panel.scroll = 0;
                    panel.expanded = false;
                    panel.error = None;
                }
                KeyCode::Up | KeyCode::Left | KeyCode::BackTab if !rows.is_empty() => {
                    self.move_assistant_selection(&mut panel, false);
                }
                KeyCode::Down | KeyCode::Right | KeyCode::Tab if !rows.is_empty() => {
                    self.move_assistant_selection(&mut panel, true);
                }
                KeyCode::Home => panel.row = 0,
                KeyCode::End => panel.row = rows.len().saturating_sub(1),
                KeyCode::PageDown if panel.expanded => {
                    panel.scroll = panel.scroll.saturating_add(3)
                }
                KeyCode::PageUp if panel.expanded => panel.scroll = panel.scroll.saturating_sub(3),
                KeyCode::PageDown => {
                    panel.row =
                        (panel.row + panel.menu_height.max(1)).min(rows.len().saturating_sub(1))
                }
                KeyCode::PageUp => panel.row = panel.row.saturating_sub(panel.menu_height.max(1)),
                KeyCode::Enter if !rows.is_empty() => {
                    return self.activate_assistant_row(panel.row, &mut panel, tx).await;
                }
                _ => {}
            }
            Ok(false)
        }
        .await;
        if panel.page == Page::Obs {
            panel.obs_configuration = Some(self.obs.configuration().await);
        }
        match result {
            Ok(true) => {}
            Ok(false) => {
                self.assistant_panel.get_or_insert(panel);
            }
            Err(e) => {
                self.assistant_panel.get_or_insert(panel).error = Some(e.to_string());
            }
        }
        true
    }
    /// Select a row from the last rendered list; activation stays in the shared key path.
    pub(crate) fn assistant_click(&mut self, event: MouseEvent) -> bool {
        if self.secret_target.is_some()
            || self.candidate_edit.is_some()
            || self.stop_flow.is_some()
            || self.login_qr.is_some()
            || event.kind != MouseEventKind::Down(crossterm::event::MouseButton::Left)
            || !event.modifiers.is_empty()
        {
            return false;
        }
        let Some(panel) = self.assistant_panel.as_ref() else {
            return false;
        };
        if panel.editor.is_some() || panel.choices.is_some() || panel.pending_save.is_some() {
            return false;
        }
        let Some((page, rect, offset)) = panel.menu_hit else {
            return false;
        };
        if page != panel.page
            || event.column < rect.x
            || event.column >= rect.right()
            || event.row < rect.y
            || event.row >= rect.bottom()
        {
            return false;
        }
        let index = offset + usize::from(event.row - rect.y);
        let rows = panel.rows(self);
        let Some(row) = rows.get(index).copied() else {
            return false;
        };
        if panel.menu_rows.get(index) != Some(&row) {
            return false;
        }
        if readonly_reason(row, self).is_some() {
            return false;
        }
        self.assistant_panel.as_mut().expect("菜单仍打开").row = index;
        true
    }
    pub(crate) fn assistant_mouse(&mut self, event: MouseEvent) -> bool {
        if self.secret_target.is_some() {
            return false;
        }
        let Some(mut panel) = self.assistant_panel.take() else {
            return false;
        };
        if matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            let down = event.kind == MouseEventKind::ScrollDown;
            if panel.editor.is_none() {
                if panel.expanded || panel.page == Page::Candidates {
                    panel.scroll = if down {
                        panel.scroll.saturating_add(3)
                    } else {
                        panel.scroll.saturating_sub(3)
                    }
                } else if let Some(c) = panel.choices.as_mut() {
                    c.selected = if down {
                        (c.selected + 1).min(c.values.len().saturating_sub(1))
                    } else {
                        c.selected.saturating_sub(1)
                    }
                } else {
                    self.move_assistant_selection(&mut panel, down)
                }
            }
        }
        self.assistant_panel = Some(panel);
        true
    }
}

const ASSISTANT_STARTUP_FRAMES: [&str; 4] =
    ["◐ AI启动中 ", "◓ AI启动中 ", "◑ AI启动中 ", "◒ AI启动中 "];
const ASSISTANT_STARTUP_SHORT_FRAMES: [&str; 4] = ["◐ 启动中", "◓ 启动中", "◑ 启动中", "◒ 启动中"];
const ASSISTANT_STARTUP_TINY_FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];

// The startup animation is only the local identity-resume check, not remote authentication.
pub(crate) fn compact(app: &RuntimeState, width: u16) -> Span<'static> {
    if (!app.runner.running && !app.resume_pending) || width == 0 {
        return Span::raw("");
    }
    if app.resume_pending {
        let frame = app.animation_tick as usize % ASSISTANT_STARTUP_FRAMES.len();
        let label = if width >= 11 {
            ASSISTANT_STARTUP_FRAMES[frame]
        } else if width >= 8 {
            ASSISTANT_STARTUP_SHORT_FRAMES[frame]
        } else {
            ASSISTANT_STARTUP_TINY_FRAMES[frame]
        };
        return Span::styled(
            label,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    }
    let color = if !app.runner.settings.automatic {
        Color::Green
    } else if app.bridge.sending_enabled() {
        Color::Red
    } else {
        Color::Yellow
    };
    let label = if width >= 2 {
        crate::bridge::AI_PREFIX
    } else {
        crate::bridge::AI_PREFIX.trim_end()
    };
    Span::styled(
        label,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}
pub(crate) fn run_label(app: &RuntimeState) -> &'static str {
    if app.runner.running || app.resume_pending {
        "暂停 AI"
    } else if app.runner.has_context() {
        "继续 AI"
    } else {
        "启动 AI"
    }
}
pub(crate) fn policy_label(app: &RuntimeState) -> &'static str {
    if !app.runner.settings.automatic {
        "逐条确认后发送"
    } else if app.bridge.sending_enabled() {
        "自动发送 · 本场已许可"
    } else {
        "自动发送 · 本场未许可"
    }
}
fn on_off(enabled: bool) -> &'static str {
    if enabled { "开启" } else { "关闭" }
}

fn readonly_reason(row: Row, app: &RuntimeState) -> Option<String> {
    match row {
        Row::ApiProtocol
        | Row::ApiKey
        | Row::ClearApiKey
        | Row::WebSearch
        | Row::ChatgptAccount
        | Row::ChatgptLogin
        | Row::ChatgptRelogin
        | Row::ChatgptSignOut
        | Row::ChatgptModels
        | Row::ChatgptModel
        | Row::Edit(Field::ApiBaseUrl | Field::ApiModel) => {
            app.check_model_change().err().map(|e| e.to_string())
        }
        Row::Edit(Field::External(_)) if app.local_transport.is_some() => {
            Some("本地模式禁止真实账号与OBS操作".into())
        }
        Row::Edit(Field::External(field))
            if !field.is_obs() && !matches!(app.account_status, AccountStatus::SignedIn { .. }) =>
        {
            Some("请先登录直播间所属B站主账号".into())
        }
        Row::Command(raw) if obs_command(raw) && app.local_transport.is_some() => {
            Some("本地模式禁止OBS操作".into())
        }
        Row::Action(Action::ApproveCandidate) => {
            app.reviewed_reply(true).err().map(|e| e.to_string())
        }
        Row::Action(
            Action::Review
            | Action::EditCandidate
            | Action::DiscardCandidate
            | Action::NextCandidate
            | Action::ApproveAllCandidates
            | Action::DiscardAllCandidates,
        ) if app.bridge.candidate_count() == 0 => Some("暂无待审回复".into()),
        Row::Group(_) => Some("只读信息".into()),
        Row::RepairHistory if app.runner.history_repair_running() => Some("重建中".into()),
        Row::Login | Row::Logout | Row::AssistantLogin if app.local_transport.is_some() => {
            Some("本地模式禁止真实账号操作".into())
        }
        Row::CancelAssistantLogin if !app.assistant_accounts.login_pending() => {
            Some("没有待取消的扫码".into())
        }
        Row::Preset(_) if app.runner.running => Some("请先暂停 AI".into()),
        Row::Preset(_) if app.runner.in_flight() => Some("等待当前回复结束".into()),
        _ => None,
    }
}

fn row_columns(row: Row, app: &RuntimeState, panel: &Panel) -> (String, String) {
    let s = &app.runner.settings;
    match row {
        Row::Group("人工发送") => ("人工发送".into(), account_label(&app.account_status)),
        Row::Group("AI 发送") => (
            "AI 发送".into(),
            app.assistant_accounts.label(&app.account_status),
        ),
        Row::Group("直播间") => (
            "直播间".into(),
            app.room
                .as_ref()
                .map(|r| format!("{} · {}", r.room_id, r.broadcaster_name))
                .unwrap_or_else(|| app.session.room_id.clone()),
        ),
        Row::Group("当前控制") => (
            "当前控制".into(),
            app.obs_status
                .as_ref()
                .map(|s| format!("{} · {:?} · {:?}", s.current_scene, s.stream, s.microphone))
                .unwrap_or_else(|| app.obs_error.clone().unwrap_or_else(|| "未连接".into())),
        ),
        Row::Group(name) => (name.into(), String::new()),
        Row::Command("/obs") => ("OBS".into(), "推流指令".into()),
        Row::Command("/diag") => ("诊断".into(), "连接与历史".into()),
        Row::Command("/help") => ("帮助".into(), String::new()),
        Row::Command("/commands") => ("指令搜索".into(), "中文搜索，保留草稿".into()),
        Row::Command("/quit") => ("退出弹幕台".into(), "不停止推流".into()),
        Row::Command(raw) => COMMAND_SPECS
            .iter()
            .chain(OBS_COMMAND_SPECS)
            .find(|s| s.completion.trim() == raw)
            .map(|s| (s.usage.into(), s.description.into()))
            .unwrap_or_else(|| (raw.into(), String::new())),
        Row::Page(Page::Account) if panel.page == Page::AiSettings => (
            "发送账号".into(),
            app.assistant_accounts.label(&app.account_status),
        ),
        Row::Page(Page::Account) => ("B站账号与直播间".into(), "账号、标题、封面".into()),
        Row::Page(Page::Obs) => ("OBS".into(), "连接配置与当前控制".into()),
        Row::Page(Page::AiSettings) => ("AI助手".into(), "模型、发送策略与维护".into()),
        Row::Page(Page::System) => ("系统与关于".into(), "工具信息、诊断与修复".into()),
        Row::Page(Page::Home) => ("本场 AI".into(), policy_label(app).into()),
        Row::Edit(Field::External(field)) => (
            field.label().into(),
            app.external_setting_value(field, panel),
        ),
        Row::Edit(Field::ArchiveSearch) => ("搜索归档".into(), "输入关键词".into()),
        Row::Page(Page::Models) => ("模型".into(), configuration_status(app).into()),
        Row::Page(Page::Assistant) => ("发送策略".into(), "方案、联网搜索与屏蔽词".into()),
        Row::Page(Page::Materials) => ("人设与资料".into(), "AI名字与人设".into()),
        Row::Page(Page::Advanced) => ("维护".into(), "可见弹幕与结束".into()),
        Row::Page(Page::Themes) => ("主题".into(), app.config.theme_name.clone()),
        Row::Page(Page::Presets) => ("方案".into(), "选择后立即保存".into()),
        Row::Page(Page::ReplyRange) => (
            "本场范围".into(),
            app.runner.effective_reply_activity().label().into(),
        ),
        Row::Page(page) => (page.title().into(), "打开".into()),
        Row::Preset(v) => (v.label().into(), String::new()),
        Row::Edit(Field::Name) => ("AI 名字".into(), s.name.clone()),
        Row::Edit(Field::Topic) => (
            "本场主题".into(),
            if app.runner.effective_topic().is_empty() {
                "未设置".into()
            } else {
                app.runner.effective_topic().into()
            },
        ),
        Row::Edit(Field::BlockedWords) => (
            "屏蔽词".into(),
            if s.blocked_words.is_empty() {
                "未设置".into()
            } else {
                s.blocked_words.join("，")
            },
        ),
        Row::Permission => (
            "发送模式".into(),
            if app.bridge.sending_enabled() {
                "自动 · 公开发送"
            } else if s.automatic {
                "未授权"
            } else {
                "逐条"
            }
            .into(),
        ),
        Row::Activity(activity) => (
            activity.label().into(),
            if activity == app.runner.effective_reply_activity() {
                "当前"
            } else {
                "选择"
            }
            .into(),
        ),
        Row::RestoreTopic => ("用直播标题".into(), String::new()),
        Row::Persona => ("人设".into(), s.persona.label().into()),
        Row::RepairHistory => (
            "重建历史".into(),
            if app.runner.history_repair_running() {
                "重建中"
            } else {
                "保留原件"
            }
            .into(),
        ),
        Row::WebSearch => ("联网搜索".into(), on_off(s.web_search).into()),
        Row::Theme(i) => {
            let e = app.config.themes.entries().nth(i);
            (
                e.map(|v| v.1).unwrap_or("主题").into(),
                e.map(|v| {
                    if v.0 == app.config.theme_name {
                        "当前"
                    } else {
                        v.0
                    }
                })
                .unwrap_or("")
                .into(),
            )
        }
        Row::ReloadThemes => ("重载主题".into(), "读取 themes.json".into()),
        Row::ApiProtocol => ("连接方式".into(), (s.api.protocol).label().into()),
        Row::Edit(Field::ApiBaseUrl) => ("API 基址".into(), s.api.base_url.clone()),
        Row::Edit(Field::ApiModel) => ("模型名".into(), s.api.model.clone()),
        Row::ApiKey => (
            "设置/更换 Key".into(),
            match &app.runner.key_status {
                KeyStatus::Set => "已设置",
                KeyStatus::Unset => "未设置",
                KeyStatus::Error(_) => "文件错误",
            }
            .into(),
        ),
        Row::ClearApiKey => ("清除 Key".into(), "需确认".into()),
        Row::ChatgptAccount => (
            "ChatGPT 账号".into(),
            selected_account(app, panel)
                .map(account_choice_label)
                .unwrap_or_else(|| "未选择".into()),
        ),
        Row::ChatgptLogin => ("Continue with ChatGPT".into(), "系统浏览器授权".into()),
        Row::ChatgptRelogin => ("重新登录当前账号".into(), "保留注册绑定".into()),
        Row::ChatgptSignOut => ("退出当前账号".into(), "清除本机 Token".into()),
        Row::ChatgptModels => (
            "读取账号模型".into(),
            if app.runner.connection_busy() {
                "账号/目录操作中"
            } else {
                "刷新/失败后重读"
            }
            .into(),
        ),
        Row::ChatgptModel => (
            "选择账号模型".into(),
            if s.api.model.is_empty() {
                if app.runner.connection_busy() {
                    "等待目录"
                } else {
                    "尚未选择"
                }
                .into()
            } else {
                s.api.model.clone()
            },
        ),
        Row::Login => ("登录".into(), account_label(&app.account_status)),
        Row::Logout => ("退出人工账号".into(), account_label(&app.account_status)),
        Row::ReuseMain => ("复用人工账号".into(), account_label(&app.account_status)),
        Row::AssistantLogin => (
            "扫码更换".into(),
            app.assistant_accounts.label(&app.account_status),
        ),
        Row::CancelAssistantLogin => ("取消扫码".into(), String::new()),
        Row::SignOutIndependent => (
            "退出 AI 账号".into(),
            app.assistant_accounts.label(&app.account_status),
        ),
        Row::Action(Action::Run) => (run_label(app).into(), String::new()),
        Row::Action(Action::Review) => (
            "待审回复".into(),
            format!("{} 条", app.bridge.candidate_count()),
        ),
        Row::Action(Action::ApproveCandidate) => ("发送".into(), "完整看完后可用".into()),
        Row::Action(Action::EditCandidate) => ("编辑".into(), "只保存".into()),
        Row::Action(Action::DiscardCandidate) => ("丢弃".into(), String::new()),
        Row::Action(Action::NextCandidate) => ("下一条".into(), String::new()),
        Row::Action(Action::ApproveAllCandidates) => {
            ("全部发送".into(), "批准当前待审批次，不开启自动发送".into())
        }
        Row::Action(Action::DiscardAllCandidates) => {
            ("全部丢弃".into(), "不影响在途或结果不确定的回复".into())
        }
        Row::Visible => ("处理可见弹幕".into(), "仅未消费的消息".into()),
        Row::Stop => ("结束 AI".into(), "清除续用与发送权".into()),
    }
}

// Width is measured in terminal cells, never UTF-8 bytes or Rust's scalar padding.
fn column_line(name: &str, value: &str, width: usize, label_width: usize) -> String {
    let label_width = label_width.min(width.saturating_sub(8));
    let name = fit_display_width(name, label_width);
    let padding = label_width.saturating_sub(UnicodeWidthStr::width(name.as_str()));
    format!(
        "{name}{}  {}",
        " ".repeat(padding),
        fit_display_width(value, width.saturating_sub(label_width + 2))
    )
}

fn account_choice_label(account: &AccountInfo) -> String {
    format!(
        "{} · {}",
        sanitize_input(account.label.clone()),
        if !account.signed_in {
            "已退出"
        } else if !account.plan_enabled {
            "未获订阅推理权限"
        } else {
            "已登录"
        }
    )
}
fn selected_account<'a>(app: &RuntimeState, panel: &'a Panel) -> Option<&'a AccountInfo> {
    panel
        .accounts
        .iter()
        .find(|a| Some(&a.id) == app.runner.settings.api.chatgpt_account.as_ref())
}
fn configuration_status(app: &RuntimeState) -> &'static str {
    if app.runner.connection_busy() {
        "账号/模型操作进行中"
    } else if app.runner.settings.api.base_url.is_empty()
        || app.runner.settings.api.model.is_empty()
        || (app.runner.settings.api.protocol == ApiProtocol::ChatgptSubscription
            && app.runner.settings.api.chatgpt_account.is_none())
    {
        "连接配置尚未齐备"
    } else if app.runner.settings.api.protocol == ApiProtocol::ChatgptSubscription {
        "已选择账号与模型；启动时检查登录"
    } else {
        match &app.runner.key_status {
            KeyStatus::Set => "本机配置齐备；未验证远端鉴权",
            KeyStatus::Unset => "尚未设置当前连接 Key",
            KeyStatus::Error(_) => "Key 文件错误",
        }
    }
}
fn selected_details(app: &RuntimeState, panel: &Panel) -> String {
    let Some(row) = panel.rows(app).get(panel.row).copied() else {
        return if panel.page == Page::Diagnostics {
            diagnostics_details(app)
        } else {
            "暂无可操作项".into()
        };
    };
    match row {
        Row::Edit(Field::External(field)) => field.description().into(),
        Row::Preset(preset) => {
            use std::fmt::Write;
            let before = &app.runner.settings;
            let mut after = before.clone();
            preset.apply(&mut after);
            let mut text = format!(
                "{}\n只修改以下十项；不改账号、模型和发送许可。",
                preset.description()
            );
            for (name, old, new) in [
                (
                    "默认范围",
                    before.reply_activity.label(),
                    after.reply_activity.label(),
                ),
                (
                    "礼物与上舰",
                    on_off(before.thank_gifts),
                    on_off(after.thank_gifts),
                ),
                (
                    "点赞",
                    on_off(before.thank_likes),
                    on_off(after.thank_likes),
                ),
                (
                    "关注",
                    on_off(before.thank_follows),
                    on_off(after.thank_follows),
                ),
                (
                    "分享",
                    on_off(before.thank_shares),
                    on_off(after.thank_shares),
                ),
                (
                    "问答时 @ 对方",
                    on_off(before.mention_sender),
                    on_off(after.mention_sender),
                ),
                (
                    "联网搜索",
                    on_off(before.web_search),
                    on_off(after.web_search),
                ),
                (
                    "拒绝时改写",
                    on_off(before.repair_blocked),
                    on_off(after.repair_blocked),
                ),
                (
                    "开麦主播优先",
                    on_off(before.dynamic_host_priority),
                    on_off(after.dynamic_host_priority),
                ),
                (
                    "静音文字补位",
                    on_off(before.dynamic_muted_support),
                    on_off(after.dynamic_muted_support),
                ),
            ] {
                let _ = write!(text, "\n{name}：{old} → {new}");
            }
            text
        }
        Row::Permission => format!(
            "当前发送账号：{} · 房间 {}。自动发送会公开发送，并按同房间同账号记住；逐条发送立即撤权。",
            app.assistant_accounts.label(&app.account_status),
            app.session.room_id
        ),
        Row::ApiProtocol => "三种明确连接协议，不探测、不回退。更换协议会设置官方基址并清空模型；重选当前协议保留自定义地址与模型。旧作用域 Key 不会发送到新服务，也不会自动删除。API 可能计费；ChatGPT 为官方 preview，共享订阅额度并受每应用限制。".into(),
        Row::Edit(Field::ApiBaseUrl) => "包含版本路径的服务根，例如 https://api.openai.com/v1。仅 HTTPS 或 loopback IP HTTP；不得含认证、query 或 fragment。换地址后需为新作用域设置 Key；先暂停并撤销发送权。".into(),
        Row::Edit(Field::ApiModel) | Row::ChatgptModel => format!(
            "{}\n模型名必须明确选择，不自动回退。文件级 max_output_tokens={}，openai_token_limit={:?}；外部编辑 assistant.json 后重启读取。{}",
            app.runner.settings.api.model, app.runner.settings.api.max_output_tokens,
            app.runner.settings.api.openai_token_limit,
            if app.runner.settings.api.protocol == ApiProtocol::ChatgptSubscription { "订阅忽略这两个输出字段，不发送到 Responses。" }
            else { "OpenAI 只发所选 token 字段；Anthropic 固定 max_tokens。" }),
        Row::ApiKey | Row::ClearApiKey => {
            let error = match &app.runner.key_status { KeyStatus::Error(e) => format!("\n文件错误：{e}"), _ => String::new() };
            format!("Key 是本机私有权限保护的明文，不是系统保险箱。设置/更换只打开隐藏输入，不回填旧 Key；替换唯一 Key 记录。清除需明确确认，空输入不是清除。运行或已有发送权时禁止修改。{error}")
        }
        Row::ChatgptAccount | Row::ChatgptLogin | Row::ChatgptRelogin | Row::ChatgptSignOut | Row::ChatgptModels => "Continue with ChatGPT 在系统浏览器由本人授权。登录保存或显式切换账号后自动读取一次模型目录，成功后直接选择；同注册仍可用的已选模型保留。目录失败不清除登录，可手动重新读取，无需重登录，不自动重试、推理或发送；认证失效按错误提示处理。OAuth Token 在私有文件明文保存；退出不删除注册，远端撤销结果按实际回报。官方 preview 共享订阅额度、每应用限制，可在 https://chatgpt.com/settings/usage 查看；不读取 ChatGPT 旧聊天、不自动转 API 计费。".into(),
        Row::WebSearch => "只把被选中的本批完整公开问题正文发送到 Exa；不发送私有资料或历史扩写查询。每轮最多一次搜索、最多3条来源；可能限流或不可用，不承诺永久免费。不可用时候选强制待审；模型 API 本身可能收费。".into(),
        Row::RepairHistory => format!(
            "先备份派生历史副本，再从私有原件重建；原件不变。{}",
            app.runner
                .history_repair_result()
                .map(|v| format!("\n上次结果：{v}"))
                .unwrap_or_default()
        ),
        Row::Visible => "显式处理当前可见且尚未消费的消息；可能消耗模型额度，不开启自动发送、不重放旧批。".into(),
        Row::Stop => "停止本机模型 worker，清除续用意图和自动发送权；候选与人工草稿保留。不声称远端推理或收费已停止。".into(),
        _ => {
            let (name, value) = row_columns(row, app, panel);
            format!("{name}：{value}")
        }
    }
}
fn details(app: &RuntimeState, panel: &Panel) -> String {
    if let Some(e) = &panel.error {
        return format!("{e}\n\n{}", selected_details(app, panel));
    }
    if let Some((Field::External(field), editor)) = &panel.editor {
        return format!(
            "{}：{}
{}
保存失败时内容保留。",
            field.label(),
            &**editor,
            field.description()
        );
    }
    if let Some((field, editor)) = &panel.editor {
        let hint = if *field == Field::ArchiveSearch {
            "输入关键词后搜索；取消不搜索。"
        } else {
            "保存失败时内容保留；取消不写入。"
        };
        return format!(
            "{}：{}
{hint}",
            field.label(),
            &**editor
        );
    }
    if let Some(c) = &panel.choices {
        return match c.kind {
            ChoiceKind::Sending => format!("发送账号：{} · 房间 {}。自动发送会公开发送并记住本房间与账号；逐条立即撤权。", app.assistant_accounts.label(&app.account_status), app.session.room_id),
            ChoiceKind::ApiProtocol => "更换协议设置官方基址并清空模型；重选当前项不重置。旧 Key 不删除、不跨作用域发送；不自动推理或授权发送。".into(),
            ChoiceKind::ClearApiKey => "确认清除唯一私有 API Key 记录；不删除目录或 ChatGPT 账号。选择保留不会写入。".into(),
            ChoiceKind::ChatgptAccount => "切换账号会清空旧账号模型并自动读取所选账号目录；成功后直接选择模型。已退出或缺订阅权限的账号不能推理。".into(),
            ChoiceKind::ChatgptModel => "选择当前账号目录中的模型；仍可用的已选模型保留。保存不启动推理、不授予发送权。".into(),
        };
    }
    if panel.page == Page::About {
        format!(
            "拾穗弹幕台 DANMU\n面向知识型主播的弹幕与提问工作台\n\n版本：{}\n官网：https://danmu.elazer.wang\n创始人：Elazer\n联系：apps@elazer.wang\n源码：{}\n许可：{}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_REPOSITORY"),
            env!("CARGO_PKG_LICENSE")
        )
    } else if panel.page == Page::Diagnostics {
        diagnostics_details(app)
    } else {
        selected_details(app, panel)
    }
}
fn row_danger(row: Row, app: &RuntimeState) -> bool {
    if let Row::Command(raw) = row {
        return COMMAND_SPECS
            .iter()
            .chain(OBS_COMMAND_SPECS)
            .find(|s| s.completion.trim() == raw)
            .is_some_and(|s| s.danger);
    }
    matches!(
        row,
        Row::Visible
            | Row::ClearApiKey
            | Row::ChatgptSignOut
            | Row::Stop
            | Row::RepairHistory
            | Row::Logout
            | Row::SignOutIndependent
            | Row::Edit(Field::External(
                ExternalField::RoomTitle | ExternalField::RoomCover | ExternalField::ObsScene
            ))
    ) || matches!(row, Row::Permission) && app.bridge.sending_enabled()
}

pub(crate) fn draw(frame: &mut ratatui::Frame, app: &mut RuntimeState) {
    let Some(mut panel) = app.assistant_panel.take() else {
        return;
    };
    panel.menu_hit = None;
    if panel.page == Page::Candidates {
        app.assistant_panel = Some(panel);
        draw_candidates(frame, app);
        return;
    }
    let screen = frame.area();
    let width = screen.width.min(104);
    let height = screen
        .height
        .min(if panel.page == Page::Home { 18 } else { 30 });
    let area = Rect::new(
        (screen.width - width) / 2,
        (screen.height - height) / 2,
        width,
        height,
    );
    let p = app.config.palette;
    let accent = Style::default().fg(p.info).add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(p.time);
    let block = Block::bordered()
        .title(Line::styled(format!(" {} ", panel.page.title()), accent))
        .border_style(Style::default().fg(p.frame))
        .style(Style::default().bg(p.background).fg(p.content));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if panel.page == Page::About {
        let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        let paragraph = Paragraph::new(details(app, &panel)).wrap(Wrap { trim: false });
        panel.scroll = panel
            .scroll
            .min((paragraph.line_count(areas[0].width) as u16).saturating_sub(areas[0].height));
        frame.render_widget(paragraph.scroll((panel.scroll, 0)), areas[0]);
        frame.render_widget(
            Paragraph::new(fit_display_width(
                "Esc返回 · PgUp/PgDn阅读",
                areas[1].width.into(),
            ))
            .style(muted),
            areas[1],
        );
        app.assistant_panel = Some(panel);
        return;
    }
    let detail_height = if panel.editor.is_some() {
        5
    } else if panel.expanded || panel.page == Page::About {
        inner.height.saturating_sub(8)
    } else {
        3
    };
    let show_navigation =
        SETTINGS_PAGES.contains(&panel.page) && panel.editor.is_none() && panel.choices.is_none();
    let nav_columns = (inner.width.saturating_add(1) / 7).max(1);
    let header_height = if show_navigation {
        (SETTINGS_PAGES.len() as u16).div_ceil(nav_columns)
    } else {
        1
    };
    let areas = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(detail_height.min(inner.height.saturating_sub(7))),
        Constraint::Length(1),
    ])
    .split(inner);
    let header = if panel.page == Page::Home {
        format!(
            "{} · {} · 房间 {} · {}",
            app.runner.state().split('；').next().unwrap_or("未启用"),
            app.assistant_accounts.label(&app.account_status),
            app.session.room_id,
            policy_label(app)
        )
    } else if panel.page == Page::Models {
        configuration_status(app).into()
    } else if panel.is_settings() {
        "设置按需保存；当前操作立即执行".into()
    } else {
        "方向键选择，Enter 执行".into()
    };
    if show_navigation {
        for (index, page) in SETTINGS_PAGES.iter().copied().enumerate() {
            let column = index as u16 % nav_columns;
            let row = index as u16 / nav_columns;
            if row >= areas[0].height || column * 7 + 6 > areas[0].width {
                continue;
            }
            let rect = Rect::new(areas[0].x + column * 7, areas[0].y + row, 6, 1);
            frame.render_widget(
                Paragraph::new(match page {
                    Page::Account => "B站",
                    Page::Obs => "OBS",
                    Page::AiSettings => "AI",
                    _ => "系统",
                })
                .style(if page == panel.page { accent } else { muted }),
                rect,
            );
        }
    } else {
        frame.render_widget(
            Paragraph::new(fit_display_width(&header, areas[0].width.into())).style(muted),
            areas[0],
        );
    }
    let menu_rows = panel.rows(app);
    let (items, selected): (Vec<ListItem>, usize) = if let Some(c) = &panel.choices {
        (
            c.values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let current = match c.kind {
                        ChoiceKind::Sending => i == usize::from(app.runner.settings.automatic),
                        ChoiceKind::ApiProtocol => {
                            API_PROTOCOLS.get(i) == Some(&app.runner.settings.api.protocol)
                        }
                        ChoiceKind::ClearApiKey => i == 0,
                        ChoiceKind::ChatgptAccount => panel.accounts.get(i).is_some_and(|a| {
                            Some(&a.id) == app.runner.settings.api.chatgpt_account.as_ref()
                        }),
                        ChoiceKind::ChatgptModel => panel
                            .models
                            .get(i)
                            .is_some_and(|m| m.slug == app.runner.settings.api.model),
                    };
                    ListItem::new(format!("{}{}", if current { "[当前] " } else { "" }, v))
                })
                .collect(),
            c.selected,
        )
    } else {
        let w = usize::from(areas[1].width.saturating_sub(2));
        let lw = menu_rows
            .iter()
            .map(|r| UnicodeWidthStr::width(row_columns(*r, app, &panel).0.as_str()))
            .max()
            .unwrap_or(0)
            .min(w.saturating_sub(6));
        (
            menu_rows
                .iter()
                .map(|r| {
                    let (n, v) = row_columns(*r, app, &panel);
                    let color = if row_danger(*r, app) {
                        p.warning
                    } else if readonly_reason(*r, app).is_some() {
                        p.time
                    } else {
                        p.content
                    };
                    ListItem::new(Line::styled(
                        column_line(&n, &v, w, lw),
                        Style::default().fg(color),
                    ))
                })
                .collect(),
            panel.row,
        )
    };
    let count = items.len();
    let mut state = ListState::default()
        .with_offset(panel.list_offset)
        .with_selected(if count == 0 {
            None
        } else {
            Some(selected.min(count - 1))
        });
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("› ")
            .highlight_style(Style::default().bg(p.frame).add_modifier(Modifier::BOLD)),
        areas[1],
        &mut state,
    );
    panel.list_offset = state.offset();
    panel.menu_height = usize::from(areas[1].height);
    if panel.editor.is_none() && panel.choices.is_none() && panel.pending_save.is_none() {
        panel.menu_hit = Some((panel.page, areas[1], state.offset()));
        panel.menu_rows = menu_rows;
    }
    let detail = if panel.expanded {
        details(app, &panel)
    } else if let Some(error) = &panel.error {
        error.clone()
    } else if panel
        .choices
        .as_ref()
        .is_some_and(|choices| matches!(choices.kind, ChoiceKind::Sending))
        || panel.page == Page::Home
    {
        "公开发送 · 同房间同账号记住".into()
    } else if panel.page == Page::Models {
        configuration_status(app).into()
    } else if panel
        .rows(app)
        .get(panel.row)
        .is_some_and(|row| row_danger(*row, app))
    {
        selected_details(app, &panel)
    } else {
        String::new()
    };
    let color = if panel.error.is_some() {
        p.warning
    } else {
        p.time
    };
    let paragraph = Paragraph::new(detail)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(color));
    let max_scroll = paragraph
        .line_count(areas[2].width)
        .saturating_sub(usize::from(areas[2].height));
    panel.scroll = panel
        .scroll
        .min(max_scroll.min(usize::from(u16::MAX)) as u16);
    frame.render_widget(paragraph.scroll((panel.scroll, 0)), areas[2]);
    let footer = if panel.pending_save.is_some() {
        "保存中 · Esc返回，操作仍继续"
    } else if matches!(panel.editor, Some((Field::Topic, _))) {
        "Esc取消 · Enter保存 · F2直播标题"
    } else if panel.editor.is_some() {
        "Esc取消 · Enter保存"
    } else if panel.choices.is_some() {
        "Esc返回 · ↑↓ Enter确认 · ?说明"
    } else if SETTINGS_PAGES.contains(&panel.page) {
        "Esc返回 · ↑↓ Enter · Tab分类 · ?说明"
    } else {
        "Esc返回 · ↑↓ Enter · ?说明"
    };
    frame.render_widget(
        Paragraph::new(fit_display_width(footer, usize::from(areas[3].width)))
            .style(Style::default().fg(p.time)),
        areas[3],
    );
    app.assistant_panel = Some(panel)
}

fn draw_candidates(frame: &mut ratatui::Frame, app: &mut RuntimeState) {
    let screen = frame.area();
    let width = screen.width.min(92);
    let height = screen.height.min(28);
    let area = Rect::new(
        (screen.width - width) / 2,
        (screen.height - height) / 2,
        width,
        height,
    );
    let p = app.config.palette;
    let block = Block::bordered()
        .title(Line::styled(" 待审回复 ", Style::default().fg(p.info)))
        .border_style(Style::default().fg(p.frame))
        .style(Style::default().bg(p.background).fg(p.content));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let empty = app.bridge.candidate_count() == 0;
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(if empty { 0 } else { 2 }),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "待审 {} 条 · 全部发送只批准当前批次",
            app.bridge.candidate_count()
        ))
        .style(Style::default().fg(p.time)),
        areas[0],
    );
    let Some(mut panel) = app.assistant_panel.take() else {
        return;
    };
    if empty {
        app.review_frame = None;
        frame.render_widget(Paragraph::new("暂无待审回复。"), areas[1])
    } else {
        app.draw_review_body(frame, areas[1], &mut panel.scroll);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(fit_display_width(
                    "Shift+Enter 发送 · e 编辑 · Delete 丢弃 · Tab 下一条",
                    usize::from(areas[2].width),
                )),
                Line::styled(
                    fit_display_width("a 全部发送 · d 全部丢弃", usize::from(areas[2].width)),
                    Style::default().fg(p.warning),
                ),
            ])
            .style(Style::default().fg(p.info)),
            areas[2],
        );
    }
    let notice = if let Some(error) = &panel.error {
        error.clone()
    } else if panel.expanded {
        selected_details(app, &panel)
    } else {
        app.reviewed_reply(true)
            .err()
            .map_or_else(|| "Esc 返回 · F1/? 说明".into(), |_| "下滑看完".into())
    };
    frame.render_widget(
        Paragraph::new(fit_display_width(&notice, usize::from(areas[3].width))).style(
            Style::default().fg(if panel.error.is_some() {
                p.warning
            } else {
                p.time
            }),
        ),
        areas[3],
    );
    app.assistant_panel = Some(panel)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_sections_close_without_synthetic_parents() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_ai_section("model").unwrap();
        let p = app.assistant_panel.as_ref().unwrap();
        assert_eq!(p.page, Page::Models);
        assert!(p.history.is_empty());
    }
    #[tokio::test]
    async fn settings_categories_and_details_are_keyboard_reachable() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_settings_section("account").unwrap();
        let (tx, _) = mpsc::channel(1);

        assert!(
            app.assistant_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx.clone())
                .await
        );
        let panel = app.assistant_panel.as_ref().unwrap();
        assert_eq!(panel.page, Page::Obs);
        assert!(panel.history.is_empty());

        assert!(
            app.assistant_key(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                tx.clone()
            )
            .await
        );
        assert!(app.assistant_panel.as_ref().unwrap().expanded);

        assert!(
            app.assistant_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
                .await
        );
        assert!(app.assistant_panel.is_none());
    }
    #[tokio::test]
    async fn topic_editor_f2_restores_the_live_title_source() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.runner.set_topic_override("临时主题".into()).unwrap();
        app.open_ai_topic().unwrap();
        let (tx, _) = mpsc::channel(1);

        assert!(
            app.assistant_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE), tx)
                .await
        );
        assert_eq!(app.runner.topic_override(), None);
        let panel = app.assistant_panel.as_ref().unwrap();
        assert_eq!(
            panel.editor.as_ref().unwrap().1.to_string(),
            app.runner.effective_topic()
        );
    }
    #[test]
    fn preset_applies_once_without_preview_and_preserves_identity_fields() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut s = app.runner.settings.clone();
        s.name = "保留名字".into();
        s.automatic = true;
        app.runner.save(s, &app.bridge).unwrap();
        let mut p = Panel::new();
        p.page = Page::Presets;
        p.history.push(HistoryEntry {
            page: Page::Assistant,
            row: 0,
            scroll: 0,
            list_offset: 0,
            expanded: false,
        });
        app.apply_assistant_row(Row::Preset(ReplyPreset::TextQa), &mut p)
            .unwrap();
        assert_eq!(p.page, Page::Assistant);
        assert_eq!(app.runner.settings.name, "保留名字");
        assert!(app.runner.settings.automatic);
    }
    #[test]
    fn sending_mode_is_an_explicit_choice() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut p = Panel::new();
        app.apply_assistant_row(Row::Permission, &mut p).unwrap();
        let c = p.choices.as_ref().unwrap();
        assert_eq!(c.values.len(), 2);
        assert!(matches!(c.kind, ChoiceKind::Sending));
        assert!(!app.bridge.sending_enabled());
        assert!(!app.runner.running);
    }
    #[tokio::test]
    async fn failed_editor_save_keeps_the_exact_input() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_ai_section("materials").unwrap();
        let mut panel = app.assistant_panel.take().unwrap();
        app.apply_assistant_row(Row::Edit(Field::Name), &mut panel)
            .unwrap();
        panel.editor.as_mut().unwrap().1.clear();
        panel.editor.as_mut().unwrap().1.insert_text("137");
        std::fs::create_dir(root.path().join("assistant.json")).unwrap();
        app.assistant_panel = Some(panel);
        let (tx, _) = mpsc::channel(1);
        assert!(
            app.assistant_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
                .await
        );
        let panel = app.assistant_panel.as_ref().unwrap();
        assert_eq!(panel.editor.as_ref().unwrap().1.to_string(), "137");
        assert!(panel.error.is_some());
    }
    #[test]
    fn child_back_restores_parent_selection_and_direct_back_closes() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_settings().unwrap();
        let mut p = app.assistant_panel.take().unwrap();
        p.row = 3;
        app.apply_assistant_row(Row::Page(Page::AiSettings), &mut p)
            .unwrap();
        app.apply_assistant_row(Row::Page(Page::Materials), &mut p)
            .unwrap();
        assert_eq!(p.page, Page::Materials);
        assert!(!p.back());
        assert_eq!(p.page, Page::AiSettings);
        assert!(!p.back());
        assert_eq!(p.page, Page::Settings);
        assert_eq!(p.row, 3);
        app.assistant_panel = Some(p);
        app.open_ai_section("materials").unwrap();
        assert!(app.assistant_panel.as_mut().unwrap().back());
    }
    #[test]
    fn reselecting_current_protocol_preserves_custom_connection() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut settings = app.runner.settings.clone();
        settings.api.base_url = "https://example.com/custom/v1".into();
        settings.api.model = "custom-model".into();
        app.runner.save(settings, &app.bridge).unwrap();
        let before = app.runner.settings.api.clone();
        let mut panel = Panel::new();
        app.apply_assistant_row(Row::ApiProtocol, &mut panel)
            .unwrap();
        app.select_assistant_choice(&mut panel, 0).unwrap();
        assert_eq!(app.runner.settings.api, before);
        app.apply_assistant_row(Row::ApiProtocol, &mut panel)
            .unwrap();
        app.select_assistant_choice(&mut panel, 1).unwrap();
        assert_eq!(
            app.runner.settings.api.protocol,
            ApiProtocol::AnthropicMessages
        );
        assert_eq!(
            app.runner.settings.api.base_url,
            ApiProtocol::AnthropicMessages.default_base_url()
        );
        assert!(app.runner.settings.api.model.is_empty());
        assert!(!app.runner.running);
        assert!(!app.bridge.sending_enabled());
    }
    #[test]
    fn connection_rows_reject_running_permission_and_pending_resume_without_writes() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.runner
            .save(app.runner.settings.clone(), &app.bridge)
            .unwrap();
        let before = std::fs::read(root.path().join("assistant.json")).unwrap();
        let rows = [
            Row::ApiProtocol,
            Row::ApiKey,
            Row::ClearApiKey,
            Row::WebSearch,
            Row::Edit(Field::ApiBaseUrl),
            Row::Edit(Field::ApiModel),
            Row::ChatgptLogin,
        ];
        for state in 0..3 {
            app.runner.running = state == 0;
            app.resume_pending = state == 1;
            app.bridge.permission(state == 2).unwrap();
            for row in rows {
                let mut panel = Panel::new();
                assert!(app.apply_assistant_row(row, &mut panel).is_err());
                assert!(panel.editor.is_none() && panel.choices.is_none());
                assert!(app.secret_target.is_none());
            }
            assert_eq!(
                std::fs::read(root.path().join("assistant.json")).unwrap(),
                before
            );
            assert!(!app.runner.settings.web_search);
        }
        app.runner.running = false;
        app.resume_pending = false;
        app.bridge.permission(false).unwrap();
        let mut panel = Panel::new();
        app.apply_assistant_row(Row::WebSearch, &mut panel).unwrap();
        assert!(app.runner.settings.web_search);
    }
    #[tokio::test]
    async fn subscription_setup_automatically_reads_catalog_and_requires_model_choice() {
        use crate::runner::{
            responses, subscription,
            test_support::{HttpServer, Response, Scenario},
        };
        use serde_json::json;
        const TEST: &str = "terminal::assistant::tests::subscription_setup_automatically_reads_catalog_and_requires_model_choice";
        const PHASE: &str = "DANMU_TEST_SUBSCRIPTION_SETUP";
        match std::env::var(PHASE) {
            Err(std::env::VarError::NotPresent) => {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env(PHASE, "loopback")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            Ok(phase) => assert_eq!(phase, "loopback"),
            Err(error) => panic!("invalid fixture phase: {error}"),
        }
        const SCOPE: &str =
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
        fn catalog() -> Response {
            Response::json(json!({"models":[
                {"visibility":"list", "slug":"first", "display_name":"First"},
                {"visibility":"list", "slug":"second", "display_name":"Second"}
            ]}))
        }
        async fn finish_models(app: &mut RuntimeState, rx: &mut mpsc::Receiver<CoreEvent>) {
            let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(event, CoreEvent::ChatgptModels { .. }));
            app.handle_ui_event(event);
        }
        async fn login(app: &mut RuntimeState, oauth: &HttpServer, selected: Option<&str>) {
            let login = app
                .runner
                .begin_chatgpt_login(&app.bridge, selected)
                .unwrap();
            let token = uuid::Uuid::new_v4();
            app.chatgpt_operation = Some(ChatgptOperation {
                token,
                account_id: app.runner.settings.api.chatgpt_account.clone(),
                scene_id: app.bridge.session_id(),
                account_generation: app.assistant_accounts.generation(),
                task: tokio::spawn(std::future::pending()),
            });
            let pending = subscription::fixture_finish_login(
                login,
                oauth,
                "oaiapp_fixture",
                "fixture-subject",
                SCOPE,
            )
            .await;
            app.handle_ui_event(CoreEvent::ChatgptLogin {
                token,
                result: Ok(pending),
            });
        }
        let oauth = HttpServer::spawn(vec![]).await;
        let api = HttpServer::spawn(vec![]).await;
        subscription::install_test_oauth_origin(&oauth.url("/")).unwrap();
        responses::install_test_api_origin(&api.url("/")).unwrap();
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let (tx, mut rx) = mpsc::channel(8);
        app.ui_tx = Some(tx.clone());
        app.open_ai_section("model").unwrap();
        app.input = "人工甲🙂乙".into();
        app.input.move_left();
        let cursor = app.input.cursor();

        // First authorization queues exactly one catalog request, never inference.
        api.push(Scenario::new(catalog()));
        login(&mut app, &oauth, None).await;
        assert!(app.runner.connection_busy());
        assert!(app.runner.settings.api.model.is_empty());
        finish_models(&mut app, &mut rx).await;
        let panel = app.assistant_panel.as_ref().unwrap();
        assert_eq!(panel.page, Page::Models);
        let choices = panel.choices.as_ref().unwrap();
        assert!(matches!(choices.kind, ChoiceKind::ChatgptModel));
        assert_eq!(choices.values, ["First · first", "Second · second"]);
        assert_eq!(choices.selected, 0);
        assert!(
            app.runner.settings.api.model.is_empty(),
            "highlight is not selection"
        );
        let mut panel = app.assistant_panel.take().unwrap();
        app.select_assistant_choice(&mut panel, 1).unwrap();
        app.assistant_panel = Some(panel);
        let account = app.runner.settings.api.chatgpt_account.clone().unwrap();
        assert_eq!(app.runner.settings.api.model, "second");
        assert_eq!(api.requests().len(), 1);

        // Reauthorizing the same registration preserves an available chosen model.
        api.push(Scenario::new(catalog()));
        login(&mut app, &oauth, Some(&account)).await;
        assert_eq!(app.runner.settings.api.model, "second");
        assert!(app.assistant_panel.as_ref().unwrap().models.is_empty());
        assert!(app.assistant_panel.as_ref().unwrap().choices.is_none());
        finish_models(&mut app, &mut rx).await;
        assert_eq!(app.runner.settings.api.model, "second");
        assert_eq!(
            app.assistant_panel
                .as_ref()
                .unwrap()
                .choices
                .as_ref()
                .unwrap()
                .selected,
            1
        );
        assert_eq!(api.requests().len(), 2);

        // A no-longer-visible model clears only after successful catalog admission.
        api.push(Scenario::new(Response::json(
            json!({"models":[{"visibility":"list", "slug":"first", "display_name":"First"}]}),
        )));
        app.begin_chatgpt_models().unwrap();
        finish_models(&mut app, &mut rx).await;
        assert!(app.runner.settings.api.model.is_empty());
        let mut panel = app.assistant_panel.take().unwrap();
        app.select_assistant_choice(&mut panel, 0).unwrap();
        app.assistant_panel = Some(panel);

        // A directory failure retains login/selection and allows manual reread.
        api.push(Scenario::new(
            Response::json(json!({"error":{"code":"rate_limit_exceeded"}})).status(429),
        ));
        login(&mut app, &oauth, Some(&account)).await;
        finish_models(&mut app, &mut rx).await;
        assert_eq!(app.runner.settings.api.model, "first");
        assert!(
            app.runner
                .chatgpt_accounts()
                .unwrap()
                .iter()
                .any(|a| a.id == account && a.signed_in)
        );
        assert!(app.chatgpt_operation.is_none() && !app.runner.connection_busy());
        assert_eq!(api.requests().len(), 4);
        api.push(Scenario::new(catalog()));
        app.begin_chatgpt_models().unwrap();
        finish_models(&mut app, &mut rx).await;
        assert_eq!(app.runner.settings.api.model, "first");

        // Explicitly switching registrations automatically reads that account once.
        let pending = subscription::fixture_pending_login(
            &root.path().join("assistant.json"),
            &oauth,
            None,
            "oaiapp_other",
            "other-subject",
            SCOPE,
        )
        .await;
        let other =
            subscription::complete_login(&root.path().join("assistant.json"), pending).unwrap();
        let mut panel = app.assistant_panel.take().unwrap();
        app.refresh_model_panel(&mut panel);
        app.apply_assistant_row(Row::ChatgptAccount, &mut panel)
            .unwrap();
        let index = panel
            .accounts
            .iter()
            .position(|a| a.id == other.id)
            .unwrap();
        api.push(Scenario::new(catalog()));
        app.select_assistant_choice(&mut panel, index).unwrap();
        app.assistant_panel = Some(panel);
        assert!(app.runner.settings.api.model.is_empty());
        finish_models(&mut app, &mut rx).await;
        assert_eq!(api.requests().len(), 6);
        assert_eq!(
            app.runner.settings.api.chatgpt_account.as_deref(),
            Some(other.id.as_str())
        );
        assert!(app.assistant_panel.as_ref().unwrap().choices.is_some());

        // Cancelled, scene-changed and newly authorized requests cannot apply late catalogs.
        for change in ["cancel", "scene", "permission", "resume", "account"] {
            let (scenario, received, release) = Scenario::new(Response::json(json!({"models":[
                {"visibility":"list", "slug":"late", "display_name":"Late"}
            ]})))
            .barrier();
            api.push(scenario);
            app.begin_chatgpt_models().unwrap();
            let token = app.chatgpt_operation.as_ref().unwrap().token;
            tokio::time::timeout(Duration::from_secs(5), received)
                .await
                .unwrap()
                .unwrap();
            match change {
                "cancel" => app.cancel_chatgpt_operation(),
                "scene" => app.bridge.new_session(),
                "permission" => app.bridge.permission(true).unwrap(),
                "resume" => app.resume_pending = true,
                "account" => app.runner.settings.api.chatgpt_account = Some(account.clone()),
                _ => unreachable!(),
            }
            release.send(()).unwrap();
            if change == "cancel" {
                app.handle_ui_event(CoreEvent::ChatgptModels {
                    token,
                    result: Ok(vec![ModelChoice {
                        slug: "late".into(),
                        display_name: "Late".into(),
                    }]),
                });
            } else {
                finish_models(&mut app, &mut rx).await;
            }
            assert!(app.runner.settings.api.model.is_empty());
            assert!(
                !app.assistant_panel
                    .as_ref()
                    .unwrap()
                    .models
                    .iter()
                    .any(|m| m.slug == "late")
            );
            assert!(app.chatgpt_operation.is_none() && !app.runner.connection_busy());
            app.bridge.permission(false).unwrap();
            app.resume_pending = false;
            app.runner.settings.api.chatgpt_account = Some(other.id.clone());
        }
        assert_eq!(api.requests().len(), 11);
        assert!(api.requests().iter().all(|r| r.method == "GET"
            && r.path == "/v1/models"
            && r.header("authorization").is_some()));
        assert!(!app.runner.running && !app.bridge.sending_enabled());
        assert_eq!(app.bridge.candidate_count(), 0);
        assert_eq!(app.input, "人工甲🙂乙");
        assert_eq!(app.input.cursor(), cursor);
        assert!(rx.try_recv().is_err());
        app.runner.shutdown(&app.bridge).await;
        api.shutdown().await;
        oauth.shutdown().await;
    }
    #[tokio::test]
    async fn model_key_menu_owns_hidden_input_and_cancel_restores_unicode_cursor() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.input = "甲🙂乙".into();
        app.input.set_cursor(1);
        app.open_ai_section("model").unwrap();
        let index = app
            .assistant_panel
            .as_ref()
            .unwrap()
            .rows(&app)
            .iter()
            .position(|row| matches!(row, Row::ApiKey))
            .unwrap();
        app.assistant_panel.as_mut().unwrap().row = index;
        let (tx, _) = mpsc::channel(1);
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(matches!(
            app.secret_target,
            Some(SecretTarget::AssistantApiKey)
        ));
        app.handle_paste("fake-secret-menu-only");
        assert!(app.assistant_panel.as_ref().unwrap().editor.is_none());
        assert!(
            !details(&app, app.assistant_panel.as_ref().unwrap()).contains("fake-secret-menu-only")
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.secret_target.is_none());
        assert_eq!(app.input, "甲🙂乙");
        assert_eq!(app.input.cursor(), 1);
        assert!(!root.path().join("assistant-secrets/api-key.json").exists());
        assert!(!app.runner.running && !app.bridge.sending_enabled());
    }
    #[test]
    fn mouse_click_uses_rendered_list_and_does_not_activate_stale_or_hidden_rows() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_settings_section("ai").unwrap();
        let backend = ratatui::backend::TestBackend::new(90, 28);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let panel = app.assistant_panel.as_ref().unwrap();
        let (_, area, offset) = panel.menu_hit.unwrap();
        let index = panel
            .rows(&app)
            .iter()
            .position(|row| matches!(row, Row::Page(Page::Models)))
            .unwrap();
        let event = MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x,
            row: area.y + u16::try_from(index - offset).unwrap(),
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.assistant_click(event));
        assert_eq!(app.assistant_panel.as_ref().unwrap().row, index);
        assert_eq!(app.assistant_panel.as_ref().unwrap().page, Page::AiSettings);
        let mut panel = app.assistant_panel.take().unwrap();
        app.apply_assistant_row(Row::Page(Page::Models), &mut panel)
            .unwrap();
        app.assistant_panel = Some(panel);
        assert!(!app.assistant_click(event));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.secret_target = Some(SecretTarget::AssistantApiKey);
        assert!(!app.assistant_click(event));
        assert!(!app.runner.running && !app.bridge.sending_enabled());
        assert!(!root.path().join("assistant.json").exists());
    }
    #[test]
    fn mouse_choice_navigation_never_commits_connection() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_ai_section("model").unwrap();
        let mut panel = app.assistant_panel.take().unwrap();
        app.apply_assistant_row(Row::ApiProtocol, &mut panel)
            .unwrap();
        let before = app.runner.settings.api.clone();
        app.assistant_panel = Some(panel);
        assert!(app.assistant_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(
            app.assistant_panel
                .as_ref()
                .unwrap()
                .choices
                .as_ref()
                .unwrap()
                .selected,
            1
        );
        assert_eq!(app.runner.settings.api, before);
        assert!(!root.path().join("assistant.json").exists());
    }
    #[tokio::test]
    async fn invalid_api_editor_preserves_unicode_draft_and_unsaved_value() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.input = "甲🙂乙".into();
        app.input.set_cursor(1);
        app.open_ai_section("model").unwrap();
        let mut panel = app.assistant_panel.take().unwrap();
        app.apply_assistant_row(Row::Edit(Field::ApiBaseUrl), &mut panel)
            .unwrap();
        panel.editor.as_mut().unwrap().1 = "https://user:private@example.com/v1".into();
        let before = app.runner.settings.api.clone();
        app.assistant_panel = Some(panel);
        let (tx, _) = mpsc::channel(1);
        app.assistant_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await;
        let panel = app.assistant_panel.as_ref().unwrap();
        assert_eq!(
            panel.editor.as_ref().unwrap().1.to_string(),
            "https://user:private@example.com/v1"
        );
        assert!(panel.error.is_some());
        assert_eq!(app.runner.settings.api, before);
        assert_eq!(app.input, "甲🙂乙");
        assert_eq!(app.input.cursor(), 1);
        assert!(!app.runner.running && !app.bridge.sending_enabled());
    }
    #[test]
    fn preset_running_gate_changes_nothing_and_keeps_permission() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut s = app.runner.settings.clone();
        s.name = "保留".into();
        s.automatic = true;
        app.runner.save(s, &app.bridge).unwrap();
        app.bridge.permission(true).unwrap();
        let before = std::fs::read(root.path().join("assistant.json")).unwrap();
        app.runner.running = true;
        let mut p = Panel::new();
        p.page = Page::Presets;
        assert!(
            app.apply_assistant_row(Row::Preset(ReplyPreset::TextQa), &mut p)
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("assistant.json")).unwrap(),
            before
        );
        assert!(app.bridge.sending_enabled());
    }
    #[test]
    fn choosing_per_item_clears_stale_automatic_preference_without_permission() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut settings = app.runner.settings.clone();
        settings.automatic = true;
        app.runner.save(settings, &app.bridge).unwrap();
        app.bridge.permission(false).unwrap();
        let mut panel = Panel::new();
        app.apply_assistant_row(Row::Permission, &mut panel)
            .unwrap();
        app.select_assistant_choice(&mut panel, 0).unwrap();
        assert!(!app.runner.settings.automatic);
        assert!(!app.bridge.sending_enabled());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("assistant.json")).unwrap())
                .unwrap();
        assert_eq!(saved["automatic"], false);
    }
    #[tokio::test]
    async fn external_save_keeps_failed_input_and_manual_draft_until_real_ack() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.input = "甲🙂乙".into();
        app.input.move_left();
        let cursor = app.input.cursor();
        let (tx, mut rx) = mpsc::channel(4);
        app.command("/obs config port 0", tx.clone()).await.unwrap();
        app.handle_paste("999");
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_ui_event(
            tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            app.assistant_panel
                .as_ref()
                .unwrap()
                .editor
                .as_ref()
                .unwrap()
                .1
                .to_string(),
            "0"
        );
        assert!(app.assistant_panel.as_ref().unwrap().error.is_some());
        assert_eq!(app.obs.configuration().await.port, 4455);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_paste("4567");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        app.handle_ui_event(
            tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            crate::obs::ObsConfiguration::load(&root.path().join("obs.json"))
                .unwrap()
                .port,
            4567
        );
        assert!(app.assistant_panel.as_ref().unwrap().editor.is_none());
        assert_eq!(app.input, "甲🙂乙");
        assert_eq!(app.input.cursor(), cursor);
    }

    #[tokio::test]
    async fn late_settings_ack_does_not_close_another_editor() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let (tx, mut rx) = mpsc::channel(4);
        app.command("/obs config host example.invalid", tx.clone())
            .await
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        app.open_ai_section("materials").unwrap();
        let mut panel = app.assistant_panel.take().unwrap();
        app.apply_assistant_row(Row::Edit(Field::Name), &mut panel)
            .unwrap();
        app.assistant_panel = Some(panel);
        let before = app
            .assistant_panel
            .as_ref()
            .unwrap()
            .editor
            .as_ref()
            .unwrap()
            .1
            .to_string();
        app.handle_ui_event(
            tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
        let panel = app.assistant_panel.as_ref().unwrap();
        assert_eq!(panel.editor.as_ref().unwrap().1.to_string(), before);
        assert_eq!(
            crate::obs::ObsConfiguration::load(&root.path().join("obs.json"))
                .unwrap()
                .host,
            "example.invalid"
        );
    }

    #[tokio::test]
    async fn obs_menu_password_and_stop_overlays_own_keyboard_focus() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.input = "人工草稿".into();
        let (tx, _rx) = mpsc::channel(4);
        app.open_obs_settings().await.unwrap();
        for command in ["/obs config password", "/obs stop"] {
            let index = app
                .assistant_panel
                .as_ref()
                .unwrap()
                .rows(&app)
                .iter()
                .position(|r| matches!(r, Row::Command(raw) if *raw == command))
                .unwrap();
            app.assistant_panel.as_mut().unwrap().row = index;
            app.handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                tx.clone(),
            )
            .await
            .unwrap();
            if command.ends_with("password") {
                assert_eq!(app.secret_target, Some(SecretTarget::ObsPassword));
                app.handle_paste("secret-not-saved");
            } else {
                assert!(matches!(
                    app.stop_flow,
                    Some(StopFlow::Confirm {
                        stop_selected: false
                    })
                ));
            }
            app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap();
            assert!(app.secret_target.is_none() && app.stop_flow.is_none());
            assert_eq!(app.assistant_panel.as_ref().unwrap().page, Page::Obs);
            assert_eq!(app.input, "人工草稿");
        }
        assert!(!root.path().join("obs-password").exists());
    }
    #[tokio::test]
    async fn settings_archive_search_requires_a_keyword_and_cancel_preserves_the_draft() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.input = EditorInput::from("甲🙂乙");
        app.input.set_cursor(1);
        let (tx, _) = mpsc::channel(2);
        app.open_settings().unwrap();
        let search = app
            .assistant_panel
            .as_ref()
            .unwrap()
            .rows(&app)
            .iter()
            .position(|row| matches!(row, Row::Edit(Field::ArchiveSearch)))
            .unwrap();
        app.assistant_panel.as_mut().unwrap().row = search;
        app.assistant_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await;
        app.handle_paste("模型 复盘");
        app.assistant_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await;
        assert!(app.assistant_panel.as_ref().unwrap().editor.is_none());
        assert_eq!(app.input, "甲🙂乙");
        assert_eq!(app.input.cursor(), 1);

        app.assistant_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await;
        app.handle_paste("模型 复盘");
        app.assistant_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await;
        assert!(app.assistant_panel.as_ref().unwrap().editor.is_none());
        assert!(app.notice.contains("归档会话"));
        assert_eq!(app.input, "甲🙂乙");
        assert_eq!(app.input.cursor(), 1);
    }

    #[tokio::test]
    async fn settings_quit_exits_the_application() {
        let root = crate::test_support::private_tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let (tx, _) = mpsc::channel(2);
        app.open_settings().unwrap();
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
                .await
                .unwrap()
        );
    }
}
