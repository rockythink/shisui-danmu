use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Host {
    #[default]
    Omp,
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Cursor,
    Copilot,
    Amp,
    Pi,
    Dsh,
}
#[derive(Debug, Clone)]
pub(crate) struct Discovery {
    pub host: Host,
    pub program: Option<PathBuf>,
    pub native: Option<PathBuf>,
}
impl Host {
    pub const ALL: [Self; 9] = [
        Self::Omp,
        Self::Claude,
        Self::Codex,
        Self::OpenCode,
        Self::Gemini,
        Self::Copilot,
        Self::Amp,
        Self::Pi,
        Self::Dsh,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Omp => "OMP",
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini CLI",
            Self::Cursor => "Cursor",
            Self::Copilot => "GitHub Copilot CLI",
            Self::Amp => "Amp",
            Self::Pi => "Pi",
            Self::Dsh => "DeepSeek Harness (DSH)",
        }
    }
    pub fn executable(self) -> &'static str {
        match self {
            Self::Omp => "omp",
            Self::Claude => "claude-agent-acp",
            Self::Codex => "codex-acp",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Cursor => "agent",
            Self::Copilot => "copilot",
            Self::Amp => "amp-acp",
            Self::Pi => "pi-acp",
            Self::Dsh => "dsh",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Omp => "使用 omp acp；沿用原生登录，禁止自动加载工具与扩展。",
            Self::Gemini => {
                "使用 gemini --acp；沿用原生登录。模型继承 Gemini 配置，不调用会写回全局设置的旧模型接口。"
            }
            Self::Claude => {
                "需要 claude-agent-acp；只安装 claude 不等于安装 ACP 适配器。使用本人的 Claude Code 登录。"
            }
            Self::Codex => {
                "需要 codex-acp；只安装 codex 不等于安装 ACP 适配器。使用本人的 Codex 登录。"
            }
            Self::OpenCode => "使用 opencode acp；启动配置与插件必须在私有执行目录中隔离。",
            Self::Cursor => "使用 Cursor 的 agent acp；不是 cursor 编辑器命令。",
            Self::Copilot => "使用 copilot --acp；这是独立 GitHub Copilot CLI，不是 VS Code 扩展。",
            Self::Amp => "需要 amp-acp 和原生 Amp CLI；不自动安装适配器。",
            Self::Pi => {
                "需要原生 Pi CLI 与 pi-acp；danmu setup pi 显式准备私有适配器。PATH 优先；全局扩展、工具和自动上下文关闭；开启联网时仅加载内置免费搜索，每轮最多一次、三条结果。"
            }
            Self::Dsh => "使用官方 DeepSeek Harness 的 ACP 传输与私有最小插件组合；不启动 Web UI。",
        }
    }
    /// Read-only discovery: PATH first, then the explicitly prepared private Pi adapter.
    pub fn discover(self) -> Option<PathBuf> {
        discover_executable(self.executable()).or_else(|| {
            if self == Self::Pi {
                crate::setup::discover_pi_adapter()
            } else {
                None
            }
        })
    }
    pub(crate) fn discovery(self) -> Discovery {
        let program = self.discover();
        let native = if program.is_none() || self == Self::Pi {
            match self {
                Self::Claude => discover_executable("claude"),
                Self::Codex => discover_executable("codex"),
                Self::Pi => discover_executable("pi"),
                Self::Amp => discover_executable("amp"),
                _ => None,
            }
        } else {
            None
        };
        Discovery {
            host: self,
            program,
            native,
        }
    }
}
fn discover_executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(name))
        .find(|p| executable_file(p))
}
pub fn executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Source {
    #[default]
    Default,
    OmpProfile(String),
    NativePrompt(PathBuf),
}
impl Source {
    pub fn description(&self) -> String {
        match self {
            Self::Default => {
                "默认最小上下文：SYSTEM.md与选定人设常驻，项目资料/skills仅显式选择；私有cwd，不扫描其他文件".into()
            }
            Self::OmpProfile(name) => format!(
                "用户主动额外上下文：OMP原生profile {name}，继承模型/人格；不继承聊天；hooks/插件/自动skills/rules禁用，工具仍受程序权限限制"
            ),
            Self::NativePrompt(path) => format!(
                "用户主动额外上下文：OMP --system-prompt {}的只读快照；文本修改后下一轮重建私有进程，不加载目录/脚本；内容未获Agent回报确认",
                path.display()
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Persona {
    #[default]
    Assistant,
    Broadcaster,
}
impl Persona {
    pub fn label(self) -> &'static str {
        match self {
            Self::Assistant => "独立人设",
            Self::Broadcaster => "复用主播",
        }
    }
    pub fn filename(self) -> &'static str {
        match self {
            Self::Assistant => "PERSONA.md",
            Self::Broadcaster => "BROADCASTER.md",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplyActivity {
    #[default]
    Cautious,
    Balanced,
    Active,
}
impl ReplyActivity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cautious => "优先回应点名",
            Self::Balanced => "也回答明确问题",
            Self::Active => "允许主动补充",
        }
    }
    pub fn guidance(self) -> &'static str {
        match self {
            Self::Cautious => "主要回应明确点名、@助手或对助手回答的追问；其他普通弹幕优先不回。",
            Self::Balanced => {
                "也可回答明确、独立且适合文字回答的知识问题；疑似在与主播交流则不回。"
            }
            Self::Active => {
                "可主动补充与当前主题直接相关且确有帮助的信息；不逐条回应、不抢主播话题。"
            }
        }
    }
}

/// One-shot reply preferences, never a stored mode or a sending authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyPreset {
    Quiet,
    TextQa,
    Thanks,
}
impl ReplyPreset {
    pub const ALL: [Self; 3] = [Self::Quiet, Self::TextQa, Self::Thanks];

