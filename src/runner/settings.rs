use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApiProtocol {
    #[default]
    OpenaiChat,
    AnthropicMessages,
    ChatgptSubscription,
}
impl ApiProtocol {
    pub(crate) fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenaiChat | Self::ChatgptSubscription => "https://api.openai.com/v1",
            Self::AnthropicMessages => "https://api.anthropic.com/v1",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenaiChat => "OpenAI兼容 Chat Completions",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::ChatgptSubscription => "ChatGPT订阅（官方 preview）",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OpenaiTokenLimit {
    #[default]
    MaxCompletionTokens,
    MaxTokens,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ApiConfig {
    pub protocol: ApiProtocol,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
    pub openai_token_limit: OpenaiTokenLimit,
    /// Local registration ID only; OAuth credentials live in private storage.
    pub chatgpt_account: Option<String>,
}
impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            protocol: ApiProtocol::OpenaiChat,
            base_url: ApiProtocol::OpenaiChat.default_base_url().into(),
            model: String::new(),
            max_output_tokens: 4096,
            openai_token_limit: OpenaiTokenLimit::MaxCompletionTokens,
            chatgpt_account: None,
        }
    }
}
pub(crate) fn normalize_base_url(input: &str) -> Result<String> {
    ensure!(
        input.len() <= 2048 && !input.chars().any(char::is_control),
        "API地址无效"
    );
    let url = url::Url::parse(input.trim()).map_err(|_| anyhow::anyhow!("API地址无效"))?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "API地址不能含认证、查询或fragment"
    );
    ensure!(url.host().is_some(), "API地址缺少主机");
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "API仅允许HTTPS或loopback IP HTTP"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
impl ApiConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.base_url.len() <= 2048 && !self.base_url.chars().any(char::is_control),
            "API地址无效"
        );
        if !self.base_url.trim().is_empty() {
            let normalized = normalize_base_url(&self.base_url)?;
            ensure!(
                self.protocol != ApiProtocol::ChatgptSubscription
                    || normalized == self.protocol.default_base_url(),
                "ChatGPT订阅仅允许官方API地址"
            );
        }
        let model = self.model.trim();
        ensure!(
            model.len() <= 256 && !self.model.chars().any(char::is_control),
            "模型名无效"
        );
        ensure!(
            (256..=8192).contains(&self.max_output_tokens),
            "输出上限须为256..8192"
        );
        if let Some(account) = &self.chatgpt_account {
            ensure!(
                !account.trim().is_empty()
                    && account.len() <= 256
                    && !account.chars().any(char::is_control),
                "ChatGPT账号标识无效"
            );
        }
        Ok(())
    }
    pub(crate) fn ready(&self) -> Result<()> {
        self.validate()?;
        ensure!(!self.base_url.trim().is_empty(), "未设置API地址");
        ensure!(!self.model.trim().is_empty(), "未设置模型名");
        ensure!(
            self.protocol != ApiProtocol::ChatgptSubscription || self.chatgpt_account.is_some(),
            "未选择ChatGPT账号"
        );
        Ok(())
    }
    pub(crate) fn normalize(&mut self) -> Result<()> {
        self.validate()?;
        self.base_url = if self.base_url.trim().is_empty() {
            String::new()
        } else {
            normalize_base_url(&self.base_url)?
        };
        self.model = self.model.trim().to_owned();
        Ok(())
    }
}
#[cfg(test)]
mod api_config_tests {
    use super::*;
    #[test]
    fn canonical_scope_preserves_prefix_and_rejects_credential_urls() {
        assert_eq!(
            normalize_base_url(" HTTPS://EXAMPLE.COM:443/prefix/v1/// ").unwrap(),
            "https://example.com/prefix/v1"
        );
        assert_eq!(
            normalize_base_url("http://[0:0:0:0:0:0:0:1]:80/v1/").unwrap(),
            "http://[::1]/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.7.8.9:1234/v1").unwrap(),
            "http://127.7.8.9:1234/v1"
        );
        assert_eq!(
            normalize_base_url("https://例子.测试/v1").unwrap(),
            "https://xn--fsqu00a.xn--0zwm56d/v1"
        );
        for input in [
            "http://localhost/v1",
            "http://example.com",
            "https://u:p@example.com/v1",
            "https://example.com/v1?q=1",
            "https://example.com/v1#x",
            "ftp://example.com/v1",
            "https://example.com/\n",
            "",
        ] {
            assert!(normalize_base_url(input).is_err(), "{input:?}");
        }
        let partial = ApiConfig {
            base_url: String::new(),
            ..ApiConfig::default()
        };
        assert!(partial.validate().is_ok());
        assert!(partial.ready().is_err());
        for tokens in [0, 255, 8193] {
            assert!(
                ApiConfig {
                    max_output_tokens: tokens,
                    ..ApiConfig::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            ApiConfig {
                model: "x\n".into(),
                ..ApiConfig::default()
            }
            .validate()
            .is_err()
        );
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

    /// Changes only the reply fields in memory. The caller owns preview and saving.
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
        settings.repair_blocked = true;
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
    pub(crate) api: ApiConfig,
    pub name: String,
    pub preferences: String,
    pub use_profile: bool,
    pub use_project: bool,
    pub workspace: Option<PathBuf>,
    pub automatic: bool,
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
    pub skills: Vec<String>,
    pub persona: Persona,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            format: 4,
            api: ApiConfig::default(),
            name: "拾穗小助手".into(),
            preferences: String::new(),
            use_profile: true,
            use_project: true,
            workspace: None,
            automatic: false,
            resume_on_start: false,
            reply_activity: ReplyActivity::Cautious,
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
            skills: Vec::new(),
            persona: Persona::Assistant,
        }
    }
}

