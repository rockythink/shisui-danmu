use super::settings::ExternalField;
use super::*;
use crate::runner::{
    acp::{Change, Choice},
    hosts,
    settings::{Discovery, Host, Persona, ReplyActivity, ReplyPreset, Source},
};
use anyhow::Context;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum Page {
    #[default]
    Home,
    Settings,
    Reading,
    Account,
    Obs,
    AiSettings,
    System,
    About,
    Models,
    Assistant,
    Materials,
    Advanced,
    ReplyRange {
        scene: bool,
    },
    Themes,
    Presets,
    Agents,
    More,
    Candidates,
    Diagnostics,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Home => "AI",
            Self::Settings => "设置与操作",
            Self::Reading => "外观与显示",
            Self::Account => "B站账号与直播间",
            Self::Obs => "OBS",
            Self::AiSettings => "AI助手",
            Self::System => "系统与关于",
            Self::About => "关于弹幕台",
            Self::Models => "模型",
            Self::Assistant => "发送策略",
            Self::Materials => "人设与资料",
            Self::Advanced => "Agent配置",
            Self::ReplyRange { scene: true } => "本场范围",
            Self::ReplyRange { scene: false } => "默认范围",
            Self::Themes => "主题",
            Self::Presets => "回复方案",
            Self::Agents => "AI 工具",
            Self::More => "更多",
            Self::Candidates => "待审回复",
            Self::Diagnostics => "诊断",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    External(ExternalField),
    Binary,
    Profile,
    Prompt,
    Name,
    Preferences,
    Topic,
    BlockedWords,
    HistorySeconds,
    Workspace,
    Skills,
    ArchiveSearch,
}
impl Field {
    fn label(self) -> &'static str {
        match self {
            Self::External(field) => field.label(),
            Self::Binary => "ACP 程序",
            Self::Profile => "OMP profile",
            Self::Prompt => "规则文件",
            Self::Name => "AI 名字",
            Self::Preferences => "补充要求",
            Self::Topic => "本场主题",
            Self::BlockedWords => "屏蔽词",
            Self::HistorySeconds => "回到最新（秒）",
            Self::Workspace => "工作区路径",
            Self::Skills => "Skills",
            Self::ArchiveSearch => "搜索归档",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Run,
    Review,
    EditCandidate,
    ApproveCandidate,
    DiscardCandidate,
    NextCandidate,
    ApproveAllCandidates,
    DiscardAllCandidates,
}
#[derive(Clone, Copy, Debug)]
enum Row {
    Preset(ReplyPreset),
    Group(&'static str),
    ToggleThanks,
    AdvancedGroup(u8),
    Command(&'static str),
    DynamicHostPriority,
    DynamicMutedSupport,
    DynamicStrategy,
    Action(Action),
    Page(Page),
    Host(Host),
    Edit(Field),
    Permission,
    ReplyActivity,
    Activity {
        activity: ReplyActivity,
        scene: bool,
    },
    RestoreTopic,
    UseProfile,
    UseProject,
    ThankGifts,
    ThankLikes,
    ThankFollows,
    ThankShares,
    MentionSender,
    Persona,
    RepairBlocked,
    RepairHistory,
    WebSearch,
    DefaultSource,
    Option(usize),
    Theme(usize),
    ReloadThemes,
    Layout,
    Usernames,
    Timestamps,
    ConnectOptions,
    Login,
    Logout,
    ReuseMain,
    AssistantLogin,
    CancelAssistantLogin,
    SignOutIndependent,
    Visible,
    Context,
    Stop,
}
#[derive(Clone, Copy)]
struct HistoryEntry {
    page: Page,
    row: usize,
    scroll: u16,
    list_offset: usize,
    expanded: bool,
    thanks_expanded: bool,
    advanced_group: Option<u8>,
}
pub(super) struct Panel {
    page: Page,
    row: usize,
    editor: Option<(Field, EditorInput)>,
    choices: Option<Choices>,
    discovered: Vec<Discovery>,
    error: Option<String>,
    scroll: u16,
    history: Vec<HistoryEntry>,
    list_offset: usize,
    menu_height: usize,
    expanded: bool,
    thanks_expanded: bool,
    advanced_group: Option<u8>,
    pending_save: Option<uuid::Uuid>,
    obs_configuration: Option<crate::obs::ObsConfiguration>,
}
struct Choices {
    id: Option<String>,
    values: Vec<Choice>,
    selected: usize,
}
const SETTINGS_PAGES: [Page; 5] = [
    Page::Reading,
    Page::Account,
    Page::Obs,
    Page::AiSettings,
    Page::System,
];

impl Panel {
    fn new() -> Self {
        Self {
            page: Page::Home,
            row: 0,
            editor: None,
            choices: None,
            discovered: Vec::new(),
            error: None,
            scroll: 0,
            history: Vec::new(),
            list_offset: 0,
            menu_height: 5,
            expanded: false,
            thanks_expanded: false,
            advanced_group: None,
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
            thanks_expanded: self.thanks_expanded,
            advanced_group: self.advanced_group,
        });
        self.page = page;
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
            thanks_expanded: thanks,
            advanced_group: advanced,
        }) = self.history.pop()
        else {
            return true;
        };
        self.page = page;
        self.row = row;
        self.scroll = scroll;
        self.list_offset = offset;
        self.expanded = expanded;
        self.thanks_expanded = thanks;
        self.advanced_group = advanced;
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
    pub(super) fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    fn rows(&self, app: &TerminalApp) -> Vec<Row> {
        match self.page {
            Page::Home => vec![
                Row::Action(Action::Run),
                Row::Permission,
                Row::Page(Page::ReplyRange { scene: true }),
                Row::Edit(Field::Topic),
                Row::Action(Action::Review),
                Row::Page(Page::Settings),
            ],
            Page::Settings => vec![
                Row::Group("设置"),
                Row::Page(Page::Reading),
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
                Row::Edit(Field::Workspace),
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
            Page::Reading => vec![
                Row::Page(Page::Themes),
                Row::Layout,
                Row::Usernames,
                Row::Timestamps,
                Row::Edit(Field::HistorySeconds),
            ],
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
                let mut rows = vec![Row::Page(Page::Agents)];
                if !app.runner.report.connected {
                    rows.push(Row::ConnectOptions)
                } else {
                    rows.extend(
                        (0..app.runner.report.options.config.as_ref().map_or(
                            usize::from(app.runner.report.options.modes.is_some()),
                            Vec::len,
                        ))
                            .map(Row::Option),
                    );
                }
                rows
            }
            Page::Assistant => {
                let mut rows = vec![
                    Row::Page(Page::Presets),
                    Row::ReplyActivity,
                    Row::Edit(Field::Preferences),
                    Row::WebSearch,
                    Row::MentionSender,
                    Row::ToggleThanks,
                ];
                if self.thanks_expanded {
                    rows.extend([
                        Row::ThankGifts,
                        Row::ThankLikes,
                        Row::ThankFollows,
                        Row::ThankShares,
                    ]);
                }
                rows
            }
            Page::Materials => vec![
                Row::Edit(Field::Name),
                Row::Persona,
                Row::UseProfile,
                Row::UseProject,
            ],
            Page::Advanced => {
                let mut rows = Vec::new();
                for group in 0..3 {
                    rows.push(Row::AdvancedGroup(group));
                    if self.advanced_group == Some(group) {
                        match group {
                            0 => rows.extend([
                                Row::DefaultSource,
                                Row::Edit(Field::Profile),
                                Row::Edit(Field::Prompt),
                                Row::Edit(Field::Skills),
                                Row::Edit(Field::BlockedWords),
                                Row::RepairBlocked,
                                Row::Edit(Field::Binary),
                            ]),
                            1 => rows.extend([
                                Row::DynamicHostPriority,
                                Row::DynamicMutedSupport,
                                Row::DynamicStrategy,
                            ]),
                            _ => rows.extend([Row::Visible, Row::Context, Row::Stop]),
                        }
                    }
                }
                rows
            }
            Page::ReplyRange { scene } => [
                ReplyActivity::Cautious,
                ReplyActivity::Balanced,
                ReplyActivity::Active,
            ]
            .into_iter()
            .map(|activity| Row::Activity { activity, scene })
            .collect(),
            Page::Themes => (0..app.config.themes.entries().count())
                .map(Row::Theme)
                .chain([Row::ReloadThemes])
                .collect(),
            Page::Presets => ReplyPreset::ALL.into_iter().map(Row::Preset).collect(),
            Page::Agents => self
                .discovered
                .iter()
                .map(|entry| Row::Host(entry.host))
                .collect(),
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
impl TerminalApp {
    pub(super) fn open_settings(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate_edit.is_none()
                && !self.secret_mode
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
        Ok(())
    }
    pub(super) fn open_archive_search(&mut self) -> Result<()> {
        self.open_settings()?;
        let mut panel = self.assistant_panel.take().expect("刚打开设置");
        panel.history.clear();
        panel.editor = Some((Field::ArchiveSearch, EditorInput::default()));
        self.assistant_panel = Some(panel);
        Ok(())
    }
    pub(super) fn open_more(&mut self) -> Result<()> {
        self.open_direct(Page::More)
    }
    pub(super) fn open_ai_range(&mut self) -> Result<()> {
        self.open_assistant()?;
        let p = self.assistant_panel.as_mut().unwrap();
        p.page = Page::ReplyRange { scene: true };
        p.history.clear();
        Ok(())
    }
    pub(super) fn open_ai_topic(&mut self) -> Result<()> {
        self.open_assistant()?;
        let mut p = self.assistant_panel.take().unwrap();
        p.page = Page::Home;
        p.history.clear();
        self.apply_assistant_row(Row::Edit(Field::Topic), &mut p)?;
        self.assistant_panel = Some(p);
        Ok(())
    }
    pub(super) fn open_assistant(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.candidate_edit.is_none()
                && !self.secret_mode
                && self.stop_flow.is_none()
                && self.login_qr.is_none(),
            "请先完成当前编辑或弹窗；人工草稿保留"
        );
        self.runner.tick(&self.bridge);
        self.sync_assistant_room();
        self.assistant_panel = Some(Panel::new());
        Ok(())
    }
    pub(super) fn open_diagnostics(&mut self) -> Result<()> {
        self.open_direct(Page::Diagnostics)
    }
    pub(super) fn open_settings_section(&mut self, section: &str) -> Result<()> {
        let target = match section {
            "account" => Page::Account,
            "obs" => Page::Obs,
            "ai" => Page::AiSettings,
            "system" => Page::System,
            "reading" => Page::Reading,
            _ => anyhow::bail!("未知设置分类"),
        };
        self.open_direct(target)
    }
    pub(super) fn open_ai_section(&mut self, section: &str) -> Result<()> {
        self.open_direct(match section {
            "model" => Page::Models,
            "replies" => Page::Assistant,
            "materials" => Page::Materials,
            "advanced" => Page::Advanced,
            _ => anyhow::bail!("未知AI设置"),
        })
    }
    pub(super) fn open_about(&mut self) -> Result<()> {
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
    pub(super) async fn open_external_editor(
        &mut self,
        field: ExternalField,
        value: Option<&str>,
        tx: mpsc::Sender<UiEvent>,
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
    pub(super) fn edit_workspace(
        &mut self,
        value: Option<&str>,
        tx: mpsc::Sender<UiEvent>,
    ) -> Result<()> {
        self.open_direct(Page::AiSettings)?;
        let mut panel = self.assistant_panel.take().unwrap();
        let result = self
            .apply_assistant_row(Row::Edit(Field::Workspace), &mut panel)
            .and_then(|_| {
                if let Some(value) = value {
                    panel.editor = Some((Field::Workspace, EditorInput::from(value.to_owned())));
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
    pub(super) async fn open_obs_settings(&mut self) -> Result<()> {
        self.open_direct(Page::Obs)?;
        self.assistant_panel.as_mut().unwrap().obs_configuration =
            Some(self.obs.configuration().await);
        Ok(())
    }
    pub(super) fn finish_setting_save(
        &mut self,
        token: uuid::Uuid,
        result: std::result::Result<settings::SavedSetting, String>,
    ) {
        if self.settings_operation != Some(token) {
            return;
        }
        self.settings_operation = None;
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
        match result {
            Ok(saved) => self.set_notice(saved.message, saved.level),
            Err(error) => self.set_notice(format!("保存失败：{error}"), NoticeLevel::Error),
        }
    }
    pub(super) fn configure_display(&mut self, setting: &str, value: &str) -> Result<()> {
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
    pub(super) fn open_theme_settings(&mut self) -> Result<()> {
        self.open_direct(Page::Themes)
    }
    pub(super) fn open_candidate_review(&mut self) -> Result<()> {
        self.open_direct(Page::Candidates)
    }

    pub(super) fn paste_assistant(&mut self, text: &str) {
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
    pub(super) fn start_assistant(&mut self, visible: bool, rebuild: bool) -> Result<()> {
        self.resume_pending = false;
        let rebuild = rebuild || self.runner.needs_rebuild;
        if self.runner.running && !rebuild {
            return Ok(());
        }
        if !self.runner.has_saved_settings() {
            let discovered: Vec<_> = Host::ALL.into_iter().map(Host::discovery).collect();
            let available: Vec<_> = discovered
                .iter()
                .filter_map(|e| e.program.clone().map(|p| (e.host, p)))
                .collect();
            if available.len() != 1 {
                self.open_direct(Page::Agents)?;
                let panel = self.assistant_panel.as_mut().unwrap();
                panel.discovered = discovered;
                panel.error = Some(
                    if available.is_empty() {
                        "未发现可用 AI 工具；可安装适配器或指定 ACP 程序"
                    } else {
                        "发现多个 AI 工具，请明确选择"
                    }
                    .into(),
                );
                return Ok(());
            }
            let mut s = self.runner.settings.clone();
            if s.binary.as_os_str().is_empty() {
                s.host = available[0].0;
                s.binary = available[0].1.clone()
            }
            self.runner.save(s, &self.bridge, false)?
        }
        if let Err(error) = self.runner.settings.program() {
            self.open_direct(Page::Models)?;
            self.assistant_panel.as_mut().expect("模型页已打开").error =
                Some(format!("AI 工具未就绪：{error}"));
            return Ok(());
        }
        self.runner.start(&self.bridge, visible, rebuild)?;
        self.set_notice("AI启动请求已提交", NoticeLevel::Info);
        Ok(())
    }
    pub(super) fn stop_assistant(&mut self) {
        self.pause_assistant();
        self.runner.stop(&self.bridge);
    }
    fn apply_assistant_row(&mut self, row: Row, panel: &mut Panel) -> Result<bool> {
        if let Some(reason) = readonly_reason(row, self) {
            anyhow::bail!("只读：{reason}")
        }
        match row {
            Row::Group(_) | Row::DynamicStrategy => {}
            Row::ToggleThanks => {
                panel.thanks_expanded = !panel.thanks_expanded;
                panel.row = panel.row.min(panel.rows(self).len().saturating_sub(1));
            }
            Row::AdvancedGroup(group) => {
                panel.advanced_group = if panel.advanced_group == Some(group) {
                    None
                } else {
                    Some(group)
                };
                panel.row = panel
                    .rows(self)
                    .iter()
                    .position(|r| matches!(r,Row::AdvancedGroup(g) if *g==group))
                    .unwrap_or(0);
            }
            Row::Command(_) => anyhow::bail!("指令须经统一执行入口"),
            Row::Preset(preset) => {
                anyhow::ensure!(!self.runner.running, "请先暂停 AI");
                anyhow::ensure!(!self.runner.in_flight(), "等待当前回复结束后再换方案");
                let mut s = self.runner.settings.clone();
                preset.apply(&mut s);
                if s != self.runner.settings {
                    self.runner.save(s, &self.bridge, false)?;
                }
                panel.back();
            }
            Row::Page(page) => {
                if page == Page::Agents && panel.discovered.is_empty() {
                    panel.discovered = Host::ALL.into_iter().map(Host::discovery).collect()
                }
                panel.navigate(page);
                if panel
                    .rows(self)
                    .first()
                    .is_some_and(|row| readonly_reason(*row, self).is_some())
                {
                    self.move_assistant_selection(panel, true);
                }
                if let Page::ReplyRange { scene } = page {
                    let current = if scene {
                        self.runner.effective_reply_activity()
                    } else {
                        self.runner.settings.reply_activity
                    };
                    panel.row = panel
                        .rows(self)
                        .iter()
                        .position(|r| matches!(r,Row::Activity{activity,..} if *activity==current))
                        .unwrap_or(0)
                }
            }
            Row::Host(host) => {
                if host == self.runner.settings.host
                    && !self.runner.settings.binary.as_os_str().is_empty()
                {
                    if !self.runner.has_saved_settings() {
                        self.runner
                            .save(self.runner.settings.clone(), &self.bridge, true)?
                    }
                } else {
                    let mut s = self.runner.settings.clone();
                    s.host = host;
                    s.binary = panel
                        .discovered
                        .iter()
                        .find(|e| e.host == host)
                        .and_then(|e| e.program.clone())
                        .unwrap_or_default();
                    s.source = Source::Default;
                    self.runner.save(s, &self.bridge, true)?
                }
                panel.back();
            }
            Row::Edit(field) => {
                let s = &self.runner.settings;
                let value = match field {
                    Field::ArchiveSearch => String::new(),
                    Field::External(field) => self.external_setting_value(field, panel),
                    Field::Binary => s.binary.display().to_string(),
                    Field::HistorySeconds => self.config.history_idle_seconds.to_string(),
                    Field::Workspace => self.runner.workspace_path()?.display().to_string(),
                    Field::Skills => s.skills.join("，"),
                    Field::Name => s.name.clone(),
                    Field::Preferences => s.preferences.clone(),
                    Field::BlockedWords => s.blocked_words.join("，"),
                    Field::Topic => self
                        .runner
                        .topic_override()
                        .unwrap_or(self.runner.effective_topic())
                        .to_owned(),
                    Field::Profile => {
                        if let Source::OmpProfile(v) = &s.source {
                            v.clone()
                        } else {
                            String::new()
                        }
                    }
                    Field::Prompt => {
                        if let Source::NativePrompt(v) = &s.source {
                            v.display().to_string()
                        } else {
                            String::new()
                        }
                    }
                };
                panel.editor = Some((field, EditorInput::from(value)));
            }
            Row::Layout => {
                self.configure_display("layout", if self.layout_chat { "list" } else { "chat" })?
            }
            Row::Usernames => {
                self.configure_display("names", if self.show_name { "off" } else { "on" })?
            }
            Row::Timestamps => {
                self.configure_display("time", if self.show_time { "off" } else { "on" })?
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
                anyhow::ensure!(self.local_transport.is_none(), "本地模式禁止账号操作");
                if matches!(row, Row::Logout) {
                    self.cancel_main_login();
                    let forgotten = self.assistant_accounts.main_changed();
                    self.bridge.identity_changed();
                    self.runner.pause(&self.bridge);
                    self.review_frame = None;
                    self.account.sign_out()?;
                    self.account_status = AccountStatus::SignedOut;
                    forgotten?
                }
            }
            Row::ReuseMain | Row::SignOutIndependent => {
                self.bridge.identity_changed();
                self.runner.pause(&self.bridge);
                self.review_frame = None;
                match row {
                    Row::ReuseMain => self.assistant_accounts.reuse_main()?,
                    Row::SignOutIndependent => self.assistant_accounts.sign_out_independent()?,
                    _ => unreachable!(),
                }
            }
            Row::AssistantLogin => {
                anyhow::ensure!(self.local_transport.is_none(), "本地模式禁止账号操作");
                self.bridge.identity_changed();
                self.runner.pause(&self.bridge);
                self.review_frame = None
            }
            Row::CancelAssistantLogin => self.assistant_accounts.cancel_login(),
            Row::Permission => {
                let on = Choice {
                    value: true.into(),
                    name: "自动发送".into(),
                };
                let off = Choice {
                    value: false.into(),
                    name: "逐条发送".into(),
                };
                panel.choices = Some(Choices {
                    id: Some("__sending__".into()),
                    values: vec![off, on],
                    selected: usize::from(self.runner.settings.automatic),
                })
            }
            Row::ReplyActivity => {
                return self
                    .apply_assistant_row(Row::Page(Page::ReplyRange { scene: false }), panel);
            }
            Row::Activity { activity, scene } => {
                if scene {
                    if activity != self.runner.effective_reply_activity() {
                        self.runner.set_reply_activity_override(activity)
                    }
                } else if activity != self.runner.settings.reply_activity {
                    let mut s = self.runner.settings.clone();
                    s.reply_activity = activity;
                    self.runner.save(s, &self.bridge, false)?
                }
                panel.back();
            }
            Row::Persona => {
                let mut s = self.runner.settings.clone();
                s.persona = match s.persona {
                    Persona::Assistant => Persona::Broadcaster,
                    Persona::Broadcaster => Persona::Assistant,
                };
                self.runner.save(s, &self.bridge, false)?
            }
            Row::RestoreTopic => self.runner.set_topic_override(String::new())?,
            Row::WebSearch => {
                let mut s = self.runner.settings.clone();
                anyhow::ensure!(
                    s.web_search || hosts::supports_search(s.host),
                    "该工具不支持联网搜索"
                );
                s.web_search = !s.web_search;
                self.runner.save(s, &self.bridge, true)?
            }
            Row::UseProfile
            | Row::UseProject
            | Row::ThankGifts
            | Row::ThankLikes
            | Row::ThankFollows
            | Row::ThankShares
            | Row::MentionSender
            | Row::RepairBlocked
            | Row::DynamicHostPriority
            | Row::DynamicMutedSupport => {
                let mut s = self.runner.settings.clone();
                let enabled = match row {
                    Row::UseProfile => &mut s.use_profile,
                    Row::UseProject => &mut s.use_project,
                    Row::ThankGifts => &mut s.thank_gifts,
                    Row::ThankLikes => &mut s.thank_likes,
                    Row::ThankFollows => &mut s.thank_follows,
                    Row::ThankShares => &mut s.thank_shares,
                    Row::MentionSender => &mut s.mention_sender,
                    Row::RepairBlocked => &mut s.repair_blocked,
                    Row::DynamicHostPriority => &mut s.dynamic_host_priority,
                    Row::DynamicMutedSupport => &mut s.dynamic_muted_support,
                    _ => unreachable!(),
                };
                *enabled = !*enabled;
                self.runner.save(s, &self.bridge, false)?
            }
            Row::DefaultSource => {
                if self.runner.settings.source != Source::Default {
                    let mut s = self.runner.settings.clone();
                    s.source = Source::Default;
                    self.runner.save(s, &self.bridge, true)?;
                }
            }
            Row::ConnectOptions => self.runner.connect_options()?,
            Row::Option(index) => {
                anyhow::ensure!(
                    !self.runner.configuring && self.runner.pending.is_none(),
                    "等待上一配置回执"
                );
                let (id, values, current) =
                    if let Some(options) = &self.runner.report.options.config {
                        let o = options.get(index).context("选项已刷新")?;
                        anyhow::ensure!(!o.unsupported, "未知选项类型");
                        (Some(o.id.clone()), o.choices.clone(), o.current.clone())
                    } else {
                        let (current, choices) = self
                            .runner
                            .report
                            .options
                            .modes
                            .as_ref()
                            .context("未报告可选项")?;
                        (
                            None,
                            choices.clone(),
                            crate::runner::acp::SettingValue::value_id(current.clone()),
                        )
                    };
                anyhow::ensure!(!values.is_empty(), "未提供可选值");
                let selected = values.iter().position(|v| v.value == current).unwrap_or(0);
                panel.choices = Some(Choices {
                    id,
                    values,
                    selected,
                })
            }
            Row::Action(Action::Run) => {
                if self.runner.running || self.resume_pending {
                    self.pause_assistant()
                } else {
                    self.start_assistant(false, false)?
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
            Row::Visible => self.start_assistant(true, true)?,
            Row::Context => self.start_assistant(false, true)?,
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
        tx: mpsc::Sender<UiEvent>,
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
        if field == Field::HistorySeconds {
            let seconds: u32 = value.trim().parse().context("请输入非负整数秒数")?;
            self.config
                .save_value("history_idle_seconds", i64::from(seconds).into())?;
            self.config.history_idle_seconds = seconds;
            self.record_reading_config();
            self.last_user_activity = Instant::now()
        } else if field == Field::Workspace {
            self.runner
                .set_workspace(value.trim().into(), &self.bridge)?
        } else if field == Field::Topic {
            self.runner.set_topic_override(value)?
        } else {
            let mut s = self.runner.settings.clone();
            match field {
                Field::Binary => s.binary = value.trim().into(),
                Field::Profile => s.source = Source::OmpProfile(value.trim().into()),
                Field::Prompt => s.source = Source::NativePrompt(value.trim().into()),
                Field::Name => s.name = value,
                Field::Preferences => s.preferences = value,
                Field::Skills => {
                    s.skills = value
                        .split([',', '，', ';', '；', '\n'])
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .collect()
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
            self.runner.save(s, &self.bridge, true)?
        }
        panel.editor = None;
        panel.error = None;
        self.set_notice("已保存", NoticeLevel::Success);
        Ok(())
    }
    fn select_assistant_choice(&mut self, panel: &mut Panel, index: usize) -> Result<()> {
        let choices = panel.choices.as_ref().context("选项已关闭")?;
        let value = choices
            .values
            .get(index)
            .context("选项已刷新")?
            .value
            .clone();
        let id = choices.id.clone();
        if id.as_deref() == Some("__sending__") {
            let allow = value == true.into();
            if allow != self.bridge.sending_enabled() || allow != self.runner.settings.automatic {
                self.set_automatic_permission(allow)?
            }
            panel.choices = None;
            return Ok(());
        }
        if choice_is_current(self, id.as_deref(), &value) {
            panel.choices = None;
            return Ok(());
        }
        let new_context = hosts::permit_option(self.runner.settings.host, id.as_deref(), &value)?;
        self.runner.change(Change {
            id,
            value,
            new_context,
        })?;
        panel.choices = None;
        Ok(())
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
        tx: mpsc::Sender<UiEvent>,
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
        if matches!(row, Row::AssistantLogin) {
            self.cancel_main_login();
            self.assistant_accounts.start_login(tx.clone())
        }
        if matches!(row, Row::Login) {
            self.set_notice("正在创建 B 站登录二维码…", NoticeLevel::Progress);
            self.start_login(tx)
        }
        Ok(close)
    }

    pub(super) async fn assistant_key(&mut self, key: KeyEvent, tx: mpsc::Sender<UiEvent>) -> bool {
        if self.candidate_edit.is_some() {
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
                    panel.thanks_expanded = false;
                    panel.advanced_group = None;
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
                    panel.thanks_expanded = false;
                    panel.advanced_group = None;
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
    pub(super) fn assistant_mouse(&mut self, event: MouseEvent) -> bool {
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

// Startup includes the pre-connection identity check, not just the ACP process.
pub(super) fn compact(app: &TerminalApp, width: u16) -> Span<'static> {
    if (!app.runner.running && !app.resume_pending) || width == 0 {
        return Span::raw("");
    }
    if app.resume_pending || !app.runner.report.connected || app.runner.report.session.is_none() {
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
pub(super) fn run_label(app: &TerminalApp) -> &'static str {
    if app.runner.running || app.resume_pending {
        "暂停 AI"
    } else if app.runner.has_context() {
        "继续 AI"
    } else {
        "启动 AI"
    }
}
pub(super) fn policy_label(app: &TerminalApp) -> &'static str {
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

fn account_label(status: &AccountStatus) -> String {
    match status {
        AccountStatus::SignedOut => "未登录".into(),
        AccountStatus::SignedIn {
            display_name,
            user_id,
        } => format!("{display_name} · UID {user_id}"),
    }
}

fn workspace_status(app: &TerminalApp) -> &'static str {
    use crate::runner::WorkspaceContextStatus;
    match app.runner.workspace_context_status() {
        WorkspaceContextStatus::Unread => "尚未读取",
        WorkspaceContextStatus::Pending => "待下轮载入",
        WorkspaceContextStatus::Loaded => "本轮已载入",
        WorkspaceContextStatus::Error(_) => "资料读取失败",
    }
}

fn readonly_reason(row: Row, app: &TerminalApp) -> Option<String> {
    let s = &app.runner.settings;
    match row {
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
        Row::Group(_) | Row::DynamicStrategy => Some("只读信息".into()),
        Row::Edit(Field::Profile | Field::Prompt) if s.host != Host::Omp => {
            Some("当前工具不支持此规则来源".into())
        }
        Row::RepairHistory if app.runner.history_repair_running() => Some("重建中".into()),
        Row::WebSearch if !s.web_search && !hosts::supports_search(s.host) => {
            Some("当前工具不支持联网搜索".into())
        }
        Row::Login | Row::Logout | Row::AssistantLogin if app.local_transport.is_some() => {
            Some("本地模式禁止真实账号操作".into())
        }
        Row::CancelAssistantLogin if !app.assistant_accounts.login_pending() => {
            Some("没有待取消的扫码".into())
        }
        Row::Preset(_) if app.runner.running => Some("请先暂停 AI".into()),
        Row::Preset(_) if app.runner.in_flight() => Some("等待当前回复结束".into()),
        Row::Option(_) if app.runner.pending.is_some() || app.runner.configuring => {
            Some("设置中".into())
        }
        _ => None,
    }
}

fn row_columns(row: Row, app: &TerminalApp, panel: &Panel) -> (String, String) {
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
        Row::ToggleThanks => (
            "互动感谢".into(),
            if panel.thanks_expanded {
                "收起"
            } else {
                "展开"
            }
            .into(),
        ),
        Row::AdvancedGroup(0) => (
            "规则".into(),
            if panel.advanced_group == Some(0) {
                "收起"
            } else {
                "展开"
            }
            .into(),
        ),
        Row::AdvancedGroup(1) => (
            "运行策略".into(),
            if panel.advanced_group == Some(1) {
                "收起"
            } else {
                "展开"
            }
            .into(),
        ),
        Row::AdvancedGroup(_) => (
            "维护".into(),
            if panel.advanced_group == Some(2) {
                "收起"
            } else {
                "展开"
            }
            .into(),
        ),
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
        Row::Page(Page::Reading) => ("外观与显示".into(), "主题、布局、阅读".into()),
        Row::Page(Page::Account) if panel.page == Page::AiSettings => (
            "发送账号".into(),
            app.assistant_accounts.label(&app.account_status),
        ),
        Row::Page(Page::Account) => ("B站账号与直播间".into(), "账号、标题、封面".into()),
        Row::Page(Page::Obs) => ("OBS".into(), "连接配置与当前控制".into()),
        Row::Page(Page::AiSettings) => ("AI助手".into(), "工作空间、Agent、发送策略".into()),
        Row::Page(Page::System) => ("系统与关于".into(), "工具信息、诊断与修复".into()),
        Row::Page(Page::Home) => ("本场 AI".into(), policy_label(app).into()),
        Row::Edit(Field::External(field)) => (
            field.label().into(),
            app.external_setting_value(field, panel),
        ),
        Row::Edit(Field::ArchiveSearch) => ("搜索归档".into(), "输入关键词".into()),
        Row::Page(Page::Models) => ("模型".into(), configuration_status(app).into()),
        Row::Page(Page::Assistant) => ("发送策略".into(), "默认范围、互动与回复".into()),
        Row::Page(Page::Materials) => ("人设与资料".into(), "名字、身份与上下文".into()),
        Row::Page(Page::Advanced) => ("Agent配置".into(), "规则、运行策略与维护".into()),
        Row::Page(Page::Themes) => ("主题".into(), app.config.theme_name.clone()),
        Row::Page(Page::Agents) => ("AI 工具".into(), s.host.label().into()),
        Row::Page(Page::Presets) => ("方案".into(), "选择后立即保存".into()),
        Row::Page(Page::ReplyRange { scene }) => (
            if scene {
                "本场范围"
            } else {
                "默认范围"
            }
            .into(),
            if scene {
                app.runner.effective_reply_activity()
            } else {
                s.reply_activity
            }
            .label()
            .into(),
        ),
        Row::Page(page) => (page.title().into(), "打开".into()),
        Row::Preset(v) => (v.label().into(), String::new()),
        Row::DynamicHostPriority => (
            "开麦主播优先".into(),
            on_off(s.dynamic_host_priority).into(),
        ),
        Row::DynamicMutedSupport => (
            "静音文字补位".into(),
            on_off(s.dynamic_muted_support).into(),
        ),
        Row::DynamicStrategy => (
            "当前策略".into(),
            app.runner.dynamic_reply_strategy().label().into(),
        ),
        Row::Edit(Field::Workspace) => (
            "工作区".into(),
            self_or(
                app.runner
                    .workspace_path()
                    .ok()
                    .map(|p| p.display().to_string()),
                "未设置",
            ),
        ),
        Row::Edit(Field::Name) => ("AI 名字".into(), s.name.clone()),
        Row::Edit(Field::Preferences) => (
            "补充要求".into(),
            if s.preferences.is_empty() {
                "默认".into()
            } else {
                s.preferences.clone()
            },
        ),
        Row::Edit(Field::Topic) => (
            "本场主题".into(),
            if app.runner.effective_topic().is_empty() {
                "未设置".into()
            } else {
                app.runner.effective_topic().into()
            },
        ),
        Row::Edit(Field::HistorySeconds) => (
            "回到最新".into(),
            if app.config.history_idle_seconds == 0 {
                "关闭".into()
            } else {
                format!("{} 秒", app.config.history_idle_seconds)
            },
        ),
        Row::Edit(Field::Profile) => (
            "OMP profile".into(),
            if let Source::OmpProfile(v) = &s.source {
                v.clone()
            } else {
                "未选用".into()
            },
        ),
        Row::Edit(Field::Prompt) => (
            "规则文件".into(),
            if let Source::NativePrompt(v) = &s.source {
                v.display().to_string()
            } else {
                "未选用".into()
            },
        ),
        Row::Edit(Field::Skills) => (
            "Skills".into(),
            if s.skills.is_empty() {
                "未设置".into()
            } else {
                s.skills.join("，")
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
        Row::Edit(Field::Binary) => (
            "ACP 程序".into(),
            if s.binary.as_os_str().is_empty() {
                "自动发现".into()
            } else {
                s.binary.display().to_string()
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
        Row::ReplyActivity => ("默认范围".into(), s.reply_activity.label().into()),
        Row::Activity { activity, scene } => (
            activity.label().into(),
            if activity
                == if scene {
                    app.runner.effective_reply_activity()
                } else {
                    s.reply_activity
                }
            {
                "当前"
            } else {
                "选择"
            }
            .into(),
        ),
        Row::RestoreTopic => ("用直播标题".into(), String::new()),
        Row::UseProfile => ("主播简介".into(), on_off(s.use_profile).into()),
        Row::UseProject => ("项目资料".into(), on_off(s.use_project).into()),
        Row::ThankGifts => ("礼物与上舰".into(), on_off(s.thank_gifts).into()),
        Row::ThankLikes => ("点赞".into(), on_off(s.thank_likes).into()),
        Row::ThankFollows => ("关注".into(), on_off(s.thank_follows).into()),
        Row::ThankShares => ("分享".into(), on_off(s.thank_shares).into()),
        Row::MentionSender => ("问答时 @ 对方".into(), on_off(s.mention_sender).into()),
        Row::Persona => ("人设".into(), s.persona.label().into()),
        Row::RepairBlocked => ("拒绝时改写".into(), on_off(s.repair_blocked).into()),
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
        Row::DefaultSource => ("默认规则".into(), "使用工具默认值".into()),
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
        Row::Layout => (
            "布局".into(),
            if app.layout_chat { "聊天" } else { "列表" }.into(),
        ),
        Row::Usernames => ("昵称".into(), on_off(app.show_name).into()),
        Row::Timestamps => ("时间".into(), on_off(app.show_time).into()),
        Row::ConnectOptions => (
            "读取模型".into(),
            if app.runner.configuring {
                "读取中"
            } else {
                "未连接"
            }
            .into(),
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
        Row::Host(h) => (
            h.label().into(),
            if h == s.host { "当前" } else { "选择" }.into(),
        ),
        Row::Option(i) => {
            if let Some(o) = app
                .runner
                .report
                .options
                .config
                .as_ref()
                .and_then(|v| v.get(i))
            {
                (
                    o.name.clone(),
                    o.choices
                        .iter()
                        .find(|v| v.value == o.current)
                        .map(|v| v.name.clone())
                        .unwrap_or_else(|| crate::runner::acp::value_label(&o.current)),
                )
            } else {
                (
                    "运行模式".into(),
                    app.runner
                        .report
                        .options
                        .modes
                        .as_ref()
                        .map(|v| v.0.clone())
                        .unwrap_or_else(|| "继承".into()),
                )
            }
        }
        Row::Visible => ("处理可见弹幕".into(), "重建上下文".into()),
        Row::Context => ("重建上下文".into(), "从后续弹幕开始".into()),
        Row::Stop => ("结束 AI".into(), "清除续用与发送权".into()),
    }
}
fn self_or(v: Option<String>, fallback: &str) -> String {
    v.unwrap_or_else(|| fallback.into())
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

fn choice_is_current(
    app: &TerminalApp,
    id: Option<&str>,
    selected: &crate::runner::acp::SettingValue,
) -> bool {
    if let Some(id) = id {
        app.runner
            .report
            .options
            .config
            .as_ref()
            .and_then(|options| options.iter().find(|o| o.id == id))
            .is_some_and(|o| o.current == *selected)
    } else {
        app.runner.report.options.modes.as_ref().is_some_and(|(current, _)| {
            matches!(selected, crate::runner::acp::SettingValue::ValueId { value } if value.0.as_ref() == current.as_str())
        })
    }
}
fn configuration_status(app: &TerminalApp) -> &'static str {
    if app.runner.pending.is_some() {
        "设置等待生效"
    } else if app.runner.configuring {
        "等待AI工具确认设置"
    } else if !app.runner.report.connected {
        "尚未读取模型设置"
    } else {
        "显示AI工具已确认的设置"
    }
}
fn selected_details(app: &TerminalApp, panel: &Panel) -> String {
    let Some(row) = panel.rows(app).get(panel.row).copied() else {
        return if panel.page == Page::Diagnostics {
            diagnostics_details(app)
        } else {
            "暂无可操作项".into()
        };
    };
    match row {
        Row::ToggleThanks => "关注、礼物与上舰、点赞、分享各有3句个体与3句集体文案轮换。每批收集5秒：不超过5名不同观众逐人原生@，更多时集体感谢；全局至少15秒，每批完成后同类再冷却（关注30秒、礼物与上舰15秒、点赞与分享60秒）。已开始批次冻结，仍保留过期、审核与本场授权。".into(),
        Row::ThankGifts => "礼物与上舰由代码在个体/集体各3句文案中轮换：5秒收集；不超过5名不同观众时逐人原生@，更多时发集体文案；全局15秒，每批完成后同类冷却15秒。已开始批次不被后来事件改变，仍保留过期、审核与本场授权。".into(),
        Row::ThankFollows => "关注由代码在个体/集体各3句文案中轮换：5秒收集；不超过5名不同观众时逐人原生@，更多时发集体文案；全局15秒，每批完成后同类冷却30秒。已开始批次不被后来事件改变，仍保留过期、审核与本场授权。".into(),
        Row::ThankLikes => "点赞由代码在个体/集体各3句文案中轮换：5秒收集；不超过5名不同观众时逐人原生@，更多时发集体文案；全局15秒，每批完成后同类冷却60秒。已开始批次不被后来事件改变，仍保留过期、审核与本场授权。".into(),
        Row::ThankShares => "分享由代码在个体/集体各3句文案中轮换：5秒收集；不超过5名不同观众时逐人原生@，更多时发集体文案；全局15秒，每批完成后同类冷却60秒。已开始批次不被后来事件改变，仍保留过期、审核与本场授权。".into(),
        Row::MentionSender => "只控制普通问答是否原生@提问者；互动感谢按人数规则独立决定是否逐人@，不受此开关影响。".into(),
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
        Row::Edit(Field::Workspace) => format!(
            "资料状态：{}\n{}\n{}",
            workspace_status(app),
            app.runner
                .workspace_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|e| e.to_string()),
            app.runner.workspace_context_note()
        ),
        Row::UseProfile => app.profile_summary(),
        Row::Host(host) => {
            let found = panel.discovered.iter().find(|e| e.host == host);
            format!(
                "{}\nACP：{}\n原生 CLI：{}",
                host.description(),
                found
                    .and_then(|e| e.program.as_ref())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "未发现".into()),
                found
                    .and_then(|e| e.native.as_ref())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "未发现".into())
            )
        }
        Row::DefaultSource | Row::Edit(Field::Profile | Field::Prompt) => {
            app.runner.settings.source.description()
        }
        Row::DynamicStrategy => "由 OBS 麦克风状态与两个开关决定；不接收或识别语音。".into(),
        Row::RepairHistory => format!(
            "先备份派生历史副本，再从私有原件重建；原件不变。{}",
            app.runner
                .history_repair_result()
                .map(|v| format!("\n上次结果：{v}"))
                .unwrap_or_default()
        ),
        Row::Visible => "新建上下文并处理当前可见弹幕；可能消耗模型额度，不开启自动发送。".into(),
        Row::Context => "丢弃旧上下文，从后续弹幕重建；不开启自动发送。".into(),
        Row::Stop => "结束专用进程，清除续用意图和自动发送权；候选与人工草稿保留。".into(),
        Row::Option(index) => {
            if let Some(o) = app
                .runner
                .report
                .options
                .config
                .as_ref()
                .and_then(|v| v.get(index))
            {
                format!(
                    "原生选项 {}（{}）。当前值：{}。未知或受宿主策略禁止的值保持只读；修改后等待原生 ACK。",
                    o.name,
                    o.id,
                    crate::runner::acp::value_label(&o.current)
                )
            } else {
                "由 AI 工具原生报告的运行模式；修改后等待原生 ACK，未报告时保持继承。".into()
            }
        }
        _ => {
            let (name, value) = row_columns(row, app, panel);
            format!("{name}：{value}")
        }
    }
}
fn diagnostics_details(app: &TerminalApp) -> String {
    let room = app
        .runner
        .room_context()
        .map(|r| {
            format!(
                "房间 {} · 主播 {} · 标题 {}",
                r.room_id,
                r.broadcaster_id,
                if r.title.is_empty() {
                    "未获取"
                } else {
                    &r.title
                }
            )
        })
        .unwrap_or_else(|| "房间资料未建立".into());
    let routing_status = app.bridge.routing_snapshot();
    let routing = &routing_status["queue"];
    format!(
        "B站连接：{}\n主账号：{}\nOBS：{}\n配置：{}\n会话日志：{}\n\nAI状态：{} {}\n当前原因：{}\n上轮结果：{}\n场次上下文：{}\n原生会话：{}\n{}\n工作区：{}\n历史重建：{}\n\n消息分流：模型待处理 {} / 128 · 致谢组 {}\n已交模型 {} · 代码致谢 {} · 合并 {}\n去重 {} · 过期 {} · 容量淘汰 {} · 跳过 {}\n点赞 {} · 分享 {}；分流明细见本场日志",
        app.connection,
        account_label(&app.account_status),
        app.obs_status
            .as_ref()
            .map(|s| format!(
                "场景 {} · 推流 {:?} · 麦克风 {:?}",
                s.current_scene, s.stream, s.microphone
            ))
            .unwrap_or_else(|| app.obs_error.clone().unwrap_or_else(|| "未连接".into())),
        app.config.config_path.display(),
        app.journal.root().display(),
        app.runner.marker(),
        app.runner.state(),
        app.runner.note,
        app.runner.last_round_summary,
        app.runner.scene_id(),
        app.runner.native_id(),
        room,
        app.runner
            .workspace_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| e.to_string()),
        app.runner.history_repair_result().unwrap_or("尚未运行"),
        routing["pending_models"].as_u64().unwrap_or(0),
        routing["pending_templates"].as_u64().unwrap_or(0),
        routing["model_dispatched"].as_u64().unwrap_or(0),
        routing["template_dispatched"].as_u64().unwrap_or(0),
        routing["merged"].as_u64().unwrap_or(0),
        routing["deduplicated"].as_u64().unwrap_or(0),
        routing["expired"].as_u64().unwrap_or(0),
        routing["overflow"].as_u64().unwrap_or(0),
        routing["skipped"].as_u64().unwrap_or(0),
        routing["likes"].as_u64().unwrap_or(0),
        routing["shares"].as_u64().unwrap_or(0)
    )
}
fn details(app: &TerminalApp, panel: &Panel) -> String {
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
        return if c.id.as_deref() == Some("__sending__") {
            format!(
                "发送账号：{} · 房间 {}。选择自动发送会公开发送并记住本房间与账号；选择逐条立即撤权。",
                app.assistant_accounts.label(&app.account_status),
                app.session.room_id
            )
        } else {
            "选中后提交给 AI 工具；收到原生 ACK 前显示当前值。未知选项保持只读。".into()
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
fn row_danger(row: Row, app: &TerminalApp) -> bool {
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
            | Row::Context
            | Row::Stop
            | Row::RepairHistory
            | Row::Logout
            | Row::SignOutIndependent
            | Row::Edit(Field::External(
                ExternalField::RoomTitle | ExternalField::RoomCover | ExternalField::ObsScene
            ))
    ) || matches!(row, Row::Permission) && app.bridge.sending_enabled()
}

pub(super) fn draw(frame: &mut ratatui::Frame, app: &mut TerminalApp) {
    let Some(mut panel) = app.assistant_panel.take() else {
        return;
    };
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
                    Page::Reading => "外观",
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
                    let current = if c.id.as_deref() == Some("__sending__") {
                        i == usize::from(app.runner.settings.automatic)
                    } else {
                        choice_is_current(app, c.id.as_deref(), &v.value)
                    };
                    ListItem::new(format!(
                        "{}{}",
                        if current { "[当前] " } else { "" },
                        v.name
                    ))
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
    let detail = if panel.expanded {
        details(app, &panel)
    } else if let Some(error) = &panel.error {
        error.clone()
    } else if panel
        .choices
        .as_ref()
        .is_some_and(|choices| choices.id.as_deref() == Some("__sending__"))
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

fn draw_candidates(frame: &mut ratatui::Frame, app: &mut TerminalApp) {
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
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_ai_section("model").unwrap();
        let p = app.assistant_panel.as_ref().unwrap();
        assert_eq!(p.page, Page::Models);
        assert!(p.history.is_empty());
    }
    #[tokio::test]
    async fn settings_categories_and_details_are_keyboard_reachable() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_settings_section("reading").unwrap();
        let (tx, _) = mpsc::channel(1);

        assert!(
            app.assistant_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx.clone())
                .await
        );
        let panel = app.assistant_panel.as_ref().unwrap();
        assert_eq!(panel.page, Page::Account);
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
        let root = tempfile::tempdir().unwrap();
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
    fn folds_stay_on_the_same_page_and_only_one_advanced_group_opens() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut p = Panel::new();
        p.page = Page::Assistant;
        app.apply_assistant_row(Row::ToggleThanks, &mut p).unwrap();
        assert!(p.thanks_expanded);
        assert!(p.rows(&app).iter().any(|r| matches!(r, Row::ThankGifts)));
        assert!(p.rows(&app).iter().any(|r| matches!(r, Row::ThankShares)));
        p.page = Page::Advanced;
        app.apply_assistant_row(Row::AdvancedGroup(0), &mut p)
            .unwrap();
        app.apply_assistant_row(Row::AdvancedGroup(1), &mut p)
            .unwrap();
        assert_eq!(p.advanced_group, Some(1));
        assert!(
            !p.rows(&app)
                .iter()
                .any(|r| matches!(r, Row::Edit(Field::Profile)))
        );
    }
    #[test]
    fn preset_applies_once_without_preview_and_preserves_identity_fields() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut s = app.runner.settings.clone();
        s.name = "保留名字".into();
        s.automatic = true;
        app.runner.save(s, &app.bridge, false).unwrap();
        let mut p = Panel::new();
        p.page = Page::Presets;
        p.history.push(HistoryEntry {
            page: Page::Assistant,
            row: 0,
            scroll: 0,
            list_offset: 0,
            expanded: false,
            thanks_expanded: false,
            advanced_group: None,
        });
        app.apply_assistant_row(Row::Preset(ReplyPreset::TextQa), &mut p)
            .unwrap();
        assert_eq!(p.page, Page::Assistant);
        assert_eq!(app.runner.settings.name, "保留名字");
        assert!(app.runner.settings.automatic);
    }
    #[test]
    fn sending_mode_is_an_explicit_choice() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut p = Panel::new();
        app.apply_assistant_row(Row::Permission, &mut p).unwrap();
        let c = p.choices.as_ref().unwrap();
        assert_eq!(c.values.len(), 2);
        assert_eq!(c.values[0].name, "逐条发送");
        assert_eq!(c.values[1].name, "自动发送");
    }
    #[tokio::test]
    async fn failed_editor_save_keeps_the_exact_input() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_settings_section("reading").unwrap();
        let mut panel = app.assistant_panel.take().unwrap();
        app.apply_assistant_row(Row::Edit(Field::HistorySeconds), &mut panel)
            .unwrap();
        panel.editor.as_mut().unwrap().1.clear();
        panel.editor.as_mut().unwrap().1.insert_text("137");
        let blocked = root.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        app.config.config_path = blocked;
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
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.open_settings().unwrap();
        let mut p = app.assistant_panel.take().unwrap();
        p.row = 4;
        app.apply_assistant_row(Row::Page(Page::Materials), &mut p)
            .unwrap();
        assert_eq!(p.page, Page::Materials);
        assert!(!p.back());
        assert_eq!(p.page, Page::Settings);
        assert_eq!(p.row, 4);
        app.assistant_panel = Some(p);
        app.open_ai_section("materials").unwrap();
        assert!(app.assistant_panel.as_mut().unwrap().back());
    }
    #[test]
    fn reselecting_current_agent_keeps_custom_binary_and_source() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut s = app.runner.settings.clone();
        let binary = root.path().join("custom/omp");
        s.binary = binary.clone();
        s.source = Source::OmpProfile("live".into());
        app.runner.save(s, &app.bridge, false).unwrap();
        let mut p = Panel::new();
        p.discovered = Host::ALL.into_iter().map(Host::discovery).collect();
        app.apply_assistant_row(Row::Host(Host::Omp), &mut p)
            .unwrap();
        assert_eq!(app.runner.settings.binary, binary);
        assert!(matches!(&app.runner.settings.source,Source::OmpProfile(v) if v=="live"));
    }
    #[test]
    fn preset_running_gate_changes_nothing_and_keeps_permission() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut s = app.runner.settings.clone();
        s.name = "保留".into();
        s.automatic = true;
        app.runner.save(s, &app.bridge, false).unwrap();
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
    fn unsupported_native_option_never_opens_choices() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        app.runner.report.options.config = Some(vec![crate::runner::acp::ConfigOption {
            id: "future".into(),
            name: "未知".into(),
            category: None,
            current: false.into(),
            choices: vec![],
            unsupported: true,
        }]);
        let mut p = Panel::new();
        assert!(app.apply_assistant_row(Row::Option(0), &mut p).is_err());
        assert!(p.choices.is_none());
        assert!(app.runner.pending.is_none());
    }
    #[test]
    fn choosing_per_item_clears_stale_automatic_preference_without_permission() {
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let mut settings = app.runner.settings.clone();
        settings.automatic = true;
        app.runner.save(settings, &app.bridge, false).unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
        let mut app = replay::app(root.path(), DanmuSession::new("local"));
        let (tx, mut rx) = mpsc::channel(4);
        app.command("/obs config host example.invalid", tx.clone())
            .await
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        app.edit_workspace(None, tx).unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
                assert!(app.secret_mode);
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
            assert!(!app.secret_mode && app.stop_flow.is_none());
            assert_eq!(app.assistant_panel.as_ref().unwrap().page, Page::Obs);
            assert_eq!(app.input, "人工草稿");
        }
        assert!(!root.path().join("obs-password").exists());
    }
    #[tokio::test]
    async fn settings_archive_search_requires_a_keyword_and_cancel_preserves_the_draft() {
        let root = tempfile::tempdir().unwrap();
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
        let root = tempfile::tempdir().unwrap();
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