    pub fn label(self) -> &'static str {
        match self {
            Self::Quiet => "少主动回复",
            Self::TextQa => "侧重回答问题",
            Self::Thanks => "感谢礼物、关注和分享",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Quiet => "优先回应点名，不主动感谢。并非停止回复；完全不回复请暂停助手。",
            Self::TextQa => "也回答明确问题，并在普通问答回复中@提问者；不主动感谢互动。",
            Self::Thanks => {
                "增加礼物、关注和分享答谢；不感谢点赞。感谢是否逐人@不受普通问答@开关影响。"
            }
        }
    }

    /// Changes only the ten reply fields in memory. The caller owns preview and saving.
    pub fn apply(self, settings: &mut Settings) {
        settings.reply_activity = match self {
            Self::TextQa => ReplyActivity::Balanced,
            Self::Quiet | Self::Thanks => ReplyActivity::Cautious,
        };
        settings.thank_gifts = self == Self::Thanks;
        settings.thank_likes = false;
        settings.thank_follows = self == Self::Thanks;
        settings.thank_shares = self == Self::Thanks;
        settings.mention_sender = self == Self::TextQa;
        settings.web_search = false;
        settings.repair_blocked = false;
        settings.dynamic_host_priority = true;
        settings.dynamic_muted_support = self != Self::Quiet;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicReplyStrategy {
    HostPriority,
    MutedSupport,
    Base,
    Unknown,
}
impl DynamicReplyStrategy {
    pub fn label(self) -> &'static str {
        match self {
            Self::HostPriority => "主播麦克风未静音 · 少插话，保留问答",
            Self::MutedSupport => "主播麦克风已静音 · 文字补充",
            Self::Base => "按选定回复范围处理",
            Self::Unknown => "麦克风状态未知",
        }
    }
    pub fn guidance(self) -> &'static str {
        match self {
            Self::HostPriority => {
                "已开启开麦让位：只减少主动插话与重复补充，不把未静音当作正在讲话或停止问答的依据。直接问助手的问题正常处理；在本轮基础回复范围允许时，无明确收件人、文字完整且可独立回答的公开问题也应简短回答，不要求必须@助手。明确给主播或其他观众的消息不抢答，依赖未知口播的问题不猜。"
            }
            Self::MutedSupport => {
                "已开启静音补位：在基础积极性范围内，更愿意接答完整、无明确收件人且仅凭文字可独立回答的公开问题；不把静音当作没有主播，不介入明确给主播或其他观众的对话。"
            }
            Self::Base => {
                "当前麦克风条件对应的动态策略已关闭，按基础积极性判断；仍先辨明对象，不猜测或复述未知口播。"
            }
            Self::Unknown => {
                "麦克风状态缺失、断连或过期，不得当成静音触发补位；按基础积极性保守判断，语音相关含糊问题不猜。"
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub format: u8,
    pub host: Host,
    pub binary: PathBuf,
    pub name: String,
    pub preferences: String,
    pub use_profile: bool,
    pub use_project: bool,
    pub workspace: Option<PathBuf>,
    pub source: Source,
    pub automatic: bool,
    /// Remember enablement intent, never a restored sending permission.
    pub resume_on_start: bool,
    pub reply_activity: ReplyActivity,
    pub dynamic_host_priority: bool,
    pub dynamic_muted_support: bool,
    pub thank_gifts: bool,
    pub thank_likes: bool,
    pub thank_follows: bool,
    pub thank_shares: bool,
    pub mention_sender: bool,
    pub repair_blocked: bool,
    pub web_search: bool,
    pub blocked_words: Vec<String>,
    pub native_preferences: Vec<NativePreferences>,
    pub skills: Vec<String>,
    pub persona: Persona,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativePreferences {
    host: Host,
    binary: PathBuf,
    source: Source,
    pub model: Option<String>,
    pub thinking: Option<String>,
}
impl NativePreferences {
    pub fn scope(settings: &Settings) -> Self {
        Self {
            host: settings.host,
            binary: settings.binary.clone(),
            source: settings.source.clone(),
            model: None,
            thinking: None,
        }
    }
    pub fn capture(&mut self, options: &super::acp::Options) {
        let value = |id: Option<&str>| {
            let id = id?;
            options
                .config
                .as_ref()?
                .iter()
                .find(|o| o.id == id && !o.unsupported)?
                .current
                .as_value_id()
                .map(|value| value.to_string())
        };
        let (model, thinking) = super::hosts::preference_ids(self.host);
        self.model = value(model);
        self.thinking = value(thinking);
    }
    fn same_scope(&self, other: &Self) -> bool {
        self.host == other.host && self.binary == other.binary && self.source == other.source
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            format: 3,
            host: Host::Omp,
            binary: PathBuf::new(),
            name: "拾穗小助手".into(),
            preferences: String::new(),
            use_profile: false,
            use_project: false,
            workspace: None,
            source: Source::Default,
            automatic: false,
            resume_on_start: false,
            reply_activity: ReplyActivity::default(),
            dynamic_host_priority: true,
            dynamic_muted_support: true,
            thank_gifts: false,
            thank_likes: false,
            thank_follows: false,
            thank_shares: false,
            mention_sender: false,
            repair_blocked: true,
            web_search: false,
            blocked_words: Vec::new(),
            native_preferences: Vec::new(),
            skills: Vec::new(),
            persona: Persona::Assistant,
        }
    }
}
impl Settings {
    pub fn dynamic_strategy(
        &self,
        microphone: crate::obs::MicrophoneState,
    ) -> DynamicReplyStrategy {
        use crate::obs::MicrophoneState;
        match microphone {
            MicrophoneState::Unmuted if self.dynamic_host_priority => {
                DynamicReplyStrategy::HostPriority
            }
            MicrophoneState::Muted if self.dynamic_muted_support => {
                DynamicReplyStrategy::MutedSupport
            }
            MicrophoneState::Unknown => DynamicReplyStrategy::Unknown,
            _ => DynamicReplyStrategy::Base,
        }
    }
    pub fn native_preferences(&self) -> Option<&NativePreferences> {
        self.native_preferences
            .iter()
            .find(|p| p.host == self.host && p.binary == self.binary && p.source == self.source)
    }
    pub fn remember_native(&mut self, preferences: NativePreferences) {
        if preferences.model.is_none() && preferences.thinking.is_none() {
            return;
        }
        if let Some(saved) = self
            .native_preferences
            .iter_mut()
            .find(|p| p.same_scope(&preferences))
        {
            *saved = preferences;
        } else {
            self.native_preferences.push(preferences);
        }
    }
    pub fn load(path: &Path) -> Result<(Self, Option<String>)> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), None));
            }
            Err(e) => return Err(e).context("读取助手设置失败"),
        };
        ensure!(bytes.len() <= 32768, "助手设置文件过大");
        let mut value: serde_json::Value =
            serde_json::from_slice(&bytes).context("助手设置损坏，未覆盖原文件")?;
        // One-way data migration: an editor executable is never reused as Copilot CLI.
        let mut host_migrated = false;
        if value["host"] == "vs_code" {
            value["host"] = "copilot".into();
            value["binary"] = "".into();
            host_migrated = true;
        }
        if let Some(preferences) = value
            .get_mut("native_preferences")
            .and_then(|v| v.as_array_mut())
        {
            for saved in preferences {
                if saved["host"] == "vs_code" {
                    saved["host"] = "copilot".into();
                    saved["binary"] = "".into();
                    saved["model"] = serde_json::Value::Null;
                    saved["thinking"] = serde_json::Value::Null;
                    host_migrated = true;
                }
            }
        }
        if value.get("format").is_some() {
            let mut migration = if value["format"] == 2 {
                let fields = value.as_object_mut().context("助手设置须为对象")?;
                if let Some(topic) = fields.remove("topic") {
                    ensure!(topic.is_string(), "旧助手主题格式损坏，未覆盖");
                }
                fields.insert("format".into(), 3.into());
                Some("已在内存迁移助手偏好；旧主题不跨场沿用，保存时备份原文件".into())
            } else {
                None
            };
            if host_migrated {
                migration = Some("旧 VS Code 入口已替换为独立 Copilot CLI；不继承编辑器程序路径或原生参数，保存时备份原文件".into());
            }
            let settings: Self = serde_json::from_value(value)?;
            settings.validate()?;
            return Ok((settings, migration));
        }
        // One-way data migration. Retired CLI model/effort strings never become ACP options.
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct Previous {
            host: Host,
            binary: PathBuf,
            name: String,
            style: String,
            #[serde(rename = "topic")]
            _topic: String,
            scope: String,
            model: String,
            strength: String,
            automatic: bool,
        }
        let old: Previous = serde_json::from_value(value).context("无法识别旧助手设置，未覆盖")?;
        let requested = !old.model.is_empty() || !old.strength.is_empty();
        let settings = Self {
            host: old.host,
            binary: if matches!(old.host, Host::Omp | Host::Gemini | Host::OpenCode) {
                old.binary
            } else {
                PathBuf::new()
            },
            name: if old.name.is_empty() {
                Self::default().name
            } else {
                old.name
            },
            preferences: [old.style, old.scope]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("；"),
            automatic: old.automatic,
            ..Self::default()
        };
        settings.validate()?;
        Ok((
            settings,
            Some(format!(
                "已在内存迁移旧助手偏好；旧主题不跨场沿用，保存时备份原文件。{}ACP模型/强度按原生回执保存，重启后重新校验恢复",
                if requested {
                    "旧CLI模型/强度请求未执行；"
                } else {
                    ""
                }
            )),
        ))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.format == 3, "未知助手设置版本，未覆盖");
        ensure!(
            self.workspace
                .as_ref()
                .is_none_or(|path| path.is_absolute()),
            "工作区须为绝对路径"
        );
        ensure!(self.skills.len() <= 16, "最多选择16个技能");
        let mut selected = std::collections::BTreeSet::new();
        for name in &self.skills {
            ensure!(
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
                "技能名称须为字母/数字/-/_，不能传路径"
            );
            ensure!(selected.insert(name), "技能重复选择：{name}");
        }
        ensure!(
            self.binary.as_os_str().is_empty() || self.binary.is_absolute(),
            "程序路径必须为绝对路径"
        );
        for (name, value, max) in [
            ("名字", self.name.as_str(), 128),
            ("偏好", self.preferences.as_str(), 8192),
        ] {
            ensure!(
                value.len() <= max && !value.chars().any(char::is_control),
                "{name}过长或含控制字符"
            );
        }
        ensure!(!self.name.trim().is_empty(), "助手名字不能为空");
        ensure!(self.blocked_words.len() <= 128, "已知词最多128条");
        for word in &self.blocked_words {
            ensure!(
                !word.trim().is_empty() && word.len() <= 128 && !word.chars().any(char::is_control),
                "已知词须为1..128字节且无控制字符"
            );
        }
        match &self.source {
            Source::Default => {}
            Source::OmpProfile(name) => {
                ensure!(self.host == Host::Omp, "该Agent未核实OMP profile加载方式");
                ensure!(
                    !name.is_empty()
                        && name.len() <= 64
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
                    "profile须为已准备的原生名称（字母/数字/-/_），不能传路径或命令"
                );
            }
            Source::NativePrompt(path) => ensure!(
                self.host == Host::Omp && path.is_absolute(),
                "原生规则文件当前仅核实OMP；须为绝对路径"
            ),
        }
        Ok(())
    }
    pub fn program(&self) -> Result<PathBuf> {
        let path = if self.binary.as_os_str().is_empty() {
            self.host
                .discover()
                .context(if self.host == Host::Pi {
                    "未发现 pi-acp；运行 danmu setup pi 显式准备私有适配器，另需原生 pi 位于 PATH；不自动安装"
                } else {
                    "未发现ACP程序；请设置已安装路径，不自动安装"
                })?
        } else {
            self.binary.clone()
        };
        ensure!(
            path.is_absolute() && executable_file(&path),
            "ACP程序不存在或不可执行"
        );
        Ok(path)
    }
    pub fn ready(&self) -> Result<()> {
        self.validate()?;
        ensure!(
            cfg!(unix),
            "ACP进程清退当前要求POSIX进程组；此平台不启动未受管Agent"
        );
        ensure!(
            !self.web_search || super::hosts::supports_search(self.host),
            "该宿主尚无受控联网搜索接入；请关闭联网，或选择 OMP / Gemini / Pi"
        );
        self.program()?;
        Ok(())
    }
    pub fn same_context(&self, other: &Self) -> bool {
        self.host == other.host
            && self.binary == other.binary
            && self.source == other.source
            && self.web_search == other.web_search
            && self.workspace == other.workspace
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        self.validate()?;
        if path.exists() {
            let (_, migration) = Self::load(path)?;
            if migration.is_some() {
                let original = std::fs::read(path)?;
                let value: serde_json::Value = serde_json::from_slice(&original)?;
                let backup = path.with_file_name(if value["format"] == 2 {
                    "assistant.pre-context.json"
                } else if value["format"] == 3 {
                    "assistant.pre-hosts.json"
                } else {
                    "assistant.pre-acp.json"
                });
                if backup.exists() {
                    ensure!(
                        std::fs::read(&backup)? == original,
                        "已有不同的旧设置备份，未覆盖；请本人处理"
                    );
                } else {
                    let mut saved =
                        tempfile::NamedTempFile::new_in(path.parent().context("缺少设置目录")?)?;
                    saved.write_all(&original)?;
                    saved.as_file().sync_all()?;
                    saved.persist_noclobber(backup)?;
                }
            }
        }
        let parent = path.parent().context("助手设置缺少父目录")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(file.as_file_mut(), self)?;
        ensure!(
            file.as_file().metadata()?.len() <= 32768,
            "助手设置超过32KiB，未覆盖原文件"
        );
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_presets_replace_only_reply_fields_without_resetting_identity_or_permissions() {
        for (preset, activity, gifts, follows, shares, mention, muted) in [
            (
                ReplyPreset::Quiet,
                ReplyActivity::Cautious,
                false,
                false,
                false,
                false,
                false,
            ),
            (
                ReplyPreset::TextQa,
                ReplyActivity::Balanced,
                false,
                false,
                false,
                true,
                true,
            ),
            (
                ReplyPreset::Thanks,
                ReplyActivity::Cautious,
                true,
                true,
                true,
                false,
                true,
            ),
        ] {
            for automatic in [false, true] {
                let mut settings = Settings {
                    format: 3,
                    host: Host::Codex,
                    binary: PathBuf::from("/private/custom-codex-acp"),
                    name: "保留我的助手名".into(),
                    preferences: "保留自定义偏好，先列证据".into(),
                    use_profile: true,
                    use_project: true,
                    workspace: Some(PathBuf::from("/private/my-workspace")),
                    source: Source::Default,
                    automatic,
                    resume_on_start: !automatic,
                    reply_activity: ReplyActivity::Active,
                    dynamic_host_priority: false,
                    dynamic_muted_support: !muted,
                    thank_gifts: !gifts,
                    thank_likes: true,
                    thank_follows: !follows,
                    thank_shares: !shares,
                    mention_sender: !mention,
                    repair_blocked: true,
                    web_search: true,
                    blocked_words: vec!["保留屏蔽词".into()],
                    native_preferences: Vec::new(),
                    skills: vec!["my-skill".into()],
                    persona: Persona::Broadcaster,
                };
                if !automatic {
                    settings.host = Host::Omp;
                    settings.source = Source::OmpProfile("my-profile".into());
                }
                let mut native = NativePreferences::scope(&settings);
                native.model = Some("my-model".into());
                native.thinking = Some("my-thinking".into());
                settings.remember_native(native);
                settings.native_preferences.push(NativePreferences {
                    host: Host::Omp,
                    binary: PathBuf::from("/private/other-omp"),
                    source: Source::NativePrompt(PathBuf::from("/private/my-rules")),
                    model: Some("other-model".into()),
                    thinking: Some("other-thinking".into()),
                });
                let expected = Settings {
                    reply_activity: activity,
                    thank_gifts: gifts,
                    thank_likes: false,
                    thank_follows: follows,
                    thank_shares: shares,
                    mention_sender: mention,
                    repair_blocked: false,
                    web_search: false,
                    dynamic_host_priority: true,
                    dynamic_muted_support: muted,
                    ..settings.clone()
                };

                preset.apply(&mut settings);

                assert_eq!(settings, expected, "{preset:?}, automatic={automatic}");
            }
        }
    }

    #[test]
    fn current_format_without_share_preference_keeps_existing_values_and_disables_shares() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let settings = Settings {
            thank_gifts: true,
            mention_sender: true,
            ..Settings::default()
        };
        let mut old: serde_json::Value = serde_json::to_value(settings).unwrap();
        old.as_object_mut().unwrap().remove("thank_shares");
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let (loaded, migration) = Settings::load(&path).unwrap();

        assert!(loaded.thank_gifts);
        assert!(loaded.mention_sender);
        assert!(!loaded.thank_shares);
        assert!(migration.is_none());
    }
}