// Inert legacy schemas. Never discover hosts, read native rules or restore native options.
#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyHost {
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
    VsCode,
}
#[allow(dead_code)] // Payload types are checked during migration, then deliberately retired.
#[derive(Default, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum LegacySource {
    #[default]
    Default,
    OmpProfile(String),
    NativePrompt(PathBuf),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyNativePreferences {
    #[serde(rename = "host")]
    _host: LegacyHost,
    #[serde(rename = "binary")]
    _binary: PathBuf,
    #[serde(rename = "source")]
    _source: LegacySource,
    #[serde(rename = "model")]
    _model: Option<String>,
    #[serde(rename = "thinking")]
    _thinking: Option<String>,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Previous {
    #[serde(rename = "host")]
    _host: LegacyHost,
    #[serde(rename = "binary")]
    _binary: PathBuf,
    name: String,
    style: String,
    #[serde(rename = "topic")]
    _topic: String,
    scope: String,
    #[serde(rename = "model")]
    _model: String,
    #[serde(rename = "strength")]
    _strength: String,
    #[serde(rename = "automatic")]
    _automatic: bool,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyV2 {
    format: u8,
    #[serde(rename = "host")]
    _host: LegacyHost,
    #[serde(rename = "binary")]
    _binary: PathBuf,
    #[serde(rename = "source")]
    _source: LegacySource,
    #[serde(rename = "native_preferences")]
    _native_preferences: Vec<LegacyNativePreferences>,
    #[serde(rename = "topic")]
    _topic: String,
    name: String,
    preferences: String,
    use_profile: bool,
    use_project: bool,
    workspace: Option<PathBuf>,
    automatic: bool,
    resume_on_start: bool,
    reply_activity: ReplyActivity,
    dynamic_host_priority: bool,
    dynamic_muted_support: bool,
    thank_gifts: bool,
    thank_likes: bool,
    thank_follows: bool,
    thank_shares: bool,
    mention_sender: bool,
    repair_blocked: bool,
    web_search: bool,
    blocked_words: Vec<String>,
    skills: Vec<String>,
    persona: Persona,
}
impl Default for LegacyV2 {
    fn default() -> Self {
        // Frozen format-3 defaults: never derived from the current API Settings.
        Self {
            format: 2,
            _host: LegacyHost::Omp,
            _binary: PathBuf::new(),
            _source: LegacySource::Default,
            _native_preferences: Vec::new(),
            _topic: String::new(),
            name: "拾穗小助手".into(),
            preferences: String::new(),
            use_profile: true,
            use_project: true,
            workspace: None,
            automatic: false,
            resume_on_start: false,
            reply_activity: ReplyActivity::Cautious,
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
            skills: Vec::new(),
            persona: Persona::Assistant,
        }
    }
}
impl LegacyV2 {
    fn migrate(self) -> Result<Settings> {
        ensure!(self.format == 2, "未知助手设置版本，未覆盖");
        // Old authorization and search intent never transfer to another backend.
        let _ = (self.automatic, self.resume_on_start, self.web_search);
        Ok(Settings {
            format: 4,
            api: ApiConfig::default(),
            name: self.name,
            preferences: self.preferences,
            use_profile: self.use_profile,
            use_project: self.use_project,
            workspace: self.workspace,
            automatic: false,
            resume_on_start: false,
            reply_activity: self.reply_activity,
            dynamic_host_priority: self.dynamic_host_priority,
            dynamic_muted_support: self.dynamic_muted_support,
            thank_gifts: self.thank_gifts,
            thank_likes: self.thank_likes,
            thank_follows: self.thank_follows,
            thank_shares: self.thank_shares,
            mention_sender: self.mention_sender,
            repair_blocked: self.repair_blocked,
            web_search: false,
            blocked_words: self.blocked_words,
            skills: self.skills,
            persona: self.persona,
        })
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyV3 {
    format: u8,
    #[serde(rename = "host")]
    _host: LegacyHost,
    #[serde(rename = "binary")]
    _binary: PathBuf,
    #[serde(rename = "source")]
    _source: LegacySource,
    #[serde(rename = "native_preferences")]
    _native_preferences: Vec<LegacyNativePreferences>,
    name: String,
    preferences: String,
    use_profile: bool,
    use_project: bool,
    workspace: Option<PathBuf>,
    automatic: bool,
    resume_on_start: bool,
    reply_activity: ReplyActivity,
    dynamic_host_priority: bool,
    dynamic_muted_support: bool,
    thank_gifts: bool,
    thank_likes: bool,
    thank_follows: bool,
    thank_shares: bool,
    mention_sender: bool,
    repair_blocked: bool,
    web_search: bool,
    blocked_words: Vec<String>,
    skills: Vec<String>,
    persona: Persona,
}
impl Default for LegacyV3 {
    fn default() -> Self {
        // Frozen format-3 defaults: never derived from the current API Settings.
        Self {
            format: 3,
            _host: LegacyHost::Omp,
            _binary: PathBuf::new(),
            _source: LegacySource::Default,
            _native_preferences: Vec::new(),
            name: "拾穗小助手".into(),
            preferences: String::new(),
            use_profile: true,
            use_project: true,
            workspace: None,
            automatic: false,
            resume_on_start: false,
            reply_activity: ReplyActivity::Cautious,
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
            skills: Vec::new(),
            persona: Persona::Assistant,
        }
    }
}
impl LegacyV3 {
    fn migrate(self) -> Result<Settings> {
        ensure!(self.format == 3, "未知助手设置版本，未覆盖");
        // Old authorization and search intent never transfer to another backend.
        let _ = (self.automatic, self.resume_on_start, self.web_search);
        Ok(Settings {
            format: 4,
            api: ApiConfig::default(),
            name: self.name,
            preferences: self.preferences,
            use_profile: self.use_profile,
            use_project: self.use_project,
            workspace: self.workspace,
            automatic: false,
            resume_on_start: false,
            reply_activity: self.reply_activity,
            dynamic_host_priority: self.dynamic_host_priority,
            dynamic_muted_support: self.dynamic_muted_support,
            thank_gifts: self.thank_gifts,
            thank_likes: self.thank_likes,
            thank_follows: self.thank_follows,
            thank_shares: self.thank_shares,
            mention_sender: self.mention_sender,
            repair_blocked: self.repair_blocked,
            web_search: false,
            blocked_words: self.blocked_words,
            skills: self.skills,
            persona: self.persona,
        })
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
    pub fn load(path: &Path) -> Result<(Self, Option<String>)> {
        match read_settings(path)? {
            Some(bytes) => Self::decode(&bytes),
            None => Ok((Self::default(), None)),
        }
    }
    fn decode(bytes: &[u8]) -> Result<(Self, Option<String>)> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).context("助手设置损坏，未覆盖原文件")?;
        let object = value
            .as_object()
            .context("助手设置须为对象，未覆盖原文件")?;
        let migrated = object.get("format") != Some(&serde_json::json!(4));
        let mut settings = match object.get("format") {
            None => {
                let old: Previous =
                    serde_json::from_value(value).context("无法识别旧助手设置，未覆盖")?;
                Self {
                    name: if old.name.is_empty() {
                        "拾穗小助手".into()
                    } else {
                        old.name
                    },
                    preferences: [old.style, old.scope]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("；"),
                    ..Self::default()
                }
            }
            Some(format) if format == 2 => serde_json::from_value::<LegacyV2>(value)
                .context("旧助手设置格式损坏，未覆盖")?
                .migrate()?,
            Some(format) if format == 3 => serde_json::from_value::<LegacyV3>(value)
                .context("旧助手设置格式损坏，未覆盖")?
                .migrate()?,
            Some(format) if format == 4 => {
                serde_json::from_value::<Self>(value).context("助手设置格式损坏，未覆盖")?
            }
            _ => anyhow::bail!("未知助手设置版本，未覆盖"),
        };
        settings.api.normalize()?;
        settings.validate()?;
        let notice = migrated.then(|| "已在内存迁移API助手设置，资料与归档保留；请配置API并重新启动、授权。旧原生规则来源不再加载，请本人将必要文本放入既有SYSTEM、人设、PROJECT或preferences；不会自动扫描或复制原生配置。首次保存将备份原始设置。".into());
        Ok((settings.harden(), notice))
    }
    /// 资料引用、开麦主播优先与有限修复统一由程序维护，不再提供界面开关。
    fn harden(mut self) -> Self {
        self.use_profile = true;
        self.use_project = true;
        self.dynamic_host_priority = true;
        self.repair_blocked = true;
        self
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.format == 4, "未知助手设置版本，未覆盖");
        self.api.validate()?;
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
        Ok(())
    }
    pub fn ready(&self) -> Result<()> {
        self.validate()?;
        self.api.ready()
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut settings = self.clone();
        settings.api.normalize()?;
        settings.validate()?;
        let serialized = serde_json::to_vec_pretty(&settings)?;
        ensure!(serialized.len() <= 32768, "助手设置超过32KiB，未覆盖原文件");
        if let Some(original) = read_settings(path)? {
            let (_, migration) = Self::decode(&original)?;
            if migration.is_some() {
                backup_legacy(path, &original)?;
            }
        }
        crate::storage::write_private_atomic(path, &serialized)
    }
}

fn read_settings(path: &Path) -> Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("读取助手设置失败"),
    };
    ensure!(file.metadata()?.is_file(), "助手设置须为普通文件");
    let mut bytes = Vec::new();
    file.take(32769)
        .read_to_end(&mut bytes)
        .context("读取助手设置失败")?;
    ensure!(bytes.len() <= 32768, "助手设置文件过大");
    Ok(Some(bytes))
}

fn backup_legacy(path: &Path, original: &[u8]) -> Result<()> {
    use std::io::Write;
    let backup = path.with_file_name("assistant.pre-api.json");
    crate::workspace_config::check_file(&backup)?;
    if let Some(saved) = read_settings(&backup)? {
        ensure!(
            saved == original,
            "已有不同的旧设置备份，未覆盖；请本人处理"
        );
        return Ok(());
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("助手设置缺少父目录")?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(original)?;
    temporary.as_file().sync_all()?;
    // No-clobber is essential: another process's different backup must never be replaced.
    temporary
        .persist_noclobber(&backup)
        .map_err(|error| error.error)
        .context("备份旧助手设置失败，未提交设置")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_old_opt_outs_restores_fixed_policy_without_overwriting_preferences() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let original = Settings {
            use_profile: false,
            use_project: false,
            dynamic_host_priority: false,
            repair_blocked: false,
            dynamic_muted_support: false,
            reply_activity: ReplyActivity::Active,
            automatic: true,
            preferences: "保留自定义要求".into(),
            ..Settings::default()
        };
        original.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        let (loaded, _) = Settings::load(&path).unwrap();

        assert!(loaded.use_profile && loaded.use_project);
        assert!(loaded.dynamic_host_priority && loaded.repair_blocked);
        assert!(!loaded.dynamic_muted_support);
        assert_eq!(loaded.reply_activity, ReplyActivity::Active);
        assert!(loaded.automatic);
        assert_eq!(loaded.preferences, "保留自定义要求");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

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
                    format: 4,
                    api: ApiConfig {
                        model: "selected-model".into(),
                        ..ApiConfig::default()
                    },
                    name: "保留我的助手名".into(),
                    preferences: "保留自定义偏好，先列证据".into(),
                    use_profile: true,
                    use_project: true,
                    workspace: Some(PathBuf::from("/private/my-workspace")),
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
                    skills: vec!["my-skill".into()],
                    persona: Persona::Broadcaster,
                };
                let expected = Settings {
                    reply_activity: activity,
                    thank_gifts: gifts,
                    thank_likes: false,
                    thank_follows: follows,
                    thank_shares: shares,
                    mention_sender: mention,
                    repair_blocked: true,
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

    fn legacy_fixture(version: u8) -> (serde_json::Value, Settings) {
        let expected = Settings {
            name: "资料助手".into(),
            preferences: "先查资料；谨慎答复".into(),
            workspace: Some(std::env::temp_dir().join("danmu-migration-workspace")),
            reply_activity: ReplyActivity::Active,
            dynamic_muted_support: false,
            thank_gifts: true,
            thank_likes: true,
            thank_follows: true,
            thank_shares: true,
            mention_sender: true,
            blocked_words: vec!["禁词".into()],
            skills: vec!["evidence-skill".into()],
            persona: Persona::Broadcaster,
            ..Settings::default()
        };
        let mut old = serde_json::to_value(&expected).unwrap();
        let object = old.as_object_mut().unwrap();
        object.remove("api");
        object.insert("format".into(), version.into());
        object.insert("host".into(), "vs_code".into());
        object.insert("binary".into(), "/retired/editor".into());
        object.insert(
            "source".into(),
            serde_json::json!({"kind":"omp_profile","value":"legacy-profile"}),
        );
        object.insert("native_preferences".into(), serde_json::json!([
            {"host":"omp","binary":"/retired/omp","source":{"kind":"native_prompt","value":"/unread/private/rules"},"model":"old-model","thinking":"high"},
            {"host":"vs_code","binary":"/retired/editor","source":{"kind":"default"},"model":null,"thinking":null}
        ]));
        for key in ["automatic", "resume_on_start", "web_search"] {
            object.insert(key.into(), true.into());
        }
        for key in [
            "use_profile",
            "use_project",
            "dynamic_host_priority",
            "repair_blocked",
        ] {
            object.insert(key.into(), false.into());
        }
        if version == 2 {
            object.insert("topic".into(), "旧场主题绝不继承".into());
        }
        (old, expected)
    }

    #[test]
    fn legacy_formats_preserve_business_data_and_only_explicit_save_creates_exact_backup() {
        for version in [2, 3] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("assistant.json");
            let (old, expected) = legacy_fixture(version);
            let mut bytes = serde_json::to_vec_pretty(&old).unwrap();
            bytes.extend_from_slice(b"\n \n");
            std::fs::write(&path, &bytes).unwrap();
            let (loaded, notice) = Settings::load(&path).unwrap();
            assert_eq!(loaded, expected);
            assert!(notice.is_some());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let backup = root.path().join("assistant.pre-api.json");
            assert!(!backup.exists());
            loaded.save(&path).unwrap();
            assert_eq!(std::fs::read(&backup).unwrap(), bytes);
            assert_eq!(Settings::load(&path).unwrap(), (expected.clone(), None));
            loaded.save(&path).unwrap();
            assert_eq!(std::fs::read(&backup).unwrap(), bytes);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                for file in [&path, &backup] {
                    assert_eq!(
                        std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
            }
        }
    }

    #[test]
    fn unversioned_schema_merges_only_style_and_scope_and_retires_authorization() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let bytes = r#" {"host":"vs_code","binary":"/retired/editor","name":"旧助手","style":"short","topic":"retired-topic","scope":"evidence","model":"retired-model","strength":"high","automatic":true} "#.as_bytes();
        std::fs::write(&path, bytes).unwrap();
        let (loaded, notice) = Settings::load(&path).unwrap();
        assert_eq!(loaded.name, "旧助手");
        assert_eq!(loaded.preferences, "short；evidence");
        assert_eq!(loaded.api, ApiConfig::default());
        assert!(!loaded.automatic && !loaded.resume_on_start && !loaded.web_search);
        assert!(notice.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        loaded.save(&path).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("assistant.pre-api.json")).unwrap(),
            bytes
        );
        assert_eq!(
            Settings::decode(br#"{"style":"only"}"#)
                .unwrap()
                .0
                .preferences,
            "only"
        );
        assert_eq!(
            Settings::decode(br#"{"name":""}"#).unwrap().0.name,
            Settings::default().name
        );
    }

    #[test]
    fn missing_legacy_fields_use_the_frozen_snapshot() {
        for input in [br#"{}"#.as_slice(), br#"{"format":2}"#, br#"{"format":3}"#] {
            let (loaded, notice) = Settings::decode(input).unwrap();
            assert_eq!(loaded, Settings::default());
            assert!(notice.is_some());
        }
    }

    #[test]
    fn strict_versions_types_and_nested_legacy_shapes_are_rejected_without_writes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let mut cases = vec![
            serde_json::json!({"unknown":1}),
            serde_json::json!({"format":1}),
            serde_json::json!({"format":5}),
            serde_json::json!({"format":"3"}),
            serde_json::json!({"format":null}),
            serde_json::json!({"automatic":"true"}),
            serde_json::json!({"format":3,"topic":"not accepted"}),
            serde_json::json!({"format":2,"topic":null}),
            serde_json::json!({"format":4,"host":"omp"}),
            serde_json::json!({"format":4,"source":{"kind":"default"}}),
            serde_json::json!({"format":4,"api":{"unknown":true}}),
        ];
        for version in [2, 3] {
            for overlay in [
                serde_json::json!({"unknown":true}),
                serde_json::json!({"name":false}),
                serde_json::json!({"host":"not-a-host"}),
                serde_json::json!({"binary":12}),
                serde_json::json!({"source":{"kind":"default","unknown":true}}),
                serde_json::json!({"source":{"kind":"omp_profile","value":1}}),
                serde_json::json!({"source":{"kind":"native_prompt","value":[],"extra":1}}),
                serde_json::json!({"native_preferences":[{"host":"omp","binary":"","source":{"kind":"default"},"extra":true}]}),
                serde_json::json!({"native_preferences":[{"host":"omp","binary":""}]}),
                serde_json::json!({"native_preferences":[{"host":"omp","binary":"","source":{"kind":"default"},"model":1}]}),
                serde_json::json!({"native_preferences":[{"host":"omp","binary":"","source":{"kind":"default","extra":1}}]}),
            ] {
                let mut case = serde_json::json!({"format":version});
                case.as_object_mut()
                    .unwrap()
                    .extend(overlay.as_object().unwrap().clone());
                cases.push(case);
            }
        }
        for case in cases {
            let bytes = serde_json::to_vec(&case).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(Settings::load(&path).is_err(), "{case}");
            assert!(Settings::default().save(&path).is_err(), "{case}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(!root.path().join("assistant.pre-api.json").exists());
        }
    }

    #[test]
    fn corrupted_and_oversized_settings_are_never_overwritten_and_limit_is_inclusive() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let mut boundary = b"{\"format\":3}".to_vec();
        boundary.resize(32768, b' ');
        std::fs::write(&path, &boundary).unwrap();
        assert!(Settings::load(&path).is_ok());
        for bytes in [b"{broken".to_vec(), {
            let mut value = boundary.clone();
            value.push(b' ');
            value
        }] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(Settings::load(&path).is_err());
            assert!(Settings::default().save(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(&path).unwrap();
        // A legal collection can still exceed the serialized file budget through JSON escaping.
        let too_large = Settings {
            skills: (0..16).map(|i| format!("s{i:063}")).collect(),
            blocked_words: (0..128).map(|_| "\\".repeat(128)).collect(),
            preferences: "p".repeat(8192),
            ..Settings::default()
        };
        assert!(too_large.validate().is_ok());
        assert!(too_large.save(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn backup_conflict_and_backup_io_failure_leave_old_settings_untouched() {
        for directory_failure in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("assistant.json");
            let backup = root.path().join("assistant.pre-api.json");
            let bytes = b"{\"format\":3,\"preferences\":\"keep\"}";
            std::fs::write(&path, bytes).unwrap();
            if directory_failure {
                std::fs::create_dir(&backup).unwrap();
            } else {
                crate::storage::write_private_atomic(&backup, b"different backup").unwrap();
            }
            let loaded = Settings::load(&path).unwrap().0;
            assert!(loaded.save(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            if !directory_failure {
                assert_eq!(std::fs::read(&backup).unwrap(), b"different backup");
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let bytes = b"{\"format\":2}";
        std::fs::write(&path, bytes).unwrap();
        crate::storage::write_private_atomic(&root.path().join("assistant.pre-api.json"), bytes)
            .unwrap();
        Settings::load(&path).unwrap().0.save(&path).unwrap();
        assert!(Settings::load(&path).unwrap().1.is_none());
    }

    #[test]
    fn failed_commit_retains_the_previous_file_and_does_not_create_a_backup_for_format4() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let original = Settings::default();
        original.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let invalid = Settings {
            name: String::new(),
            ..original
        };
        assert!(invalid.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(!root.path().join("assistant.pre-api.json").exists());
        let directory = root.path().join("blocked.json");
        std::fs::create_dir(&directory).unwrap();
        assert!(Settings::default().save(&directory).is_err());
        assert!(directory.is_dir());
    }

    #[test]
    fn format4_reopen_preserves_authorization_search_and_normalizes_the_selected_connection() {
        for protocol in [
            ApiProtocol::OpenaiChat,
            ApiProtocol::AnthropicMessages,
            ApiProtocol::ChatgptSubscription,
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("assistant.json");
            let settings = Settings {
                automatic: true,
                resume_on_start: true,
                web_search: true,
                api: ApiConfig {
                    protocol,
                    base_url: if protocol == ApiProtocol::ChatgptSubscription {
                        " HTTPS://API.OPENAI.COM:443/v1/// ".into()
                    } else {
                        " HTTPS://EXAMPLE.COM:443/prefix/v1/// ".into()
                    },
                    model: " selected-model ".into(),
                    max_output_tokens: 2048,
                    openai_token_limit: OpenaiTokenLimit::MaxTokens,
                    chatgpt_account: Some("local-account-id".into()),
                },
                ..Settings::default()
            };
            settings.save(&path).unwrap();
            let (loaded, notice) = Settings::load(&path).unwrap();
            assert!(loaded.automatic && loaded.resume_on_start && loaded.web_search);
            assert!(notice.is_none());
            let mut expected = settings;
            expected.api.normalize().unwrap();
            assert_eq!(loaded, expected);
            assert!(loaded.ready().is_ok());
            assert!(!root.path().join("assistant.pre-api.json").exists());
        }
    }

    #[test]
    fn subscription_requires_official_scope_and_an_account_without_paid_fallback() {
        let mut config = ApiConfig {
            protocol: ApiProtocol::ChatgptSubscription,
            model: "catalog-model".into(),
            ..ApiConfig::default()
        };
        assert!(config.validate().is_ok());
        assert!(config.ready().is_err());
        config.chatgpt_account = Some("account-id".into());
        assert!(config.ready().is_ok());
        for invalid in [
            "https://example.com/v1",
            "http://127.0.0.1/v1",
            "https://api.openai.com/v1/responses",
            "https://api.openai.com:444/v1",
        ] {
            config.base_url = invalid.into();
            assert!(config.validate().is_err());
        }
        config.base_url.clear();
        assert!(config.validate().is_ok());
        assert!(config.ready().is_err());
        config.chatgpt_account = Some(" ".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn all_known_legacy_hosts_and_sources_are_parsed_as_inert_data() {
        for host in [
            "omp",
            "claude",
            "codex",
            "open_code",
            "gemini",
            "cursor",
            "copilot",
            "amp",
            "pi",
            "dsh",
            "vs_code",
        ] {
            for source in [
                serde_json::json!({"kind":"default"}),
                serde_json::json!({"kind":"omp_profile","value":"old-profile"}),
                serde_json::json!({"kind":"native_prompt","value":"/never-open-this-native-file"}),
            ] {
                for version in [2, 3] {
                    let (mut old, expected) = legacy_fixture(version);
                    old["host"] = host.into();
                    old["source"] = source.clone();
                    old["native_preferences"] = serde_json::json!([
                        {"host":host,"binary":"/never-execute-this-host","source":source},
                        {"host":host,"binary":"/never-execute-this-host","source":source,"model":"retired","thinking":null}
                    ]);
                    assert_eq!(
                        Settings::decode(&serde_json::to_vec(&old).unwrap())
                            .unwrap()
                            .0,
                        expected
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_backup_paths_are_rejected_without_overwriting_the_original_or_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for linked in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("assistant.json");
            let backup = root.path().join("assistant.pre-api.json");
            let outside = root.path().join("outside");
            let bytes = b"{\"format\":3}";
            std::fs::write(&path, bytes).unwrap();
            std::fs::write(&outside, b"private target").unwrap();
            if linked {
                symlink(&outside, &backup).unwrap();
            } else {
                std::fs::write(&backup, bytes).unwrap();
                std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            assert!(Settings::load(&path).unwrap().0.save(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(std::fs::read(&outside).unwrap(), b"private target");
        }
    }

    #[test]
    fn api_field_and_url_budgets_are_enforced_before_commit() {
        for tokens in [256, 8192] {
            assert!(
                ApiConfig {
                    max_output_tokens: tokens,
                    ..ApiConfig::default()
                }
                .validate()
                .is_ok()
            );
        }
        let model = "m".repeat(256);
        assert!(
            ApiConfig {
                model: model.clone(),
                ..ApiConfig::default()
            }
            .ready()
            .is_ok()
        );
        assert!(
            ApiConfig {
                model: model + "m",
                ..ApiConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(normalize_base_url(&format!("https://example.com/{}", "p".repeat(2048))).is_err());
        assert!(normalize_base_url("https://example.com/\t").is_err());
        for field in ["binary", "native_preferences", "topic"] {
            let mut value = serde_json::json!({"format":4});
            value[field] = serde_json::Value::Null;
            assert!(Settings::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
}
