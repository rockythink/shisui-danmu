#[cfg(test)]
use crate::runtime::{
    ChatgptOperation, DELIVERY_NOTICE_LIFETIME, MainLogin, PendingDelivery,
    reconcile_cross_origin_event,
};
use crate::runtime::{CoreEvent, DeliveryStatus, NoticeLevel, RuntimeState, StopFlow};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecretTarget {
    ObsPassword,
    AssistantApiKey,
}

pub(crate) mod activity;
pub(crate) mod agent;
pub(crate) mod assistant;
use crate::runtime::settings;
#[cfg(all(test, windows))]
mod windows_tests;

pub use agent::run_local as local;
pub(crate) mod input;
mod meter;
mod qr;
mod replay;
pub use replay::run as replay;

pub use qr::qr_lines;

pub(crate) fn login_qr_lines(url: &str) -> Result<Vec<String>> {
    compact_qr_lines(url)
}

#[cfg(test)]
use crate::{
    bilibili::BilibiliClientEvent,
    bridge::Bridge,
    delivery::Outcome,
    obs::{ObsController, ObsStatus},
    persistence::SessionJournal,
};
use crate::{
    bilibili::{
        AccountClient, AccountStatus, LoginPoll, RoomLiveStatus, RoomSnapshot, kind_label,
        segment_message, usable_author_id,
    },
    bridge,
    config::TerminalConfig,
    domain::{DanmuEvent, DanmuEventKind, DanmuEventOrigin, DanmuSession, DanmuSessionEndReason},
    obs::{MicrophoneLevel, MicrophoneState},
    theme::Palette,
};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, Utc};
#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use input::EditorInput;
use meter::microphone_meter;
use qr::{compact_qr_lines, draw_qr};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    io::{self, Stdout},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
#[cfg(test)]
use tokio::sync::oneshot;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// Register once and keep the receivers alive across all startup/main-loop draws.
fn shutdown_signal() -> io::Result<impl Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        Ok(async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
                _ = hangup.recv() => {},
            }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupCheck {
    Waiting,
    Running,
    Passed(&'static str),
    Skipped(&'static str),
    Warning(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StartupView {
    pub(crate) local: StartupCheck,
    pub(crate) account: StartupCheck,
    pub(crate) room: StartupCheck,
    pub(crate) metrics: StartupCheck,
    pub(crate) obs: StartupCheck,
}

impl StartupView {
    pub(crate) fn new() -> Self {
        Self {
            local: StartupCheck::Running,
            account: StartupCheck::Running,
            room: StartupCheck::Waiting,
            metrics: StartupCheck::Skipped("进入后后台读取"),
            obs: StartupCheck::Running,
        }
    }

    pub(crate) fn active_label(self) -> &'static str {
        if self.local == StartupCheck::Running {
            "恢复本地会话"
        } else if self.account == StartupCheck::Running {
            "检查登录状态"
        } else if self.room == StartupCheck::Running {
            "检查直播间　"
        } else if self.obs == StartupCheck::Running {
            "后台检查OBS连接"
        } else {
            "启动自检完成"
        }
    }
}

#[derive(Debug, Default)]
pub(crate) enum StartupGate {
    #[default]
    Checking,
    Ready,
    Blocked,
    EnteringObsPassword(EditorInput),
    Skipped,
}

impl StartupGate {
    pub(crate) fn can_finish(&self) -> bool {
        matches!(self, Self::Ready | Self::Skipped)
    }
}

impl StartupView {
    pub(crate) fn has_warning(self) -> bool {
        [self.local, self.room]
            .into_iter()
            .any(|status| matches!(status, StartupCheck::Warning(_)))
    }

    pub(crate) fn obs_requires_password(self) -> bool {
        matches!(self.obs, StartupCheck::Warning(message) if message.contains("OBS 已启用认证时"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandCategory {
    Display,
    Account,
    Obs,
    Ai,
    System,
}

impl CommandCategory {
    pub(crate) fn color(self, palette: Palette) -> Color {
        match self {
            Self::Display => palette.host,
            Self::Account => palette.info,
            Self::Obs => palette.name,
            Self::Ai => palette.success,
            Self::System => palette.content,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandSpec {
    pub(crate) completion: &'static str,
    pub(crate) usage: &'static str,
    pub(crate) description: &'static str,
    pub(crate) category: CommandCategory,
    pub(crate) danger: bool,
}

enum SlashKeyAction {
    Ignored,
    Handled,
    Submit(String),
}

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        completion: "/mute",
        usage: "静音麦克风",
        description: "关闭声音；重复执行仍静音",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/unmute",
        usage: "恢复麦克风",
        description: "声音将公开",
        category: CommandCategory::Obs,
        danger: true,
    },
    CommandSpec {
        completion: "/scene",
        usage: "切换场景",
        description: "输入场景名称",
        category: CommandCategory::Obs,
        danger: true,
    },
    CommandSpec {
        completion: "/commands",
        usage: "指令",
        description: "中文搜索，保留草稿",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/more",
        usage: "更多",
        description: "OBS、诊断、帮助、退出",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/settings",
        usage: "设置与操作",
        description: "四类设置、本场与常用操作",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/diag",
        usage: "诊断",
        description: "历史告警与连接信息",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/pin",
        usage: "重点消息",
        description: "标记选中或最新消息",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/find",
        usage: "搜索归档",
        description: "输入关键词；也可直接带参数",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/obs ",
        usage: "OBS",
        description: "已有连接与控制操作",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/help",
        usage: "帮助",
        description: "操作说明",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/quit",
        usage: "退出弹幕台",
        description: "不停止推流",
        category: CommandCategory::System,
        danger: true,
    },
    CommandSpec {
        completion: "/ai",
        usage: "AI助手",
        description: "打开运行面板，不启动",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/review",
        usage: "待审回复",
        description: "全文审核后发送",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/pause",
        usage: "暂停AI",
        description: "撤权，保留可继续状态",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai start",
        usage: "启动AI",
        description: "连接工具，处理新弹幕",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai auto on",
        usage: "自动发送",
        description: "公开发送 · 同房间同账号记住",
        category: CommandCategory::Ai,
        danger: true,
    },
    CommandSpec {
        completion: "/ai auto off",
        usage: "逐条发送",
        description: "立即撤销自动许可",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai range",
        usage: "本场范围",
        description: "仅改变当前场次",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai topic",
        usage: "本场主题",
        description: "仅影响AI上下文，不修改直播间",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai visible",
        usage: "处理可见弹幕",
        description: "处理尚未消费的可见内容",
        category: CommandCategory::Ai,
        danger: true,
    },
    CommandSpec {
        completion: "/ai key",
        usage: "隐藏输入 API Key",
        description: "设置当前 API 连接的私有 Key，不接收明文参数",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai stop",
        usage: "结束AI",
        description: "释放进程并撤销许可",
        category: CommandCategory::Ai,
        danger: true,
    },
    CommandSpec {
        completion: "/diag repair",
        usage: "重建历史",
        description: "先备份，再修复副本",
        category: CommandCategory::System,
        danger: true,
    },
    CommandSpec {
        completion: "/ai model",
        usage: "模型",
        description: "AI工具与原生选项",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai replies",
        usage: "发送策略",
        description: "方案、联网搜索与屏蔽词",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai materials",
        usage: "人设与资料",
        description: "AI名字与人设",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/ai advanced",
        usage: "维护",
        description: "可见弹幕、重建上下文与结束",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/about",
        usage: "关于弹幕台",
        description: "官网、创始人、版本与源码",
        category: CommandCategory::System,
        danger: false,
    },
    CommandSpec {
        completion: "/room title",
        usage: "直播间标题",
        description: "公开修改B站标题；不改AI主题",
        category: CommandCategory::Account,
        danger: true,
    },
    CommandSpec {
        completion: "/room cover",
        usage: "直播间封面",
        description: "上传本地图片并提交审核",
        category: CommandCategory::Account,
        danger: true,
    },
    CommandSpec {
        completion: "/display theme",
        usage: "主题",
        description: "选择界面主题",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display names on",
        usage: "显示昵称",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display names off",
        usage: "隐藏昵称",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display time on",
        usage: "显示时间",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display time off",
        usage: "隐藏时间",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display layout chat",
        usage: "聊天布局",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
    CommandSpec {
        completion: "/display layout list",
        usage: "列表布局",
        description: "保存显示设置",
        category: CommandCategory::Display,
        danger: false,
    },
];

const OBS_COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        completion: "/obs config host",
        usage: "接入地址",
        description: "编辑OBS WebSocket主机名或IP",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs config port",
        usage: "接入端口",
        description: "编辑OBS WebSocket端口",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs status",
        usage: "连接状态",
        description: "查看OBS",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs connect",
        usage: "检查连接",
        description: "检查OBS",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs config mic",
        usage: "麦克风配置",
        description: "输入麦克风名称",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs config password",
        usage: "连接密码",
        description: "编辑私有密码",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/obs start",
        usage: "开始推流",
        description: "立即开始直播",
        category: CommandCategory::Obs,
        danger: true,
    },
    CommandSpec {
        completion: "/obs stop",
        usage: "停止推流",
        description: "直播将中断",
        category: CommandCategory::Obs,
        danger: true,
    },
];

const SETTINGS_COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        completion: "/settings account",
        usage: "B站账号与直播间",
        description: "主账号、AI发送账号、标题与封面",
        category: CommandCategory::Account,
        danger: false,
    },
    CommandSpec {
        completion: "/settings obs",
        usage: "OBS设置",
        description: "接入配置与当前控制",
        category: CommandCategory::Obs,
        danger: false,
    },
    CommandSpec {
        completion: "/settings ai",
        usage: "AI助手设置",
        description: "模型、发送策略与维护",
        category: CommandCategory::Ai,
        danger: false,
    },
    CommandSpec {
        completion: "/settings system",
        usage: "系统与关于",
        description: "工具信息、诊断与修复",
        category: CommandCategory::System,
        danger: false,
    },
];

fn command_value(mut raw: &str, tokens: usize) -> Option<&str> {
    for _ in 0..tokens {
        raw = raw.trim_start();
        raw = &raw[raw.find(char::is_whitespace).unwrap_or(raw.len())..];
    }
    let value = raw.trim();
    (!value.is_empty()).then_some(value)
}

fn ai_command(spec: &CommandSpec) -> bool {
    matches!(spec.completion, "/ai" | "/review" | "/pause")
        || spec.completion.starts_with("/ai ")
        || spec.completion == "/settings ai"
}

fn obs_command(command: &str) -> bool {
    matches!(command, "/obs" | "/mute" | "/unmute" | "/scene") || command.starts_with("/obs ")
}

fn command_suggestions(input: &str, include_ai: bool) -> Vec<&'static CommandSpec> {
    let query = input.strip_prefix('/').unwrap_or(input);
    let (primary, settings, obs): (&[CommandSpec], &[CommandSpec], &[CommandSpec]) =
        if input.starts_with("/obs ") {
            (OBS_COMMAND_SPECS, &[], &[])
        } else if input.starts_with("/settings ") {
            (SETTINGS_COMMAND_SPECS, &[], &[])
        } else {
            (COMMAND_SPECS, SETTINGS_COMMAND_SPECS, OBS_COMMAND_SPECS)
        };
    let show_ai = include_ai || !query.trim().is_empty();
    let commands = primary
        .iter()
        .chain(settings)
        .chain(obs)
        .filter(|spec| show_ai || !ai_command(spec))
        .filter(|spec| {
            !query.is_empty()
                || matches!(
                    spec.completion,
                    "/mute"
                        | "/unmute"
                        | "/scene"
                        | "/ai"
                        | "/review"
                        | "/pause"
                        | "/pin"
                        | "/find"
                        | "/settings"
                        | "/diag"
                        | "/help"
                        | "/quit"
                )
        });
    let prefixes: Vec<_> = commands
        .clone()
        .filter(|spec| spec.completion.trim_start_matches('/').starts_with(query))
        .collect();
    if !prefixes.is_empty() {
        return prefixes;
    }
    let titles: Vec<_> = commands
        .clone()
        .filter(|spec| spec.usage.contains(query))
        .collect();
    if !titles.is_empty() {
        return titles;
    }
    commands
        .filter(|spec| spec.description.contains(query))
        .collect()
}

#[derive(Default)]
pub(crate) struct HelpState {
    pub(crate) scroll: usize,
    pub(crate) max_scroll: usize,
    pub(crate) page_rows: usize,
}

const HELP_SECTIONS: &[(&str, &str)] = &[
    (
        "常用",
        "输入 / 只显示常用操作；输入中文或命令前缀搜索全部，↑↓选择、Enter执行、Tab补全。/mute静音，/unmute恢复声音，/scene切场景，/diag看诊断。Ctrl-O独立搜索并保留草稿与光标。低频配置在/settings，AI运行操作在/ai。
普通输入 Enter 人工发送；粘贴只插入、不执行。Shift-↑/↓选择回复对象，Enter插入@；滚轮或↑↓浏览历史，Esc / End回最新。主界面直接拖选文字，用终端复制快捷键（macOS为⌘C）；↶表示重启前的弹幕。/pin标记消息；/find 打开关键词编辑，/find [关键词]直接搜索归档。",
    ),
    (
        "设置与回复",
        "设置用方向键选择、Enter执行、Esc返回；“设置”“当前操作”为只读分组。直达页Esc一次关闭。Tab切换同级分类，F1或?展开说明。编辑Enter保存、Esc取消；归档搜索必须输入关键词，取消不搜索。
/display theme选择主题；/display names或time on|off；/display layout chat|list保存布局。/settings ai集中AI配置；/ai model选模型，/ai replies选方案、联网与屏蔽词，/ai materials改名字与人设，/ai advanced维护。工作区、来源、技能与补充要求在assistant.json配置；资料引用、开麦让位与有限修复固定启用。
/review查看候选全文；Shift-Enter发送本条，e编辑，Delete丢弃，Tab下一条。长文须先下滑看完；编辑只保存，须重新看完才可发送。",
    ),
    (
        "AI运行",
        "/ai start 启动并等待新消息；/pause 暂停并撤权；/ai stop 释放本机 worker。/ai range 本场范围，/ai topic 本场主题。\n/ai auto on 允许公开自动发送，同房间同账号记住；/ai auto off 立即撤权。旧自动偏好不是许可。/ai visible 处理可见弹幕；/ai key 打开隐藏 Key 输入。/diag repair 先备份再重建历史。",
    ),
    (
        "指令与编辑",
        "指令搜索用↑↓选择、Enter执行；Tab只补全，带参数命令不猜参数。Ctrl-O、Ctrl-G、Ctrl-P、Ctrl-C仍兼容；不需要鼠标点击。\n←/→移光标，Home/End到首尾，Alt-←/→跳词。Backspace/Delete删除，Ctrl-U清空，Ctrl-W删前词，Ctrl-K删到末尾。单次粘贴最多4096字节，控制字符过滤。",
    ),
    (
        "状态与诊断",
        "输入框右侧保留人数、点赞与在线数；-- 表示暂无可靠数据。✓送达，?送达未确认且不自动重发，×被拒。AI逐条为绿色，未授权为黄色，自动为红色。\n/settings system提供诊断和修复；/about查看官网、创始人和版本。/diag保留历史告警、真实路径与连接细节，不扫描工具。人工发送失败仅结束本次及旧排队任务，后续新输入可继续发送；未确认消息不自动重发，登录失效仍须重新登录。",
    ),
    (
        "OBS与退出",
        "/settings obs配置接入地址、端口、密码和输入；/obs config host、port、mic不带参数打开编辑器，带值立即保存；/obs connect检查连接。\n/mute静音；/unmute公开声音；/scene [名称]编辑或切换场景；/obs start开始推流。只有/obs stop二次确认，确认后3秒内可取消。/quit退出弹幕台，不停止推流。\n/room title [标题]公开修改直播间标题，/room cover [图片路径]提交封面；仅主账号本人的直播间可修改，审核中不代表已生效。本地模式拒绝真实账号、直播间修改与OBS。",
    ),
];

#[cfg(test)]
static SUBSCRIPTION_TEST_BROWSER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
fn open_chatgpt_browser(url: &str) -> Result<()> {
    #[cfg(test)]
    if let Some(path) = SUBSCRIPTION_TEST_BROWSER.get() {
        let mut authorize = url::Url::parse(url).map_err(|_| anyhow!("测试授权地址无效"))?;
        anyhow::ensure!(
            authorize.scheme() == "http" && authorize.host_str() == Some("127.0.0.1"),
            "测试授权地址须为loopback"
        );
        let nonce = authorize
            .query_pairs()
            .find(|(key, _)| key == "nonce")
            .map(|(_, value)| value.into_owned())
            .ok_or_else(|| anyhow!("测试nonce缺失"))?;
        let client = authorize
            .query_pairs()
            .find(|(key, _)| key == "client_id")
            .map(|(_, value)| value.into_owned())
            .ok_or_else(|| anyhow!("测试client缺失"))?;
        let pairs: Vec<(String, String)> = authorize
            .query_pairs()
            .filter(|(key, _)| key != "id_token_hint")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        authorize.set_query(None);
        authorize.query_pairs_mut().extend_pairs(pairs);
        let issued = if client == "dynamic_agent_client" {
            "oaiapp_fixture"
        } else {
            &client
        };
        let record = serde_json::json!({"authorize_url": authorize.as_str(),
            "token_response": crate::runner::subscription::fixture_login_token_response(&nonce, issued, "fixture-subject", true)});
        crate::storage::write_private_atomic(
            path,
            &serde_json::to_vec(&record).map_err(|_| anyhow!("测试授权记录编码失败"))?,
        )
        .map_err(|_| anyhow!("私有测试授权通道写入失败"))?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    let status = command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| anyhow!("无法打开系统浏览器；未输出授权地址，请重试"))?;
    anyhow::ensure!(
        status.success(),
        "系统浏览器打开失败；未输出授权地址，请重试"
    );
    Ok(())
}
impl RuntimeState {
    pub(crate) fn begin_assistant_api_key(&mut self) -> Result<()> {
        self.check_assistant_connection_change()?;
        anyhow::ensure!(
            self.runner.settings.api.protocol
                != crate::runner::settings::ApiProtocol::ChatgptSubscription,
            "ChatGPT订阅使用浏览器登录，不使用API Key"
        );
        anyhow::ensure!(self.secret_target.is_none(), "已有隐藏输入，请先保存或取消");
        if self.command_search_draft.is_some() {
            self.close_command_search();
        }
        self.secret_draft = Some(std::mem::take(&mut self.input));
        self.secret_target = Some(SecretTarget::AssistantApiKey);
        self.set_notice(
            "输入当前连接的 API Key；Enter保存，Esc取消；更换将替换唯一Key记录，不公开发送",
            NoticeLevel::Progress,
        );
        Ok(())
    }

    pub(crate) fn record_reading_config(&mut self) {
        self.runner
            .record_reading_config(crate::workspace_config::Reading {
                single_line: self.config.single_line,
                chat_layout: self.layout_chat,
                show_time: self.show_time,
                show_name: self.show_name,
                history_idle_seconds: self.config.history_idle_seconds,
                theme: self.config.theme_name.clone(),
            });
        if let Some(warning) = self.runner.config_warning.clone() {
            self.set_notice(warning, NoticeLevel::Error);
        }
    }
    pub(crate) fn persist_ui(&mut self, key: &str, value: toml_edit::Value) -> bool {
        match self.config.save_value(key, value) {
            Ok(()) => true,
            Err(error) => {
                self.set_notice(format!("配置未保存：{error}"), NoticeLevel::Error);
                false
            }
        }
    }

    pub(crate) fn is_browsing_history(&self) -> bool {
        self.selection_active || self.scroll_offset > 0
    }

    pub(crate) fn advance_history_at(&mut self, now: Instant) {
        // Modal operations must not silently discard a reply target underneath them.
        if self.stop_flow.is_some()
            || self.login_qr.is_some()
            || self.secret_target.is_some()
            || self.candidate_edit.is_some()
            || self.assistant_panel.is_some()
            || self.help.is_some()
        {
            self.last_user_activity = now;
            return;
        }
        if self.is_browsing_history()
            && self.config.history_idle_seconds > 0
            && now.saturating_duration_since(self.last_user_activity)
                >= Duration::from_secs(u64::from(self.config.history_idle_seconds))
        {
            self.return_to_live();
            self.set_notice("FOLLOW", NoticeLevel::Info);
        }
    }

    pub(crate) fn return_to_live(&mut self) {
        self.selection_active = false;
        self.selected = 0;
        self.scroll_offset = 0;
        self.unread_live_count = 0;
    }

    pub(crate) fn handle_mouse(&mut self, event: MouseEvent) -> bool {
        if self.stop_flow.is_some()
            || self.login_qr.is_some()
            || self.secret_target.is_some()
            || self.candidate_edit.is_some()
            || self.main_login.is_some()
            || self.assistant_accounts.login_pending()
        {
            return false;
        }
        if matches!(
            event.kind,
            MouseEventKind::Down(crossterm::event::MouseButton::Left)
        ) {
            if self.help.is_some() || self.command_search_draft.is_some() {
                return false;
            }
            self.last_user_activity = Instant::now();
            return self.assistant_click(event);
        }
        if !matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return false;
        }
        self.last_user_activity = Instant::now();
        if let Some(help) = self.help.as_mut() {
            match event.kind {
                MouseEventKind::ScrollUp => help.scroll = help.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => {
                    help.scroll = help.scroll.saturating_add(3).min(help.max_scroll)
                }
                _ => {}
            }
        } else if self.command_search_draft.is_some() || self.input.starts_with('/') {
            let count = self.command_suggestions().len();
            if count > 0 {
                self.slash_selection = if event.kind == MouseEventKind::ScrollUp {
                    self.slash_selection.saturating_sub(1)
                } else {
                    (self.slash_selection + 1).min(count - 1)
                };
            }
        } else if self.assistant_panel.is_some() {
            self.assistant_mouse(event);
        } else {
            self.scroll_history(event.kind == MouseEventKind::ScrollUp);
        }
        false
    }

    pub(crate) fn open_commands(&mut self) {
        if self.command_search_draft.is_none() {
            self.command_search_draft = Some(std::mem::take(&mut self.input));
        }
        self.slash_selection = 0;
        self.clear_review_surface();
    }

    pub(crate) fn scroll_history(&mut self, older: bool) {
        let mut candidates = self
            .session
            .recent_events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.kind.activity_lifetime().is_none())
            .map(|(index, _)| index);
        let Some(newest) = candidates.next() else {
            self.return_to_live();
            return;
        };
        let anchor = if self.selection_active {
            self.selected
        } else {
            self.scroll_offset
        }
        .max(newest);
        let next = if older {
            candidates.find(|index| *index > anchor).unwrap_or(anchor)
        } else {
            candidates
                .rev()
                .find(|index| *index < anchor)
                .unwrap_or(newest)
        };
        if next == newest {
            self.return_to_live();
        } else {
            self.selection_active = false;
            self.selected = 0;
            self.scroll_offset = next;
        }
    }

    fn handle_slash_key(&mut self, key: &KeyEvent) -> SlashKeyAction {
        let searching = self.command_search_draft.is_some();
        if self.secret_target.is_some() || (!searching && !self.input.starts_with('/')) {
            return SlashKeyAction::Ignored;
        }

        let suggestions = self.command_suggestions();
        let move_up = key.code == KeyCode::Up;
        let move_down = key.code == KeyCode::Down
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('n'));

        if key.code == KeyCode::Esc {
            self.close_command_search();
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        if suggestions.is_empty() {
            if searching
                && matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Tab | KeyCode::Up | KeyCode::Down
                )
            {
                if key.code == KeyCode::Enter && self.input.starts_with('/') {
                    let command = sanitize_input(self.input.take());
                    self.close_command_search();
                    return SlashKeyAction::Submit(command);
                }
                return SlashKeyAction::Handled;
            }
            return SlashKeyAction::Ignored;
        }
        if move_up {
            self.slash_selection = self
                .slash_selection
                .checked_sub(1)
                .unwrap_or(suggestions.len() - 1);
            return SlashKeyAction::Handled;
        }
        if move_down {
            self.slash_selection = (self.slash_selection + 1) % suggestions.len();
            return SlashKeyAction::Handled;
        }
        let selected = self.slash_selection.min(suggestions.len() - 1);
        if key.code == KeyCode::Tab {
            self.input
                .replace(suggestions[selected].completion.to_owned());
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        if key.code == KeyCode::Enter {
            return self.activate_palette_row(selected);
        }
        SlashKeyAction::Ignored
    }

    fn activate_palette_row(&mut self, index: usize) -> SlashKeyAction {
        let suggestions = self.command_suggestions();
        let Some(suggestion) = suggestions.get(index) else {
            return SlashKeyAction::Handled;
        };
        let completion = suggestion.completion;
        let raw = self.input.trim();
        let has_arguments = raw.starts_with(completion.trim_end())
            && raw.len() > completion.trim_end().len()
            && raw.as_bytes().get(completion.trim_end().len()) == Some(&b' ');
        if completion.ends_with(' ') && !has_arguments {
            self.input.replace(completion.to_owned());
            self.slash_selection = 0;
            return SlashKeyAction::Handled;
        }
        let command = if has_arguments {
            raw.to_owned()
        } else {
            completion.to_owned()
        };
        self.close_command_search();
        SlashKeyAction::Submit(sanitize_input(command))
    }

    fn command_suggestions(&self) -> Vec<&'static CommandSpec> {
        let include_ai = self.runner.has_saved_settings()
            || self.runner.running
            || self.bridge.candidate_count() != 0;
        if self.command_search_draft.is_some() || self.input.starts_with('/') {
            command_suggestions(&self.input, include_ai)
        } else {
            Vec::new()
        }
    }

    pub(crate) fn close_command_search(&mut self) {
        self.input = self.command_search_draft.take().unwrap_or_default();
        self.slash_selection = 0;
    }

    pub(crate) fn handle_stop_key(&mut self, key: KeyCode, now: Instant) {
        match self.stop_flow {
            Some(StopFlow::Confirm { stop_selected }) => match key {
                KeyCode::Up | KeyCode::Left => {
                    self.stop_flow = Some(StopFlow::Confirm {
                        stop_selected: true,
                    });
                }
                KeyCode::Down | KeyCode::Right => {
                    self.stop_flow = Some(StopFlow::Confirm {
                        stop_selected: false,
                    });
                }
                KeyCode::Enter if stop_selected => {
                    if let Some(token) = self.stop_confirmation_token() {
                        let operation_id = self.operation_token();
                        if let Err(error) = self.confirm_stop_request(token, now, operation_id) {
                            self.set_notice(error.to_string(), NoticeLevel::Error);
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Esc => self.cancel_stop(),
                _ => {}
            },
            Some(StopFlow::Countdown { .. }) if key == KeyCode::Esc => self.cancel_stop(),
            _ => {}
        }
    }

    pub(crate) fn cancel_stop(&mut self) {
        self.cancel_stop_request();
        self.set_notice("已取消停止推流", NoticeLevel::Info);
    }

    pub(crate) async fn handle_key(
        &mut self,
        key: KeyEvent,
        tx: mpsc::Sender<CoreEvent>,
    ) -> Result<bool> {
        self.ui_tx = Some(tx.clone());
        if key.kind == crossterm::event::KeyEventKind::Press
            && key.code == KeyCode::Esc
            && self.chatgpt_operation.is_some()
        {
            self.cancel_chatgpt_operation();
            self.set_notice(
                "已取消本机 ChatGPT 操作；不声称远端请求已取消",
                NoticeLevel::Info,
            );
            return Ok(false);
        }
        if key.kind == crossterm::event::KeyEventKind::Press
            && self.stop_flow.is_some()
            && !(key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'p')))
        {
            self.handle_stop_key(key.code, Instant::now());
            return Ok(false);
        }
        if key.kind == crossterm::event::KeyEventKind::Press {
            if self.secret_target.is_some()
                && matches!(
                    key.code,
                    KeyCode::Tab
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                )
            {
                return Ok(false);
            }
            if key.code == KeyCode::Esc && self.main_login.is_some() {
                self.cancel_main_login();
                self.set_notice("已取消主账号扫码，原账号未更改", NoticeLevel::Info);
                return Ok(false);
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(true);
            }
            if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.pause_assistant();
                return Ok(false);
            }
            if key.code == KeyCode::Esc && self.assistant_accounts.login_pending() {
                self.assistant_accounts.cancel_login();
                self.login_qr = None;
                self.set_notice("已取消助手扫码，原账号未更改", NoticeLevel::Info);
                return Ok(false);
            }
            if self.login_qr.is_some() {
                if key.code == KeyCode::Esc {
                    self.login_qr = None;
                }
                return Ok(false);
            }
            if self.handle_help_key(key.code) {
                return Ok(false);
            }
            if self.command_search_draft.is_none()
                && self.candidate_edit.is_none()
                && self.secret_target.is_none()
                && self.assistant_key(key, tx.clone()).await
            {
                return Ok(self.quit_requested);
            }
            if key.code == KeyCode::Char('g')
                && key.modifiers == KeyModifiers::CONTROL
                && self.command_search_draft.is_none()
                && self.secret_target.is_none()
            {
                if let Err(error) = self.open_assistant() {
                    self.set_notice(error.to_string(), NoticeLevel::Error);
                }
                return Ok(false);
            }
        }

        if key.kind != crossterm::event::KeyEventKind::Release {
            self.last_user_activity = Instant::now();
        }
        if key.kind != crossterm::event::KeyEventKind::Press {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT {
            if self.secret_target.is_some()
                || self.stop_flow.is_some()
                || self.login_qr.is_some()
                || self.command_search_draft.is_some()
            {
                return Ok(false);
            }
            self.review_key(key);
            return Ok(false);
        }
        if self.stop_flow.is_some() {
            self.handle_stop_key(key.code, Instant::now());
            return Ok(false);
        }
        if key.code == KeyCode::Char('o') && key.modifiers == KeyModifiers::CONTROL {
            if self.candidate_edit.is_none() && self.secret_target.is_none() {
                if self.command_search_draft.is_some() {
                    self.close_command_search();
                } else {
                    self.open_commands();
                }
            }
            return Ok(false);
        }

        if self.candidate_edit.is_some() {
            match key.code {
                KeyCode::Enter => {
                    self.save_candidate_edit();
                    return Ok(false);
                }
                KeyCode::Esc => {
                    self.finish_candidate_edit();
                    self.set_notice("已取消回复编辑；人工草稿已恢复", NoticeLevel::Info);
                    return Ok(false);
                }
                KeyCode::Tab | KeyCode::Up | KeyCode::Down => return Ok(false),
                _ => {}
            }
        }

        match if self.candidate_edit.is_some() {
            SlashKeyAction::Ignored
        } else {
            self.handle_slash_key(&key)
        } {
            SlashKeyAction::Ignored => {}
            SlashKeyAction::Handled => return Ok(false),
            SlashKeyAction::Submit(command) => {
                self.submit(command, tx).await?;
                return Ok(self.quit_requested);
            }
        }
        let selection_navigation = (key.modifiers.is_empty()
            || key.modifiers.contains(KeyModifiers::SHIFT))
            && matches!(key.code, KeyCode::Up | KeyCode::Down);
        if self.candidate_edit.is_none()
            && self.secret_target.is_none()
            && self.command_search_draft.is_none()
            && self.selection_active
            && !(self.input.starts_with('/')
                || (self.input.is_empty() && key.code == KeyCode::Char('/')))
            && !selection_navigation
            && !matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::End)
        {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => {
                    self.input.clear();
                    return Ok(false);
                }
                KeyCode::Char('a') => {
                    self.input.move_to_start();
                    return Ok(false);
                }
                KeyCode::Char('e') => {
                    self.input.move_to_end();
                    return Ok(false);
                }
                KeyCode::Char('w') => {
                    self.input.delete_previous_word();
                    self.slash_selection = 0;
                    return Ok(false);
                }
                KeyCode::Char('k') => {
                    self.input.delete_to_end();
                    self.slash_selection = 0;
                    return Ok(false);
                }
                KeyCode::Char('d') => {
                    self.input.delete_at_cursor();
                    self.slash_selection = 0;
                    return Ok(false);
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                if self.login_qr.take().is_some() {
                    return Ok(false);
                }
                if self.secret_target.is_some() {
                    self.secret_target = None;
                    self.input = self.secret_draft.take().unwrap_or_default();
                    return Ok(false);
                }
                if self.selection_active || self.scroll_offset > 0 {
                    self.return_to_live();
                    return Ok(false);
                }
                return Ok(false);
            }
            KeyCode::Tab => {
                if !self.persist_ui("chat_layout", (!self.layout_chat).into()) {
                    return Ok(false);
                }
                self.layout_chat = !self.layout_chat;
                self.set_notice(
                    format!(
                        "已切换为{}布局",
                        if self.layout_chat {
                            "聊天"
                        } else {
                            "信息流"
                        }
                    ),
                    NoticeLevel::Success,
                );
                self.record_reading_config();
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_selection(true),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_selection(false)
            }
            KeyCode::Up | KeyCode::Down
                if key.modifiers.is_empty() && self.secret_target.is_none() =>
            {
                self.scroll_history(key.code == KeyCode::Up);
            }
            KeyCode::Home => self.input.move_to_start(),
            KeyCode::End
                if self.candidate_edit.is_none()
                    && self.command_search_draft.is_none()
                    && self.secret_target.is_none()
                    && (self.selection_active || self.scroll_offset > 0) =>
            {
                self.return_to_live()
            }
            KeyCode::End => self.input.move_to_end(),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.move_word_left()
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.move_word_right()
            }
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Backspace => {
                self.input.delete_before_cursor();
                self.slash_selection = 0;
            }
            KeyCode::Delete => {
                self.input.delete_at_cursor();
                self.slash_selection = 0;
            }
            KeyCode::Enter => {
                if self.secret_target.is_none()
                    && self.selection_active
                    && !self.input.starts_with('/')
                {
                    let username = self
                        .session
                        .recent_events
                        .get(self.selected)
                        .and_then(|event| event.username.clone());
                    self.return_to_live();
                    if let Some(username) = username {
                        let needs_space = self
                            .input
                            .previous_grapheme()
                            .is_some_and(|value| !value.trim().is_empty());
                        self.insert_text(&format!(
                            "{}@{username} ",
                            if needs_space { " " } else { "" }
                        ));
                        self.set_notice(
                            format!("已选择 {username}，继续编辑后按 Enter 发送"),
                            NoticeLevel::Info,
                        );
                    }
                } else {
                    if self.secret_target.is_some() {
                        let secret = self.input.to_string();
                        self.submit(secret, tx).await?;
                    } else {
                        let command = sanitize_input(self.input.take());
                        if !command.is_empty() {
                            self.submit(command, tx).await?;
                        }
                    }
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(character)
            }
            _ => {}
        }
        Ok(self.quit_requested)
    }

    pub(crate) async fn submit(
        &mut self,
        input: String,
        tx: mpsc::Sender<CoreEvent>,
    ) -> Result<()> {
        if let Some(target) = self.secret_target {
            let result = match target {
                SecretTarget::ObsPassword => self.obs.set_password(&input).await,
                SecretTarget::AssistantApiKey => self
                    .check_assistant_connection_change()
                    .and_then(|()| self.runner.save_api_key(&input, &self.bridge)),
            };
            match result {
                Err(error) => {
                    self.input.replace(input);
                    self.set_notice(format!("保存失败：{error}（内容保留）"), NoticeLevel::Error);
                }
                Ok(()) => {
                    self.secret_target = None;
                    self.input = self.secret_draft.take().unwrap_or_default();
                    self.set_notice("已保存", NoticeLevel::Success);
                }
            }
            return Ok(());
        }
        if input.starts_with('/') {
            return self.command(&input, tx).await;
        }

        let segments = segment_message(&input, crate::bilibili::SEND_SEGMENT_LIMIT);
        if !segments.is_empty() {
            self.enqueue(segments, None, tx);
        }
        Ok(())
    }

    pub(crate) async fn command(&mut self, raw: &str, tx: mpsc::Sender<CoreEvent>) -> Result<()> {
        self.ui_tx = Some(tx.clone());
        anyhow::ensure!(self.secret_target.is_none(), "隐藏输入中不能执行公开命令");
        let action: Result<()> = async {
            let parts = raw.split_whitespace().collect::<Vec<_>>();
            if self.local_transport.is_some()
                && parts
                    .first()
                    .is_some_and(|command| obs_command(command) || *command == "/room")
            {
                anyhow::bail!("本地模式禁止账号与OBS操作");
            }
            match parts.as_slice() {
                ["/about"] => self.open_about()?,
                ["/display", "theme"] => self.open_theme_settings()?,
                ["/display", setting, value] => self.configure_display(setting, value)?,
                [
                    "/ai",
                    section @ ("model" | "replies" | "materials" | "advanced"),
                ] => self.open_ai_section(section)?,
                ["/settings", "obs"] => self.open_obs_settings().await?,
                ["/room", setting @ ("title" | "cover"), ..] => {
                    let field = if *setting == "title" {
                        settings::ExternalField::RoomTitle
                    } else {
                        settings::ExternalField::RoomCover
                    };
                    self.open_external_editor(field, command_value(raw, 2), tx)
                        .await?;
                }
                ["/obs", "config", setting @ ("host" | "port" | "mic"), ..] => {
                    let field = match *setting {
                        "host" => settings::ExternalField::ObsHost,
                        "port" => settings::ExternalField::ObsPort,
                        _ => settings::ExternalField::ObsMicrophone,
                    };
                    self.open_external_editor(field, command_value(raw, 3), tx)
                        .await?;
                }
                ["/scene", ..] => {
                    self.open_external_editor(
                        settings::ExternalField::ObsScene,
                        command_value(raw, 1),
                        tx,
                    )
                    .await?;
                }
                ["/commands"] => self.open_commands(),
                ["/more"] => self.open_more()?,
                ["/ai", "start"] => {
                    self.start_assistant(false)?;
                }
                ["/ai", "auto", "on"] => {
                    self.set_automatic_permission(true)?;
                }
                ["/ai", "auto", "off"] => {
                    self.set_automatic_permission(false)?;
                }
                ["/ai", "range"] => self.open_ai_range()?,
                ["/ai", "topic"] => self.open_ai_topic()?,
                ["/ai", "visible"] => self.start_assistant(true)?,
                ["/ai", "key"] => self.begin_assistant_api_key()?,
                ["/ai", "key", ..] => anyhow::bail!("/ai key 不接收参数；请在隐藏输入中填写"),
                ["/ai", "stop"] => self.stop_assistant(),
                ["/diag", "repair"] => {
                    self.runner.repair_history(&self.journal)?;
                }
                ["/settings", section] => {
                    if let Err(error) = self.open_settings_section(section) {
                        self.set_notice(error.to_string(), NoticeLevel::Error);
                    }
                }
                ["/settings"] | ["/diag"] | ["/ai"] | ["/review"] => {
                    let result = match parts[0] {
                        "/settings" => self.open_settings(),
                        "/diag" => self.open_diagnostics(),
                        "/ai" => self.open_assistant(),
                        _ => self.open_candidate_review(),
                    };
                    if let Err(error) = result {
                        self.set_notice(error.to_string(), NoticeLevel::Error);
                    }
                }
                ["/pause"] => self.pause_assistant(),

                ["/quit"] => {
                    self.quit_requested = true;
                }
                ["/help"] => {
                    self.help = Some(HelpState::default());
                }
                ["/pin"] => {
                    let notice = if self.selection_active {
                        "已将选中消息设为重点"
                    } else {
                        "未选择消息，已将最新一条设为重点"
                    };
                    self.with_selected(|session, id| session.feature(Some(id)), notice)
                }
                ["/find"] => self.open_archive_search()?,
                ["/find", rest @ ..] => self.search_archive(&rest.join(" ")),
                ["/obs"] | ["/obs", "status"] | ["/obs", "connect"] => {
                    self.set_notice("正在检查 OBS 连接…", NoticeLevel::Progress);
                    self.execute_command(crate::runtime::RuntimeCommand::Obs(
                        crate::runtime::ObsCommand::Connect,
                    ))
                    .await?;
                }
                ["/mute"] | ["/unmute"] => {
                    self.set_notice(
                        if parts[0] == "/mute" {
                            "正在静音麦克风…"
                        } else {
                            "正在取消麦克风静音…"
                        },
                        NoticeLevel::Progress,
                    );
                    let muted = parts[0] == "/mute";
                    self.execute_command(crate::runtime::RuntimeCommand::Obs(
                        crate::runtime::ObsCommand::SetMuted { muted },
                    ))
                    .await?;
                }
                ["/obs", "config", "password"] => {
                    self.secret_target = Some(SecretTarget::ObsPassword);
                    self.secret_draft = Some(std::mem::take(&mut self.input));
                    self.set_notice(
                        "请输入 OBS WebSocket 密码并按 Enter；输入不会显示或进入历史",
                        NoticeLevel::Progress,
                    );
                }
                ["/obs", "start"] => {
                    self.set_notice("正在启动 OBS 推流…", NoticeLevel::Progress);
                    self.execute_command(crate::runtime::RuntimeCommand::Obs(
                        crate::runtime::ObsCommand::StartStream,
                    ))
                    .await?;
                }
                ["/obs", "stop"] => {
                    if self.stop_flow.is_none() {
                        self.execute_command(crate::runtime::RuntimeCommand::Obs(
                            crate::runtime::ObsCommand::RequestStop,
                        ))
                        .await?;
                    }
                }
                _ => self.set_notice(
                    format!("未知命令：{raw}；输入 /help 查看命令"),
                    NoticeLevel::Error,
                ),
            }
            Ok(())
        }
        .await;
        if let Err(error) = action {
            self.set_notice(error.to_string(), NoticeLevel::Error);
            return Ok(());
        }
        // Operation failures stay in the UI; current journal write failures still propagate.
        self.journal.snapshot(&self.session)?;
        Ok(())
    }

    pub(crate) fn with_selected(
        &mut self,
        operation: impl FnOnce(&mut DanmuSession, &str),
        notice: &str,
    ) {
        if let Some(id) = self
            .session
            .recent_events
            .get(self.selected)
            .map(|event| event.id.clone())
        {
            operation(&mut self.session, &id);
            self.set_notice(notice, NoticeLevel::Success);
        } else {
            self.set_notice("当前没有可操作的消息", NoticeLevel::Info);
        }
    }

    pub(crate) fn move_selection(&mut self, older: bool) {
        let candidates = self
            .session
            .recent_events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.kind == DanmuEventKind::Danmu
                    && event
                        .username
                        .as_deref()
                        .is_some_and(|name| !name.trim().is_empty())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            self.return_to_live();
            return;
        }
        if !self.selection_active {
            self.selection_active = true;
            self.unread_live_count = 0;
            self.scroll_offset = 0;
            self.selected = candidates[0];
            return;
        }
        let position = candidates
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let next = if older {
            (position + 1).min(candidates.len() - 1)
        } else {
            position.saturating_sub(1)
        };
        self.selected = candidates[next];
    }

    pub(crate) fn search_archive(&mut self, query: &str) {
        match self.journal.search(query) {
            Ok(results) => {
                let message = if results.is_empty() {
                    "未找到归档会话".into()
                } else {
                    format!(
                        "找到 {} 个归档会话：{}",
                        results.len(),
                        results
                            .iter()
                            .take(3)
                            .map(|item| format!(
                                "{}@{}",
                                item.room_id,
                                item.started_at.with_timezone(&Local).format("%m-%d %H:%M")
                            ))
                            .collect::<Vec<_>>()
                            .join("、")
                    )
                };
                self.set_notice(message, NoticeLevel::Info);
            }
            Err(error) => self.set_notice(error.to_string(), NoticeLevel::Error),
        }
    }

    pub(crate) fn insert(&mut self, character: char) {
        let mut buffer = [0; 4];
        self.insert_text(character.encode_utf8(&mut buffer));
    }

    pub(crate) fn handle_help_key(&mut self, key: KeyCode) -> bool {
        let Some(help) = self.help.as_mut() else {
            return false;
        };
        match key {
            KeyCode::Esc | KeyCode::Enter => self.help = None,
            KeyCode::Up => help.scroll = help.scroll.saturating_sub(1),
            KeyCode::Down => help.scroll = help.scroll.saturating_add(1).min(help.max_scroll),
            KeyCode::PageUp => help.scroll = help.scroll.saturating_sub(help.page_rows),
            KeyCode::PageDown => {
                help.scroll = help
                    .scroll
                    .saturating_add(help.page_rows)
                    .min(help.max_scroll)
            }
            KeyCode::Home => help.scroll = 0,
            KeyCode::End => help.scroll = help.max_scroll,
            _ => {}
        }
        true
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        if self.secret_target.is_some() {
            if text.len() > 4096 || self.input.len().saturating_add(text.len()) > 4096 {
                self.set_notice("隐藏输入超过4096字节，未插入", NoticeLevel::Error);
                return;
            }
            self.last_user_activity = Instant::now();
            self.input.insert_paste(text, false);
            self.slash_selection = 0;
            return;
        }
        if self.help.is_some()
            || self.stop_flow.is_some()
            || self.login_qr.is_some()
            || self.main_login.is_some()
            || self.assistant_accounts.login_pending()
        {
            return;
        }
        if self.assistant_panel.is_some()
            && self.secret_target.is_none()
            && self.candidate_edit.is_none()
            && self.command_search_draft.is_none()
        {
            self.paste_assistant(text);
            return;
        }
        if self.secret_target.is_none()
            && self.candidate_edit.is_none()
            && self.command_search_draft.is_none()
            && self.selection_active
            && !(self.input.starts_with('/') || (self.input.is_empty() && text.starts_with('/')))
        {
            return;
        }
        if text.len() > 4096 {
            self.set_notice("粘贴超过4096字节，未插入", NoticeLevel::Error);
            return;
        }
        self.last_user_activity = Instant::now();
        self.input.insert_paste(text, false);
        self.slash_selection = 0;
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        if self.secret_target.is_some() && self.input.len().saturating_add(text.len()) > 4096 {
            self.set_notice("隐藏输入超过4096字节，未插入", NoticeLevel::Error);
            return;
        }
        self.input.insert_text(text);
        self.slash_selection = 0;
    }
}

struct TerminalGuard {
    pub(crate) terminal: Terminal<CrosstermBackend<Stdout>>,
    pub(crate) mouse_capture: bool,
}
impl TerminalGuard {
    pub(crate) fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        // Windows can only restore mouse mode after EnableMouseCapture saved it.
        #[cfg(not(windows))]
        execute!(stdout, DisableMouseCapture)?;
        execute!(stdout, EnableBracketedPaste)?;
        // Crossterm's Windows backend rejects the Kitty keyboard protocol.
        #[cfg(not(windows))]
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        )?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            mouse_capture: false,
        })
    }

    pub(crate) fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        if self.mouse_capture == enabled {
            return Ok(());
        }
        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
        }
        self.mouse_capture = enabled;
        Ok(())
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Windows mouse restoration reinstates the saved raw input mode.
        // Release owned capture before restoring cooked input for the shell.
        let _ = self.set_mouse_capture(false);
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), DisableBracketedPaste);
        #[cfg(not(windows))]
        let _ = execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags);
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn display_events(events: &[DanmuEvent]) -> impl Iterator<Item = &DanmuEvent> {
    events
        .iter()
        .rev()
        .filter(|event| event.kind.activity_lifetime().is_none())
}

fn startup_check_line(
    label: &'static str,
    status: StartupCheck,
    palette: Palette,
) -> Line<'static> {
    let (marker, detail, color) = match status {
        StartupCheck::Waiting => ("·", "等待", palette.frame),
        StartupCheck::Running => ("›", "检查中", palette.info),
        StartupCheck::Passed(detail) => ("✓", detail, palette.success),
        StartupCheck::Skipped(detail) => ("–", detail, palette.time),
        StartupCheck::Warning(detail) => ("!", detail, palette.warning),
    };
    Line::from(vec![
        Span::styled(format!("{marker} "), Style::default().fg(color)),
        Span::styled(format!("{label}  "), Style::default().fg(palette.content)),
        Span::styled(detail, Style::default().fg(color)),
    ])
}

fn startup_progress_line(
    elapsed: Duration,
    data_ready: bool,
    width: usize,
    palette: Palette,
) -> Line<'static> {
    let filled = if data_ready {
        width
    } else {
        ((elapsed.as_millis() / 90) as usize % width.max(1)).min(width.saturating_sub(1))
    };
    Line::from(vec![
        Span::styled("━".repeat(filled), Style::default().fg(palette.info)),
        Span::styled(
            "─".repeat(width.saturating_sub(filled)),
            Style::default().fg(palette.frame),
        ),
    ])
}

fn draw_startup(
    frame: &mut ratatui::Frame,
    palette: Palette,
    tick: u64,
    startup: &StartupView,
    gate: &StartupGate,
    elapsed: Duration,
    data_ready: bool,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.background)),
        area,
    );

    let cursor = if (tick / 3).is_multiple_of(2) {
        "▮"
    } else {
        " "
    };
    let masked_password = match gate {
        StartupGate::EnteringObsPassword(input) => {
            let visible = input.graphemes(true).count().min(16);
            format!("OBS 密码 {}{cursor}", "●".repeat(visible))
        }
        _ => String::new(),
    };
    let (active_label, active_color) = match gate {
        StartupGate::Blocked if startup.obs_requires_password() => {
            ("启动已阻断 · OBS 需要配置".to_owned(), palette.warning)
        }
        StartupGate::Blocked => ("启动已阻断 · 检查未通过".to_owned(), palette.warning),
        StartupGate::EnteringObsPassword(_) => (masked_password, palette.info),
        StartupGate::Skipped => ("已跳过未通过检查".to_owned(), palette.warning),
        _ => (startup.active_label().to_owned(), palette.time),
    };
    let (footer, footer_color) = match gate {
        StartupGate::Blocked if startup.obs_requires_password() => {
            ("Enter 配置 OBS · S 跳过 · Ctrl+C 退出", palette.warning)
        }
        StartupGate::Blocked => ("Enter 重试 · S 跳过 · Ctrl+C 退出", palette.warning),
        StartupGate::EnteringObsPassword(_) => {
            ("Enter 保存 · Esc 返回 · Ctrl+C 退出", palette.info)
        }
        _ => ("Ctrl+C 取消", palette.frame),
    };
    if area.width < 42 || area.height < 16 {
        let height = area.height.min(3);
        let top = area.height.saturating_sub(height) / 2;
        let compact_area = Rect::new(area.x, area.y + top, area.width, height);
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    "DANMU",
                    Style::default()
                        .fg(palette.content)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · Elazer · ", Style::default().fg(palette.rank)),
                Span::styled("elazer.wang", Style::default().fg(palette.info)),
            ])
            .alignment(Alignment::Center),
            startup_progress_line(elapsed, data_ready, 12, palette).alignment(Alignment::Center),
            Line::from(Span::styled(
                active_label.clone(),
                Style::default().fg(active_color),
            ))
            .alignment(Alignment::Center),
        ];
        frame.render_widget(Paragraph::new(Text::from(lines)), compact_area);
        return;
    }

    let border = Style::default().fg(palette.info);
    let logo_lines = vec![
        Line::from(Span::styled("╭────────────────╮", border)),
        Line::from(vec![
            Span::styled("│ ", border),
            Span::styled("●", Style::default().fg(palette.warning)),
            Span::raw("  "),
            Span::styled("●", Style::default().fg(palette.rank)),
            Span::raw("  "),
            Span::styled("●", Style::default().fg(palette.success)),
            Span::styled("        │", border),
        ]),
        Line::from(vec![
            Span::styled("│  ", border),
            Span::styled(
                "DANMU",
                Style::default()
                    .fg(palette.content)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ▸ ", border),
            Span::styled(cursor, Style::default().fg(palette.host)),
            Span::styled("     │", border),
        ]),
        Line::from(Span::styled("╰──────────╮     │", border)),
        Line::from(Span::styled("           ╰─────╯", border)),
    ];
    let total_height = 16_u16;
    let top = area.height.saturating_sub(total_height) / 2;
    let logo_area = Rect::new(area.x, area.y + top, area.width, 5);
    let identity_area = Rect::new(area.x, logo_area.y + 5, area.width, 1);
    let status_width = area.width.min(46);
    let status_area = Rect::new(
        area.x + area.width.saturating_sub(status_width) / 2,
        logo_area.y + 7,
        status_width,
        9,
    );
    frame.render_widget(
        Paragraph::new(Text::from(logo_lines)).alignment(Alignment::Center),
        logo_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("by Elazer", Style::default().fg(palette.rank)),
            Span::styled("  ·  ", Style::default().fg(palette.frame)),
            Span::styled("elazer.wang", Style::default().fg(palette.info)),
        ]))
        .alignment(Alignment::Center),
        identity_area,
    );
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                "启动自检",
                Style::default()
                    .fg(palette.content)
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
            startup_progress_line(elapsed, data_ready, 24, palette).alignment(Alignment::Center),
            Line::from(vec![
                Span::styled("› ", Style::default().fg(palette.info)),
                Span::styled(active_label, Style::default().fg(active_color)),
            ]),
            startup_check_line("本地数据", startup.local, palette),
            startup_check_line("登录状态", startup.account, palette),
            startup_check_line("B 站房间", startup.room, palette),
            startup_check_line("直播指标", startup.metrics, palette),
            startup_check_line("OBS 连接", startup.obs, palette),
            Line::from(Span::styled(footer, Style::default().fg(footer_color)))
                .alignment(Alignment::Center),
        ])),
        status_area,
    );
}

fn draw(frame: &mut ratatui::Frame, app: &mut RuntimeState) {
    app.clear_review_surface();
    let palette = app.config.palette;
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.background)),
        area,
    );
    if area.width < 32 || area.height < 7 {
        draw_compact(frame, area, app, palette);
        draw_overlays(frame, area, app, palette);
        return;
    }
    let status_lines = technical_status_lines(app, palette, area.width);
    let status_height = u16::try_from(status_lines.len())
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(4))
        .max(1);
    let notice_lines = application_notice_lines(app, palette, area.width);
    let available_after_body = area.height.saturating_sub(status_height + 2);
    let notice_height = u16::try_from(notice_lines.len())
        .unwrap_or(u16::MAX)
        .min(3)
        .min(available_after_body.saturating_sub(2));
    let review_height =
        agent::review_height(app).min(available_after_body.saturating_sub(notice_height + 2));
    let input_height = input_area_height(app, area.width)
        .min(available_after_body.saturating_sub(notice_height + review_height))
        .max(2);
    let rows = Layout::vertical([
        Constraint::Length(status_height),
        Constraint::Min(2),
        Constraint::Length(notice_height),
        Constraint::Length(review_height),
        Constraint::Length(input_height),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(Text::from(status_lines)), rows[0]);
    draw_body(frame, rows[1], app, palette);

    if notice_height > 0 {
        frame.render_widget(Paragraph::new(Text::from(notice_lines)), rows[2]);
    }
    agent::draw_review(frame, rows[3], app);
    draw_input(frame, rows[4], app, palette);

    draw_overlays(frame, area, app, palette);
}

fn draw_overlays(frame: &mut ratatui::Frame, area: Rect, app: &mut RuntimeState, palette: Palette) {
    if app.stop_flow.is_some() {
        draw_stop_flow(frame, area, app, palette);
    } else if let Some(lines) = &app.login_qr {
        draw_qr(frame, area, lines, palette);
    } else if app.secret_target.is_some() || app.candidate_edit.is_some() {
        // The focused input already carries Enter/Esc instructions.
    } else if app
        .assistant_panel
        .as_ref()
        .is_some_and(|panel| panel.is_editing())
    {
        assistant::draw(frame, app);
    } else if app.help.is_some() {
        draw_help(frame, area, app, palette);
    } else if app.command_search_draft.is_some() || app.input.starts_with('/') {
        draw_command_palette(frame, area, app, palette);
    } else if app.assistant_panel.is_some() {
        assistant::draw(frame, app);
    }
}

fn draw_stop_flow(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut RuntimeState,
    palette: Palette,
) {
    let Some(flow) = app.stop_flow else {
        return;
    };
    let width = area.width.min(42);
    let height = area.height.min(6);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = rounded_block(" 停止推流？ ", palette);
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let text = match flow {
        StopFlow::Confirm { .. } => "直播将中断".to_owned(),
        StopFlow::Countdown { deadline } => format!(
            "{} 秒后停止推流",
            deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64()
                .ceil()
                .max(1.0) as u64
        ),
        StopFlow::Stopping => "正在停止推流…".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(palette.content)),
        inner,
    );
    if inner.height < 2 {
        return;
    }
    match flow {
        StopFlow::Confirm { stop_selected } => {
            for (row, label, selected, color) in [
                (1, "停止推流", stop_selected, Color::Red),
                (2, "取消", !stop_selected, palette.content),
            ] {
                if row < inner.height {
                    frame.render_widget(
                        Paragraph::new(format!("{} {label}", if selected { "›" } else { " " }))
                            .style(Style::default().fg(color)),
                        Rect::new(inner.x, inner.y + row, inner.width, 1),
                    );
                }
            }
            if inner.height > 3 {
                frame.render_widget(
                    Paragraph::new("↑↓选择 · Enter执行 · Esc取消")
                        .style(Style::default().fg(palette.time)),
                    Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
                );
            }
        }
        StopFlow::Countdown { .. } => {
            frame.render_widget(
                Paragraph::new("Esc 取消停止").style(Style::default().fg(palette.info)),
                Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            );
        }
        StopFlow::Stopping => {}
    }
}

fn draw_help(frame: &mut ratatui::Frame, area: Rect, app: &mut RuntimeState, palette: Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .title(" 帮助 ")
        .style(Style::default().bg(palette.background).fg(palette.content));
    let inner = block.inner(area);
    let mut lines = Vec::new();
    for (heading, body) in HELP_SECTIONS {
        lines.extend(wrap_styled_spans(
            vec![Span::styled(
                *heading,
                Style::default()
                    .fg(palette.info)
                    .add_modifier(Modifier::BOLD),
            )],
            inner.width,
            Alignment::Left,
        ));
        lines.extend(wrap_styled_spans(
            vec![Span::raw(*body)],
            inner.width,
            Alignment::Left,
        ));
        lines.push(Line::default());
    }
    if app.local_transport.is_some() {
        lines.extend(wrap_styled_spans(
            vec![Span::styled(
                "仅本地练习（不连接直播平台）",
                Style::default().fg(palette.info),
            )],
            inner.width,
            Alignment::Left,
        ));
        lines.extend(wrap_styled_spans(
            vec![Span::raw("/event 姓名 正文 注入本地弹幕；/local confirmed|uncertain|rejected 设下一次本地送达结果。\n/session end 结束本地场次；/session new 新建场次且不授权，不是 OBS 控制。")],
            inner.width,
            Alignment::Left,
        ));
    }
    let help = app.help.as_mut().expect("帮助已打开");
    help.page_rows = usize::from(inner.height).max(1);
    help.max_scroll = lines.len().saturating_sub(help.page_rows);
    help.scroll = help.scroll.min(help.max_scroll);
    let footer = format!(
        " {}/{} · ↑↓/PgDn · Esc返回 · 助手发送{} ",
        help.scroll + 1,
        help.max_scroll + 1,
        if app.bridge.sending_enabled() {
            "已许可"
        } else {
            "未许可"
        },
    );
    frame.render_widget(block.title_bottom(footer), area);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(help.scroll)
                .take(help.page_rows)
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}

fn draw_command_palette(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut RuntimeState,
    palette: Palette,
) {
    if app.candidate_edit.is_some() || app.secret_target.is_some() {
        return;
    }
    let area = Rect {
        height: area
            .height
            .saturating_sub(input_area_height(app, area.width) + 1),
        ..area
    };
    let suggestions = app.command_suggestions();
    if (suggestions.is_empty() && app.command_search_draft.is_none()) || area.height < 4 {
        return;
    }

    let capacity = usize::from(
        (area.height / 2)
            .clamp(8, 12)
            .min(area.height.saturating_sub(2)),
    );
    let visible = suggestions.len().max(1).min(capacity);
    app.slash_selection = app.slash_selection.min(suggestions.len().saturating_sub(1));
    let offset = app
        .slash_selection
        .saturating_sub(visible - 1)
        .min(suggestions.len().saturating_sub(visible));
    let height = (visible as u16 + 2).min(area.height);
    let width = area.width.saturating_sub(4).min(72);
    let popup = Rect::new(
        area.x + 2,
        area.y + area.height.saturating_sub(height),
        width,
        height,
    );
    let mut items = suggestions
        .iter()
        .skip(offset)
        .take(visible)
        .map(|suggestion| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(
                        "{}  {}   ",
                        suggestion.usage,
                        suggestion.completion.trim_end()
                    ),
                    Style::default()
                        .fg(if suggestion.danger {
                            Color::Red
                        } else {
                            suggestion.category.color(palette)
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(suggestion.description, Style::default().fg(palette.time)),
            ]))
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(ListItem::new("没有匹配项；请换个词，Esc恢复草稿"));
    }
    let mut state = ListState::default().with_selected(Some(app.slash_selection - offset));
    let title = format!(
        " 命令 · 共 {} 项 · ↑↓选择 · Enter执行 · Tab补全 ",
        suggestions.len()
    );
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .title(title)
                .title_bottom(if app.command_search_draft.is_some() {
                    " 输入中文搜索全部 · Esc恢复草稿 · 不会发送弹幕 "
                } else {
                    " 常用操作 · 输入中文搜索全部 · Ctrl-O保留草稿 "
                }),
        )
        .style(Style::default().bg(palette.background).fg(palette.content))
        .highlight_style(
            Style::default()
                .bg(palette.frame)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut state);
}

fn draw_compact(
    frame: &mut ratatui::Frame,
    mut area: Rect,
    app: &mut RuntimeState,
    palette: Palette,
) {
    if app.runner.running || app.resume_pending {
        let assistant = assistant::compact(app, area.width.saturating_sub(8));
        let room = fit_display_width(
            &format!(" ROOM {}", app.config.room_id),
            usize::from(area.width).saturating_sub(assistant.width()),
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                assistant,
                Span::styled(room, Style::default().fg(palette.content)),
            ])),
            Rect::new(area.x, area.y, area.width, area.height.min(1)),
        );
        area.y = area.y.saturating_add(1);
        area.height = area.height.saturating_sub(1);
    }
    if area.height == 0 {
        return;
    }
    if app.is_browsing_history() && area.height < 3 {
        draw_history_status(frame, area, app, palette);
        return;
    }
    if area.height < 3 {
        frame.render_widget(
            Paragraph::new(format!(
                "{} · ROOM {} · ❯ {}",
                connection_badge(app),
                app.config.room_id,
                visible_input(app)
            ))
            .style(Style::default().fg(palette.content)),
            area,
        );
        return;
    }
    let input_height = input_area_height(app, area.width)
        .min(area.height.saturating_sub(1))
        .max(2);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(input_height)]).split(area);
    if app.is_browsing_history() {
        draw_events(frame, rows[0], app, palette, "History");
        draw_input(frame, rows[1], app, palette);
        return;
    }
    let latest = display_events(&app.session.recent_events)
        .map(|event| {
            Line::from(Span::styled(
                map_event_emotes(event),
                Style::default().fg(palette.content),
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(latest).style(Style::default().fg(palette.content)),
        rows[0],
    );
    draw_input(frame, rows[1], app, palette);
}

fn draw_body(frame: &mut ratatui::Frame, area: Rect, app: &mut RuntimeState, palette: Palette) {
    let title = app
        .session
        .featured_event
        .as_ref()
        .map(|event| {
            if app.show_name {
                format!(
                    "NOW · {}：{}",
                    event.username.as_deref().unwrap_or("观众"),
                    map_event_emotes(event)
                )
            } else {
                format!("NOW · {}", map_event_emotes(event))
            }
        })
        .unwrap_or_else(|| "Ghost Stage".into());
    draw_events(frame, area, app, palette, &title);
}

fn rounded_block(title: &str, palette: Palette) -> Block<'static> {
    let title = if title.is_empty() {
        String::new()
    } else {
        format!(" {title} ")
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(palette.frame))
}

fn room_live_label(room: &RoomSnapshot) -> &'static str {
    match room.live_status {
        RoomLiveStatus::Offline => "○ OFFLINE",
        RoomLiveStatus::Live => "● LIVE",
        RoomLiveStatus::Rotating => "◉ ROTATING",
    }
}

const STATUS_RED: Color = Color::Rgb(225, 29, 72);
const STATUS_ORANGE: Color = Color::Rgb(194, 65, 12);
const STATUS_GREEN: Color = Color::Rgb(21, 128, 61);
const STATUS_BLUE: Color = Color::Rgb(29, 78, 216);

// Local identity emphasis, deliberately distinct from green delivery confirmation.
const ASSISTANT_MESSAGE_COLOR: Color = Color::Rgb(255, 175, 95);
fn status_span(content: String, background: Color, palette: Palette) -> Span<'static> {
    Span::styled(
        content,
        Style::default()
            .fg(contrast_foreground(background, palette))
            .bg(background)
            .add_modifier(Modifier::BOLD),
    )
}

fn fit_display_width(content: &str, width: usize) -> String {
    if UnicodeWidthStr::width(content) <= width {
        return content.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut fitted = String::new();
    let mut used = 0;
    for grapheme in content.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used + grapheme_width + 1 > width {
            break;
        }
        fitted.push_str(grapheme);
        used += grapheme_width;
    }
    fitted.push('…');
    fitted
}

fn live_elapsed(room: &RoomSnapshot, now: DateTime<Utc>) -> String {
    let Some(started_at) = room.live_started_at.filter(|_| room.is_live()) else {
        return "--:--:--".into();
    };
    let seconds = now.signed_duration_since(started_at).num_seconds().max(0);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn primary_status_line(
    room: &RoomSnapshot,
    app: &RuntimeState,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let indicator_on = (app.animation_tick / 2).is_multiple_of(2);
    // Preserve the original six-column indicator throughout its blink cycle.
    let live_label = if room.is_live() && !indicator_on {
        "      "
    } else {
        room_live_label(room)
    };
    let live = format!(" {live_label} ");
    let elapsed = format!(" ◷ {} ", live_elapsed(room, Utc::now()));
    let live_width = UnicodeWidthStr::width(live.as_str());
    let elapsed_width = UnicodeWidthStr::width(elapsed.as_str());
    let title_limit = usize::from(width).saturating_sub(2 * live_width.max(elapsed_width));
    let title = fit_display_width(&room.title, title_limit);
    let title_width = UnicodeWidthStr::width(title.as_str());
    let title_start = usize::from(width).saturating_sub(title_width) / 2;
    let left_gap = title_start.saturating_sub(live_width);
    let right_gap =
        usize::from(width).saturating_sub(live_width + left_gap + title_width + elapsed_width);
    Line::from(vec![
        if room.is_live() {
            Span::styled(
                live,
                Style::default()
                    .fg(Color::LightRed)
                    .bg(palette.background)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            status_span(live, STATUS_BLUE, palette)
        },
        Span::raw(" ".repeat(left_gap)),
        Span::styled(
            title,
            Style::default()
                .fg(palette.rank)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(right_gap)),
        Span::styled(
            elapsed,
            Style::default()
                .fg(palette.content)
                .bg(palette.background)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn secondary_status_line(
    room: &RoomSnapshot,
    app: &RuntimeState,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let obs_connected = app.obs_error.is_none() && app.obs_status.is_some();
    let obs_indicator = "●";
    let obs_color = if obs_connected {
        STATUS_GREEN
    } else {
        STATUS_RED
    };
    let (microphone_indicator, microphone_color) = match app
        .obs_status
        .as_ref()
        .filter(|_| app.obs_error.is_none())
        .map(|status| status.microphone)
    {
        Some(MicrophoneState::Unmuted) => ("◉", STATUS_RED),
        Some(MicrophoneState::Muted) => ("○ ", palette.rank),
        _ => ("?", palette.rank),
    };
    let devices_width = UnicodeWidthStr::width("OBS ")
        + UnicodeWidthStr::width(obs_indicator)
        + UnicodeWidthStr::width("  MIC ")
        + UnicodeWidthStr::width(microphone_indicator);
    let assistant = assistant::compact(app, width.saturating_sub(devices_width as u16 + 8));
    let assistant_width = UnicodeWidthStr::width(assistant.content.as_ref());
    let broadcaster = fit_display_width(
        &format!(" @ {} ", room.broadcaster_name),
        usize::from(width).saturating_sub(devices_width + assistant_width),
    );
    let gap = usize::from(width).saturating_sub(
        UnicodeWidthStr::width(broadcaster.as_str()) + devices_width + assistant_width,
    );
    let neutral = Style::default()
        .fg(palette.content)
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::with_capacity(6);
    if !broadcaster.is_empty() {
        spans.push(status_span(broadcaster, STATUS_GREEN, palette));
    }
    spans.push(assistant);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend([
        Span::styled("OBS ", neutral),
        Span::styled(
            obs_indicator,
            Style::default().fg(obs_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  MIC ", neutral),
        Span::styled(
            microphone_indicator,
            Style::default()
                .fg(microphone_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    Line::from(spans)
}

fn technical_status_lines(app: &RuntimeState, palette: Palette, width: u16) -> Vec<Line<'static>> {
    let Some(room) = app.room.as_ref() else {
        return vec![Line::from(vec![
            assistant::compact(app, width.saturating_sub(8)),
            status_span(" ◌ ROOM ".into(), STATUS_ORANGE, palette),
        ])];
    };
    vec![
        primary_status_line(room, app, palette, width),
        secondary_status_line(room, app, palette, width),
    ]
}
fn application_notice_lines(
    app: &RuntimeState,
    palette: Palette,
    width: u16,
) -> Vec<Line<'static>> {
    if app.notice.is_empty() {
        return Vec::new();
    }
    let (prefix, color) = match app.notice_level {
        NoticeLevel::Info => ("·", palette.time),
        NoticeLevel::Success => ("+", palette.success),
        NoticeLevel::Progress => ("·", palette.info),
        NoticeLevel::Warning => ("!", palette.rank),
        NoticeLevel::Error => ("!", palette.warning),
    };
    vec![Line::from(Span::styled(
        format!(
            "{prefix} {}",
            fit_display_width(
                &app.notice.replace(['\n', '\r'], " "),
                usize::from(width).saturating_sub(2).min(64)
            )
        ),
        Style::default().fg(color),
    ))]
}

fn draw_history_status(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &RuntimeState,
    palette: Palette,
) {
    let first = if area.width >= 44 {
        " HISTORY · Esc to follow"
    } else if area.width >= 24 {
        " HISTORY · Esc"
    } else {
        " HISTORY"
    };
    let mut second = format!(" +{} NEW", app.unread_live_count);
    if app.config.history_idle_seconds > 0 {
        if app.stop_flow.is_some() || app.login_qr.is_some() || app.secret_target.is_some() {
            second.push_str(" · TIMER PAUSED");
        } else {
            let remaining = u64::from(app.config.history_idle_seconds)
                .saturating_sub(app.last_user_activity.elapsed().as_secs());
            second.push_str(&format!(" · AUTO {remaining}s"));
        }
    } else {
        second.push_str(" · MANUAL");
    }
    frame.render_widget(
        Paragraph::new(vec![Line::from(first), Line::from(second)]).style(
            Style::default()
                .fg(contrast_foreground(palette.warning, palette))
                .bg(palette.warning)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

fn draw_events(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut RuntimeState,
    palette: Palette,
    title: &str,
) {
    if app.is_browsing_history() && area.height < 4 {
        draw_history_status(frame, area, app, palette);
        return;
    }
    let displayed_events = display_events(&app.session.recent_events).collect::<Vec<_>>();
    let selection_anchor = app
        .selection_active
        .then(|| app.session.recent_events.get(app.selected))
        .flatten();
    let browse_anchor = selection_anchor.or_else(|| {
        (app.scroll_offset > 0)
            .then(|| app.session.recent_events.get(app.scroll_offset))
            .flatten()
    });
    let selection_target = selection_anchor.and_then(|selected| {
        displayed_events
            .iter()
            .position(|event| event.id == selected.id)
    });
    let browse_target = browse_anchor.and_then(|selected| {
        displayed_events
            .iter()
            .position(|event| event.id == selected.id)
    });
    let title = if browse_target.is_some() {
        format!("{title} · HISTORY")
    } else {
        format!("{title} · FOLLOW")
    };
    let mut block = rounded_block(&title, palette);
    if browse_target.is_some() {
        block = block.border_style(Style::default().fg(palette.warning));
    }
    if let Some((kind, content)) = app.activity.render(
        usize::from(area.width.saturating_sub(4)),
        app.show_name,
        Instant::now(),
    ) {
        block = block.title_bottom(Line::styled(
            format!(" {content} "),
            Style::default().fg(event_color(kind, palette)),
        ));
    }
    let mut inner = block.inner(area);
    let items = displayed_events
        .into_iter()
        .map(|event| ListItem::new(event_lines(event, app, palette, inner.width)))
        .collect::<Vec<_>>();

    let scroll_target = browse_target.or_else(|| (!items.is_empty()).then(|| items.len() - 1));
    let used_height = items.iter().fold(0_u16, |height, item| {
        height.saturating_add(item.height() as u16)
    });
    frame.render_widget(block, area);
    if browse_target.is_some() {
        let height = inner.height.min(2);
        let banner = Rect {
            y: inner.bottom().saturating_sub(height),
            height,
            ..inner
        };
        draw_history_status(frame, banner, app, palette);
        inner.height = inner.height.saturating_sub(height);
    }
    let visible_height = used_height.min(inner.height);
    let list_area = Rect {
        y: inner.y + inner.height.saturating_sub(visible_height),
        height: visible_height,
        ..inner
    };
    let list = List::new(items).highlight_style(if selection_target.is_some() {
        Style::default()
            .fg(palette.info)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    });
    let mut state = ListState::default().with_selected(scroll_target);
    frame.render_stateful_widget(list, list_area, &mut state);
}

fn event_marker(kind: DanmuEventKind) -> &'static str {
    match kind {
        DanmuEventKind::Danmu => "•",
        DanmuEventKind::Gift => "◆",
        DanmuEventKind::GuardEvent => "♛",
        DanmuEventKind::Superchat => "▣",
        DanmuEventKind::Enter => "→",
        DanmuEventKind::Like => "♥",
        DanmuEventKind::Follow => "+",
        DanmuEventKind::Share => "↗",
        DanmuEventKind::Pk => "⚔",
        DanmuEventKind::Lottery => "✦",
        DanmuEventKind::Moderation => "!",
        DanmuEventKind::RoomStatus => "◉",
        DanmuEventKind::System => "·",
    }
}

fn is_assistant_message(event: &DanmuEvent, app: &RuntimeState) -> bool {
    event.kind == DanmuEventKind::Danmu
        && app
            .assistant_accounts
            .independent_user_id(&app.account_status)
            .is_some_and(|uid| event.author_id.as_deref() == Some(uid))
}

fn event_lines(
    event: &DanmuEvent,
    app: &RuntimeState,
    palette: Palette,
    width: u16,
) -> Text<'static> {
    let broadcaster = app
        .room
        .as_ref()
        .is_some_and(|room| event.author_id.as_deref() == Some(room.broadcaster_id.as_str()));
    let assistant = is_assistant_message(event, app);
    let mut metadata = Vec::new();
    if app.show_time {
        metadata.push(Span::styled(
            event
                .timestamp
                .with_timezone(&Local)
                .format("%H:%M ")
                .to_string(),
            Style::default().fg(palette.time),
        ));
    }
    if event.origin == DanmuEventOrigin::Archived {
        metadata.push(Span::styled("↶ ", Style::default().fg(palette.time)));
    }
    if event.kind != DanmuEventKind::Danmu {
        let color = event_color(event.kind, palette);
        metadata.push(Span::styled(
            format!(" {} {} ", event_marker(event.kind), kind_label(event.kind)),
            Style::default()
                .fg(palette.background)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        ));
        metadata.push(Span::raw(" "));
    }
    if app.show_name
        && let Some(name) = event.username.as_deref()
    {
        let name = clean_platform_markup(name);
        if broadcaster {
            metadata.push(Span::styled(
                "♚ ",
                Style::default()
                    .fg(palette.rank)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        metadata.push(Span::styled(
            name,
            Style::default()
                .fg(if assistant {
                    ASSISTANT_MESSAGE_COLOR
                } else if broadcaster {
                    palette.host
                } else {
                    palette.name
                })
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.show_name && event.kind == DanmuEventKind::Danmu {
        metadata.push(Span::styled(
            kind_label(event.kind),
            Style::default()
                .fg(palette.name)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.runner.running
        && let Some(mark) = app.bridge.mark(&event.id)
    {
        let color = match mark {
            bridge::Mark::Processing => Color::Yellow,
            bridge::Mark::Confirmed => Color::Green,
            bridge::Mark::Finished => Color::DarkGray,
            bridge::Mark::Failed => Color::Red,
        };
        metadata.push(Span::raw(" "));
        metadata.push(Span::styled("◆", Style::default().fg(color)));
    }
    let featured = app
        .session
        .featured_event
        .as_ref()
        .is_some_and(|item| item.id == event.id);
    let content = Span::styled(
        visible_event_content(event),
        Style::default()
            .fg(event_color(event.kind, palette))
            .add_modifier(if featured {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
    );
    let alignment = if app.layout_chat && broadcaster {
        Alignment::Right
    } else {
        Alignment::Left
    };
    if app.layout_chat || !app.config.single_line {
        let mut lines = wrap_styled_spans(metadata, width, alignment);
        lines.extend(wrap_styled_spans(vec![content], width, alignment));
        if app.layout_chat {
            lines.push(Line::raw(""));
        }
        Text::from(lines)
    } else {
        if !metadata.is_empty() {
            metadata.push(Span::raw(" "));
        }
        metadata.push(content);
        Text::from(wrap_styled_spans(metadata, width, Alignment::Left))
    }
}

fn wrap_styled_spans(
    spans: Vec<Span<'static>>,
    width: u16,
    alignment: Alignment,
) -> Vec<Line<'static>> {
    if spans.is_empty() {
        return Vec::new();
    }
    let width = usize::from(width).max(1);
    let mut lines = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0;
    for span in spans {
        let style = span.style;
        let mut segment = String::new();
        for grapheme in span.content.graphemes(true) {
            if grapheme == "\n" {
                if !segment.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut segment), style));
                }
                lines.push(Line::from(std::mem::take(&mut current_spans)).alignment(alignment));
                current_width = 0;
                continue;
            }
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if current_width > 0 && current_width + grapheme_width > width {
                if !segment.is_empty() {
                    current_spans.push(Span::styled(std::mem::take(&mut segment), style));
                }
                lines.push(Line::from(std::mem::take(&mut current_spans)).alignment(alignment));
                current_width = 0;
            }
            segment.push_str(grapheme);
            current_width += grapheme_width;
        }
        if !segment.is_empty() {
            current_spans.push(Span::styled(segment, style));
        }
    }
    if !current_spans.is_empty() || lines.is_empty() {
        lines.push(Line::from(current_spans).alignment(alignment));
    }
    lines
}

fn clean_platform_markup(value: &str) -> String {
    value.replace("<%", "").replace("%>", "")
}

fn visible_event_content(event: &DanmuEvent) -> String {
    let content = clean_platform_markup(&map_event_emotes(event));
    if let Some(target) = event.reply_to.as_deref() {
        let target = clean_platform_markup(target);
        let target = target.trim();
        let already_in_body = content
            .strip_prefix('@')
            .and_then(|body| body.strip_prefix(target))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
        if !target.is_empty() && !already_in_body {
            return format!("@{target} {content}");
        }
    }
    if event.kind != DanmuEventKind::Danmu
        && let Some(username) = event.username.as_deref()
    {
        let username = clean_platform_markup(username);
        if let Some(remainder) = content.strip_prefix(&username) {
            let remainder = remainder.trim();
            return if remainder.is_empty() {
                kind_label(event.kind).into()
            } else {
                remainder.to_string()
            };
        }
    }
    content
}

fn visible_input(app: &RuntimeState) -> String {
    if app.secret_target.is_some() {
        "•".repeat(app.input.to_string().graphemes(true).count())
    } else {
        app.input.to_string()
    }
}

fn connection_badge(app: &RuntimeState) -> String {
    if app.connection.starts_with("已连接") {
        app.last_realtime_at
            .as_ref()
            .map(|timestamp| format!("实时 {}", timestamp.format("%H:%M:%S")))
            .unwrap_or_else(|| "已连接".into())
    } else if app.connection.contains("重连") {
        "正在重连".into()
    } else {
        "正在连接".into()
    }
}

fn color_luma(color: Color) -> Option<u32> {
    match color {
        Color::Rgb(red, green, blue) => {
            Some((299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue)) / 1000)
        }
        _ => None,
    }
}

fn contrast_foreground(background: Color, palette: Palette) -> Color {
    let Some(background_luma) = color_luma(background) else {
        return palette.content;
    };
    let content_contrast =
        color_luma(palette.content).map_or(0, |luma| luma.abs_diff(background_luma));
    let canvas_contrast =
        color_luma(palette.background).map_or(0, |luma| luma.abs_diff(background_luma));
    if content_contrast >= canvas_contrast {
        palette.content
    } else {
        palette.background
    }
}
fn powerline_title(segments: Vec<(String, Color)>) -> Line<'static> {
    Line::from(
        segments
            .into_iter()
            .map(|(label, foreground)| {
                Span::styled(
                    format!(" {label} "),
                    Style::default()
                        .fg(foreground)
                        .bg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn delivery_status_title(app: &RuntimeState, palette: Palette) -> Line<'static> {
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let (label, color) = match app.delivery_status {
        DeliveryStatus::Idle => ("↑", palette.success),
        DeliveryStatus::Sending | DeliveryStatus::AwaitingEcho | DeliveryStatus::Verifying => (
            SPINNER[app.animation_tick as usize % SPINNER.len()],
            palette.info,
        ),
        DeliveryStatus::Delivered => ("✓", palette.success),
        DeliveryStatus::Uncertain => ("?", palette.warning),
        DeliveryStatus::Failed => ("×", palette.warning),
    };
    Line::from(Span::styled(
        format!(" {label} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn input_business_title(app: &RuntimeState, palette: Palette, width: u16) -> Line<'static> {
    let watched = app
        .watched
        .map_or_else(|| "--".into(), |value| value.to_string());
    let likes = app
        .likes
        .map_or_else(|| "--".into(), |value| value.to_string());
    let online = app
        .online_viewers
        .map_or_else(|| "--".into(), |value| value.to_string());
    let mut segments = Vec::with_capacity(3);
    let mut remaining = usize::from(width);
    for (label, value, color) in [
        ("◉", watched, palette.rank),
        ("♥", likes, palette.warning),
        ("●", online, palette.success),
    ] {
        let needed = UnicodeWidthStr::width(label) + UnicodeWidthStr::width(value.as_str()) + 3;
        if needed > remaining {
            break;
        }
        remaining -= needed;
        segments.push((format!("{label} {value}"), color));
    }
    powerline_title(segments)
}

fn line_display_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn input_top_line(
    left: Line<'static>,
    right: Line<'static>,
    width: u16,
    palette: Palette,
    microphone: Option<(MicrophoneState, Option<MicrophoneLevel>)>,
) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let fill_width = usize::from(width)
        .saturating_sub(6)
        .saturating_sub(line_display_width(&left))
        .saturating_sub(line_display_width(&right));
    let meter =
        microphone.and_then(|(state, level)| microphone_meter(state, level, fill_width, palette));
    let meter_width = meter.as_ref().map_or(0, |meter| meter.width);
    let remaining = fill_width.saturating_sub(meter_width);
    let before_meter = remaining / 2;
    let after_meter = remaining - before_meter;
    let meter_span_count = meter.as_ref().map_or(0, |meter| meter.spans.len());
    let mut spans = Vec::with_capacity(left.spans.len() + right.spans.len() + meter_span_count + 4);
    spans.push(Span::styled("╭──", border_style));
    spans.extend(left.spans);
    spans.push(Span::styled("─".repeat(before_meter), border_style));
    if let Some(meter) = meter {
        spans.extend(meter.spans);
    }
    spans.push(Span::styled("─".repeat(after_meter), border_style));
    spans.extend(right.spans);
    spans.push(Span::styled("──╮", border_style));
    Line::from(spans)
}

fn input_prompt_line(app: &RuntimeState, width: u16, palette: Palette) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let input = visible_input(app);
    let input_width = UnicodeWidthStr::width(input.as_str());
    let fill_width = usize::from(width)
        .saturating_sub(5)
        .saturating_sub(input_width);
    Line::from(vec![
        Span::styled("╰─ ", border_style),
        Span::styled(input, Style::default().fg(palette.content)),
        Span::raw(" ".repeat(fill_width)),
        Span::styled("─╯", border_style),
    ])
}

const MAX_INPUT_CONTENT_LINES: usize = 4;

fn input_content_width(width: u16) -> usize {
    usize::from(width.saturating_sub(5)).max(1)
}

fn wrapped_input_lines(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for grapheme in value.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if line_width > 0 && line_width + grapheme_width > width {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
        line.push_str(grapheme);
        line_width += grapheme_width;
        if line_width >= width {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
    }
    lines.push(line);
    lines
}

fn wrapped_cursor_position(value: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut row = 0;
    let mut column = 0;
    for grapheme in value.graphemes(true).take(cursor) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if column > 0 && column + grapheme_width > width {
            row += 1;
            column = 0;
        }
        column += grapheme_width;
        if column >= width {
            row += 1;
            column = 0;
        }
    }
    (row, column)
}

fn input_area_height(app: &RuntimeState, width: u16) -> u16 {
    let lines = wrapped_input_lines(&visible_input(app), input_content_width(width)).len();
    u16::try_from(lines.min(MAX_INPUT_CONTENT_LINES)).unwrap_or(1) + 1
}

fn input_content_line(content: &str, width: u16, palette: Palette) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let content_width = input_content_width(width);
    let fill_width = content_width.saturating_sub(UnicodeWidthStr::width(content));
    Line::from(vec![
        Span::styled("│  ", border_style),
        Span::styled(content.to_string(), Style::default().fg(palette.content)),
        Span::raw(" ".repeat(fill_width)),
        Span::styled(" │", border_style),
    ])
}

fn input_final_line(content: &str, width: u16, palette: Palette) -> Line<'static> {
    let border_style = Style::default().fg(palette.info);
    let content_width = input_content_width(width);
    let fill_width = content_width.saturating_sub(UnicodeWidthStr::width(content));
    Line::from(vec![
        Span::styled("╰─ ", border_style),
        Span::styled(content.to_string(), Style::default().fg(palette.content)),
        Span::raw(" ".repeat(fill_width)),
        Span::styled("─╯", border_style),
    ])
}

fn draw_input(frame: &mut ratatui::Frame, area: Rect, app: &RuntimeState, palette: Palette) {
    let (left, right) = if app.candidate_edit.is_some() {
        (
            powerline_title(vec![(
                "编辑待确认回复 · Enter保存 · Esc取消 · 不发送".into(),
                palette.warning,
            )]),
            Line::default(),
        )
    } else if app.command_search_draft.is_some() {
        (
            Line::from(" 命令搜索 · 输入中文 · Esc恢复草稿 "),
            Line::default(),
        )
    } else if app.secret_target.is_some() {
        (
            powerline_title(vec![(
                match app.secret_target {
                    Some(SecretTarget::AssistantApiKey) => {
                        "API Key · Enter保存 · Esc取消 · 不发送".into()
                    }
                    _ => "OBS 密码 · Enter保存 · Esc取消".into(),
                },
                palette.warning,
            )]),
            Line::default(),
        )
    } else if app.stop_flow.is_some() {
        (
            powerline_title(vec![("停止推流 · 请在弹窗中操作".into(), palette.warning)]),
            Line::default(),
        )
    } else {
        let status = delivery_status_title(app, palette);
        let available = area
            .width
            .saturating_sub(line_display_width(&status) as u16 + 6);
        (status, input_business_title(app, palette, available))
    };

    let meter_context =
        if app.secret_target.is_none() && !app.stop_flow.is_some() && app.obs_error.is_none() {
            app.obs_status
                .as_ref()
                .map(|status| (status.microphone, app.microphone_level))
        } else {
            None
        };

    let visible = visible_input(app);
    if area.width < 6 {
        frame.render_widget(
            Paragraph::new(vec![
                input_top_line(left, right, area.width, palette, meter_context),
                input_prompt_line(app, area.width, palette),
            ]),
            area,
        );
        let before = visible
            .graphemes(true)
            .take(app.input.cursor())
            .collect::<String>();
        let width = UnicodeWidthStr::width(before.as_str()) as u16;
        if app.stop_flow.is_some() || app.assistant_panel.is_some() {
            return;
        }
        frame.set_cursor_position((
            area.x + 3 + width.min(area.width.saturating_sub(4)),
            area.y + 1,
        ));
        return;
    }

    let content_width = input_content_width(area.width);
    let wrapped = wrapped_input_lines(&visible, content_width);
    let (cursor_row, cursor_column) =
        wrapped_cursor_position(&visible, app.input.cursor(), content_width);
    let content_rows = usize::from(area.height.saturating_sub(1)).max(1);
    let viewport_start = cursor_row
        .saturating_sub(content_rows.saturating_sub(1))
        .min(wrapped.len().saturating_sub(content_rows));
    let mut lines = Vec::with_capacity(content_rows + 1);
    lines.push(input_top_line(
        left,
        right,
        area.width,
        palette,
        meter_context,
    ));
    for row in 0..content_rows {
        let content = wrapped
            .get(viewport_start + row)
            .map(String::as_str)
            .unwrap_or_default();
        if row + 1 == content_rows {
            lines.push(input_final_line(content, area.width, palette));
        } else {
            lines.push(input_content_line(content, area.width, palette));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
    if app.stop_flow.is_some() || app.assistant_panel.is_some() {
        return;
    }
    frame.set_cursor_position((
        area.x + 3 + u16::try_from(cursor_column).unwrap_or(u16::MAX),
        area.y + 1 + u16::try_from(cursor_row.saturating_sub(viewport_start)).unwrap_or(0),
    ));
}

fn map_event_emotes(event: &DanmuEvent) -> String {
    event
        .emotes
        .iter()
        .fold(map_bili_emotes(&event.content), |content, emote| {
            let fallback = if emote.fallback.is_empty() {
                "🙂"
            } else {
                emote.fallback.as_str()
            };
            content.replace(&emote.text, fallback)
        })
}

fn map_bili_emotes(content: &str) -> String {
    [
        ("[dog]", "🐶"),
        ("[doge]", "🐕"),
        ("[妙啊]", "👍"),
        ("[笑哭]", "😂"),
        ("[辣眼睛]", "🙈"),
        ("[吃瓜]", "🍉"),
        ("[鼓掌]", "👏"),
        ("[赞]", "👍"),
        ("[爱心]", "❤️"),
        ("[捂脸]", "🤦"),
        ("[呲牙]", "😁"),
        ("[大哭]", "😭"),
        ("[花]", "🌸"),
        ("[委屈]", "🥺"),
        ("[微笑]", "🙂"),
        ("[滑稽]", "😏"),
        ("[疑惑]", "🤔"),
        ("[惊讶]", "😮"),
        ("[害羞]", "😊"),
        ("[生气]", "😠"),
        ("[无语]", "😑"),
        ("[口罩]", "😷"),
        ("[星星眼]", "🤩"),
        ("[OK]", "👌"),
    ]
    .into_iter()
    .fold(content.to_string(), |value, (code, emoji)| {
        value.replace(code, emoji)
    })
}

fn event_color(kind: DanmuEventKind, palette: Palette) -> Color {
    match kind {
        DanmuEventKind::Danmu => palette.content,
        DanmuEventKind::Gift => palette.rank,
        DanmuEventKind::GuardEvent => palette.success,
        DanmuEventKind::Superchat => palette.warning,
        DanmuEventKind::Enter | DanmuEventKind::RoomStatus => palette.info,
        DanmuEventKind::Like | DanmuEventKind::Moderation => palette.warning,
        DanmuEventKind::Follow => palette.success,
        DanmuEventKind::Share => palette.name,
        DanmuEventKind::Pk | DanmuEventKind::Lottery => palette.rank,
        DanmuEventKind::System => palette.time,
    }
}
fn sanitize_input(value: String) -> String {
    value
        .chars()
        .filter(|character| {
            !matches!(
                *character,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
            )
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub async fn interactive_login(account: AccountClient) -> Result<()> {
    let challenge = account.login_challenge().await?;
    println!("请使用哔哩哔哩客户端扫码登录：\n");
    for line in qr_lines(challenge.url.as_str())? {
        println!("{line}");
    }
    println!("\n二维码地址：{}", challenge.url);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match account.poll_login(&challenge.key).await? {
            LoginPoll::Waiting => print!("."),
            LoginPoll::Scanned => print!(" 已扫码，等待确认"),
            LoginPoll::Expired => {
                return Err(anyhow!("登录二维码已过期，请重新运行 danmu --login"));
            }
            LoginPoll::SignedIn(AccountStatus::SignedIn { display_name, .. }) => {
                println!("\n登录成功：{display_name}");
                return Ok(());
            }
            LoginPoll::SignedIn(AccountStatus::SignedOut) => return Err(anyhow!("登录态验证失败")),
        }
        use std::io::Write;
        io::stdout().flush()?;
    }
}

pub async fn configure_obs(path: &std::path::Path) -> Result<()> {
    let mut configuration = crate::obs::ObsConfiguration::load(path)?;
    println!("OBS WebSocket 主机 [{}]：", configuration.host);
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if !input.trim().is_empty() {
        configuration.host = input.trim().into();
    }
    println!("OBS WebSocket 端口 [{}]：", configuration.port);
    input.clear();
    io::stdin().read_line(&mut input)?;
    if let Ok(port) = input.trim().parse::<u16>() {
        configuration.port = port;
    }
    println!("默认直播场景 [{}]：", configuration.default_live_scene);
    input.clear();
    io::stdin().read_line(&mut input)?;
    if !input.trim().is_empty() {
        configuration.default_live_scene = input.trim().into();
    }
    println!("麦克风输入 [{}]：", configuration.microphone_input_name);
    input.clear();
    io::stdin().read_line(&mut input)?;
    if !input.trim().is_empty() {
        configuration.microphone_input_name = input.trim().into();
    }
    let password = rpassword::prompt_password("OBS WebSocket 密码（留空保持当前值）：")?;
    if !password.is_empty() {
        use crate::obs::ObsPasswordStore;
        crate::obs::FileObsPasswordStore::new(path.to_path_buf(), true).write(
            &configuration.host,
            configuration.port,
            &password,
        )?;
    }
    configuration.save(path)?;
    println!("OBS 配置已保存：{}", path.display());
    println!("OBS 密码保存在 TUI 私有文件中，不使用系统钥匙串。");
    Ok(())
}

pub(crate) fn presentation_config(paths: &crate::storage::StoragePaths) -> Result<TerminalConfig> {
    let themes = crate::theme::ThemeCatalog::load(paths.themes_file.clone())?;
    let theme_name = themes.selected().to_owned();
    let (_, palette) = themes.resolve(&theme_name)?;
    Ok(TerminalConfig {
        config_path: paths.config_file.clone(),
        instance: None,
        history_idle_seconds: 0,
        room_id: String::new(),
        single_line: true,
        chat_layout: false,
        show_time: true,
        show_name: true,
        palette,
        theme_name,
        themes,
    })
}

pub async fn run(
    mut config: TerminalConfig,
    mut paths: crate::storage::StoragePaths,
) -> Result<()> {
    paths.config_file = std::path::absolute(&config.config_path)?;
    config.config_path.clone_from(&paths.config_file);
    let room_id = config.room_id.clone();
    let external_instance = config.instance.clone();
    let handle = crate::runtime::open(crate::runtime::RuntimeOptions {
        obs_passwords: std::sync::Arc::new(crate::obs::FileObsPasswordStore::new(
            paths.obs_configuration.clone(),
            true,
        )),
        storage: paths,
        workspace_default: crate::workspace::default_path()?,
        mode: crate::runtime::RuntimeMode::Live,
        external_instance,
    })
    .await?;
    handle
        .presentation(move |state| {
            Box::pin(async move {
                state.layout_chat = config.chat_layout;
                state.show_name = config.show_name;
                state.show_time = config.show_time;
                state.config = config;
                state.record_reading_config();
                state.startup_gate = Some(StartupGate::Checking);
                state.startup_started = Instant::now();
            })
        })
        .await?;
    let result = async {
        handle
            .execute(crate::runtime::RuntimeCommand::Connect { room_id })
            .await?;
        handle
            .execute(crate::runtime::RuntimeCommand::Obs(
                crate::runtime::ObsCommand::Connect,
            ))
            .await?;
        drive(handle.clone(), false).await
    }
    .await;
    result.and(handle.shutdown().await)
}

async fn drive(handle: crate::runtime::RuntimeHandle, local: bool) -> Result<()> {
    let shutdown = shutdown_signal()?;
    tokio::pin!(shutdown);
    let mut terminal = TerminalGuard::enter()?;
    let mut projection = Some(Terminal::new(ratatui::backend::TestBackend::new(1, 1))?);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(90));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tick.tick() => {
                let size = terminal.terminal.size()?;
                let mut buffered = projection.take().expect("presentation buffer belongs to current draw");
                if buffered.backend().buffer().area.width != size.width || buffered.backend().buffer().area.height != size.height {
                    buffered.backend_mut().resize(size.width, size.height);
                }
                let (buffered, mouse_capture, result) = handle.presentation(move |state| Box::pin(async move {
                    state.animation_tick = state.animation_tick.wrapping_add(1);
                    state.advance_history_at(Instant::now());
                    state.advance_startup_projection();
                    let result = buffered.draw(|frame| {
                        if let Some(gate) = &state.startup_gate {
                            draw_startup(frame, state.config.palette, state.animation_tick, &state.startup_view, gate, state.startup_started.elapsed(), state.room.is_some());
                        } else { draw(frame, state); }
                    }).map(|_| ());
                    let mouse_capture = state.stop_flow.is_some() || state.login_qr.is_some() || state.secret_target.is_some()
                        || state.candidate_edit.is_some() || state.help.is_some() || state.assistant_panel.is_some()
                        || state.command_search_draft.is_some() || state.input.starts_with('/');
                    (buffered, mouse_capture, result)
                })).await?;
                result?;
                // Only this host touches stdout. A paused terminal consumer cannot hold
                // the runtime's matcher, collection, or archival event loop hostage.
                terminal.set_mouse_capture(mouse_capture)?;
                terminal.terminal.draw(|frame| {
                    let area = frame.area();
                    let source = buffered.backend().buffer();
                    for y in 0..area.height.min(source.area.height) {
                        for x in 0..area.width.min(source.area.width) {
                            frame.buffer_mut()[(x, y)].clone_from(&source[(x, y)]);
                        }
                    }
                    let cursor = buffered.backend().cursor_position();
                    if buffered.backend().cursor_visible() && cursor.x < area.width && cursor.y < area.height {
                        frame.set_cursor_position(cursor);
                    }
                })?;
                projection = Some(buffered);
            }
            event = events.next() => {
                let Some(event) = event else { break; };
                let event = event?;
                let (quit, effects) = handle.presentation(move |state| Box::pin(async move {
                    let tx = state.ui_tx.as_ref().expect("core event channel").clone();
                    let result = match event {
                        Event::Key(key) if local && state.handle_local_key(key)? => Ok(state.quit_requested),
                        Event::Key(key) if state.startup_gate.is_some() => state.handle_startup_key(key).await,
                        Event::Key(key) => state.handle_key(key, tx).await,
                        Event::Paste(text) => { state.handle_paste(&text); Ok(false) }
                        Event::Mouse(mouse) if state.handle_mouse(mouse) => state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx).await,
                        _ => Ok(false),
                    };
                    let quit = match result {
                        Ok(quit) => quit,
                        Err(error) => { state.set_notice(error.to_string(), NoticeLevel::Error); false }
                    };
                    Ok::<_, anyhow::Error>((quit, std::mem::take(&mut state.effects)))
                })).await??;
                for effect in effects {
                    match effect {
                        crate::runtime::HostEffect::OpenBrowser { operation_id, url } => {
                            if let Err(error) = open_chatgpt_browser(&url) {
                                let _ = handle.execute(crate::runtime::RuntimeCommand::CancelOperation { operation_id }).await;
                                handle.presentation(move |state| Box::pin(async move { state.set_notice(error.to_string(), NoticeLevel::Error); })).await?;
                            }
                        }
                    }
                }
                if quit { break; }
            }
        }
    }
    Ok(())
}

impl RuntimeState {
    pub(crate) fn presentation_anchors(&self) -> (Option<String>, Option<String>) {
        (
            self.selection_active
                .then(|| self.session.recent_events.get(self.selected))
                .flatten()
                .map(|event| event.id.clone()),
            (self.scroll_offset > 0)
                .then(|| self.session.recent_events.get(self.scroll_offset))
                .flatten()
                .map(|event| event.id.clone()),
        )
    }
    pub(crate) fn restore_presentation_anchors(
        &mut self,
        anchors: (Option<String>, Option<String>),
        event: &DanmuEvent,
        live: bool,
    ) {
        self.activity.push(event, Instant::now(), Utc::now());
        self.selected = anchors
            .0
            .as_deref()
            .and_then(|id| {
                self.session
                    .recent_events
                    .iter()
                    .position(|event| event.id == id)
            })
            .unwrap_or(0);
        self.scroll_offset = anchors
            .1
            .as_deref()
            .and_then(|id| {
                self.session
                    .recent_events
                    .iter()
                    .position(|event| event.id == id)
            })
            .unwrap_or(0);
        if live && (self.selection_active || self.scroll_offset > 0) {
            self.unread_live_count = self.unread_live_count.saturating_add(1);
        } else if !self.selection_active && self.scroll_offset == 0 {
            self.unread_live_count = 0;
        }
    }
    fn handle_local_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind != crossterm::event::KeyEventKind::Press
            || key.code != KeyCode::Enter
            || !key.modifiers.is_empty()
            || self.selection_active
            || self.command_search_draft.is_some()
            || self.candidate_edit.is_some()
            || self.assistant_panel.is_some()
            || self.help.is_some()
            || self.stop_flow.is_some()
            || self.secret_target.is_some()
            || self.login_qr.is_some()
        {
            return Ok(false);
        }
        if let Some(rest) = self.input.strip_prefix("/event ") {
            if let Some((name, text)) = rest.split_once(' ') {
                let mut event =
                    DanmuEvent::new(DanmuEventKind::Danmu, sanitize_input(text.to_owned()));
                event.username = Some(sanitize_input(name.to_owned()));
                event.author_id = Some(name.to_owned());
                self.ingest_event(event);
            } else {
                self.set_notice("/event 姓名 正文", NoticeLevel::Warning);
            }
        } else if let Some(mode) = self.input.strip_prefix("/local ") {
            let outcome = match mode {
                "confirmed" => 0,
                "uncertain" => 1,
                "rejected" => 2,
                _ => {
                    self.set_notice("/local confirmed|uncertain|rejected", NoticeLevel::Warning);
                    return Ok(true);
                }
            };
            if let Some(transport) = &self.local_transport {
                transport
                    .outcome
                    .store(outcome, std::sync::atomic::Ordering::Relaxed);
            }
        } else if self.input == "/session new" {
            if self.session.ended_at.is_none() {
                self.session
                    .end(Utc::now(), DanmuSessionEndReason::Completed);
                self.journal.end(&self.session)?;
            }
            self.bridge.new_session();
            self.session = DanmuSession::new("local");
            self.journal.start(&self.session)?;
            self.new_message_count = 0;
            self.events_dirty = true;
        } else if self.input == "/session end" {
            self.bridge.end();
            if self.session.ended_at.is_none() {
                self.session
                    .end(Utc::now(), DanmuSessionEndReason::Completed);
                self.journal.end(&self.session)?;
            }
        } else {
            return Ok(false);
        }
        self.input.take();
        Ok(true)
    }
}

impl RuntimeState {
    fn advance_startup_projection(&mut self) {
        if self.startup_gate.is_none() {
            return;
        }
        if self
            .startup_gate
            .as_ref()
            .is_some_and(StartupGate::can_finish)
        {
            self.startup_gate = None;
            return;
        }
        self.startup_view.local = StartupCheck::Passed("已读取");
        self.startup_view.account = if self.room.is_some() {
            StartupCheck::Passed(
                if matches!(self.account_status, AccountStatus::SignedIn { .. }) {
                    "已登录"
                } else {
                    "未登录 · 监看模式"
                },
            )
        } else {
            StartupCheck::Running
        };
        self.startup_view.room = if self.room.is_some() {
            StartupCheck::Passed("房间可访问")
        } else if self.connection.starts_with("连接失败") {
            StartupCheck::Warning("读取失败 · 可重试")
        } else {
            StartupCheck::Running
        };
        self.startup_view.obs = if self.obs_checked_at.is_some() {
            match &self.obs_error {
                None => StartupCheck::Passed("已连接"),
                Some(error) if error.contains("/obs config password") => {
                    StartupCheck::Warning("需要密码 · /obs config password")
                }
                Some(_) => StartupCheck::Warning("连接失败 · 进入后重试"),
            }
        } else {
            StartupCheck::Running
        };
        if matches!(self.startup_gate, Some(StartupGate::Checking)) {
            if self.room.is_some() {
                self.startup_gate = Some(StartupGate::Ready);
            } else if self.startup_view.has_warning() {
                self.startup_gate = Some(StartupGate::Blocked);
            }
        }
    }
    async fn handle_startup_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind != crossterm::event::KeyEventKind::Press {
            return Ok(false);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(true);
        }
        if let Some(StartupGate::EnteringObsPassword(editor)) = self.startup_gate.as_mut() {
            match key.code {
                KeyCode::Esc => self.startup_gate = Some(StartupGate::Blocked),
                KeyCode::Char(character) if !character.is_control() => {
                    editor.insert_text(character.encode_utf8(&mut [0; 4]))
                }
                KeyCode::Backspace => editor.delete_before_cursor(),
                KeyCode::Enter => {
                    let value = editor.to_string();
                    match self.obs.set_password(&value).await {
                        Ok(()) => {
                            self.execute_command(crate::runtime::RuntimeCommand::Obs(
                                crate::runtime::ObsCommand::Refresh,
                            ))
                            .await?;
                            self.execute_command(crate::runtime::RuntimeCommand::Connect {
                                room_id: self.config.room_id.clone(),
                            })
                            .await?;
                            self.startup_gate = Some(StartupGate::Checking);
                        }
                        Err(error) => {
                            self.set_notice(format!("OBS密码未保存：{error}"), NoticeLevel::Error)
                        }
                    }
                }
                _ => {}
            }
        } else if matches!(self.startup_gate, Some(StartupGate::Blocked)) {
            match key.code {
                KeyCode::Char('s' | 'S') => self.startup_gate = Some(StartupGate::Skipped),
                KeyCode::Enter if self.startup_view.obs_requires_password() => {
                    self.startup_gate =
                        Some(StartupGate::EnteringObsPassword(EditorInput::default()))
                }
                KeyCode::Enter => {
                    self.execute_command(crate::runtime::RuntimeCommand::Connect {
                        room_id: self.config.room_id.clone(),
                    })
                    .await?;
                    self.startup_gate = Some(StartupGate::Checking);
                }
                _ => {}
            }
        }
        Ok(false)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::ObsConfiguration;
    use ratatui::backend::TestBackend;
    pub(crate) fn test_app(temp: &tempfile::TempDir, session: DanmuSession) -> RuntimeState {
        replay::app(temp.path(), session)
    }

    #[test]
    pub(crate) fn independent_assistant_message_color_requires_uid_not_name_or_prefix() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        crate::storage::write_private_atomic(
            &temp.path().join("assistant-account.json"),
            &serde_json::to_vec(&serde_json::json!({"mode":"independent", "account":id})).unwrap(),
        )
        .unwrap();
        crate::storage::write_private_atomic(
            &temp.path().join(format!("AssistantAccounts/{id}.json")),
            &serde_json::to_vec(&serde_json::json!({
                "cookieHeader":"SESSDATA=fake-local; bili_jct=fake-local; DedeUserID=22",
                "csrf":"fake-local",
                "identity":{"SignedIn":{"display_name":"助手", "user_id":"22"}}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "✦ 同名同前缀回复");
        event.username = Some("助手".into());
        let emphasized = |event: &DanmuEvent, app: &RuntimeState| {
            event_lines(event, app, app.config.palette, 80)
                .lines
                .into_iter()
                .flat_map(|line| line.spans)
                .filter(|span| span.style.fg == Some(ASSISTANT_MESSAGE_COLOR))
                .map(|span| span.content.into_owned())
                .collect::<Vec<_>>()
        };
        for uid in [None, Some("33")] {
            event.author_id = uid.map(str::to_owned);
            assert!(emphasized(&event, &app).is_empty());
        }
        event.author_id = Some("22".into());
        for running in [true, false] {
            app.runner.running = running;
            assert_eq!(emphasized(&event, &app), ["助手"]);
        }
        let body = event_lines(&event, &app, app.config.palette, 80);
        let body = body
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("同名同前缀回复"))
            .unwrap();
        assert_eq!(body.style.fg, Some(app.config.palette.content));
        assert!(!body.style.add_modifier.contains(Modifier::BOLD));

        app.show_name = false;
        assert!(emphasized(&event, &app).is_empty());
        app.account_status = AccountStatus::SignedIn {
            display_name: "主账号".into(),
            user_id: "22".into(),
        };
        assert!(emphasized(&event, &app).is_empty());
        app.account_status = AccountStatus::SignedOut;
        app.assistant_accounts.reuse_main().unwrap();
        assert!(emphasized(&event, &app).is_empty());
    }

    #[tokio::test]
    pub(crate) async fn stop_flow_requires_selection_and_supports_cancelling_countdown() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, mut rx) = mpsc::channel(8);
        let now = Instant::now();
        app.command("/obs stop", tx.clone()).await.unwrap();
        app.set_notice_at("停止推流有中断直播风险", NoticeLevel::Warning, now);
        app.expire_notice_at(now + DELIVERY_NOTICE_LIFETIME);
        app.advance_stop_at(now + DELIVERY_NOTICE_LIFETIME, &tx);
        assert!(app.notice.is_empty());
        assert!(matches!(
            app.stop_flow,
            Some(StopFlow::Confirm {
                stop_selected: false
            })
        ));
        assert!(rx.try_recv().is_err(), "提醒消退不能批准OBS停止");
        assert!(
            !app.handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                tx.clone()
            )
            .await
            .unwrap()
        );
        app.advance_stop_at(now + Duration::from_secs(10), &tx);
        assert!(app.stop_flow.is_none());

        app.command("/obs stop", tx.clone()).await.unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        // Holding Enter must not confirm through a repeat event.
        app.handle_key(
            KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Repeat,
            ),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(matches!(app.stop_flow, Some(StopFlow::Confirm { .. })));
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(matches!(app.stop_flow, Some(StopFlow::Countdown { .. })));
        app.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.input.is_empty());
        assert!(
            !app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap()
        );
        app.advance_stop_at(now + Duration::from_secs(10), &tx);
        assert!(app.stop_flow.is_none());

        app.command("/obs stop", tx.clone()).await.unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(
            app.stop_flow.is_none(),
            "reopening must reset selection to return"
        );
    }

    #[tokio::test]
    pub(crate) async fn stop_flow_sends_one_request_only_after_three_seconds() {
        use futures_util::SinkExt;
        use serde_json::{Value, json};
        use tokio::net::TcpListener;
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.obs = ObsController::new(
            ObsConfiguration {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                ..ObsConfiguration::default()
            },
            temp.path().join("obs.json"),
            std::sync::Arc::new(crate::obs::FileObsPasswordStore::new(
                temp.path().join("obs.json"),
                false,
            )),
        );
        let (tx, mut rx) = mpsc::channel(8);
        app.command("/obs stop", tx.clone()).await.unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(matches!(
            app.stop_flow,
            Some(StopFlow::Confirm {
                stop_selected: false
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(app.stop_flow.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        app.command("/obs stop", tx.clone()).await.unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        let Some(StopFlow::Countdown { deadline }) = app.stop_flow else {
            panic!("countdown expected");
        };
        let now = deadline - Duration::from_secs(3);
        app.advance_stop_at(now + Duration::from_millis(2999), &tx);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );

        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            ws.send(Message::Text(
                json!({"op": 0, "d": {"obsWebSocketVersion": "5.6.0", "rpcVersion": 1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let identify: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(identify["op"], 1);
            ws.send(Message::Text(
                json!({"op": 2, "d": {"negotiatedRpcVersion": 1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let mut stops = 0;
            while let Ok(message) =
                tokio::time::timeout(Duration::from_millis(100), ws.next()).await
            {
                let request: Value =
                    serde_json::from_str(message.unwrap().unwrap().to_text().unwrap()).unwrap();
                let response = match request["d"]["requestType"].as_str().unwrap() {
                    "GetVersion" => json!({
                        "obsVersion": "31.0.0", "obsWebSocketVersion": "5.6.0", "rpcVersion": 1,
                        "availableRequests": [], "supportedImageFormats": [],
                        "platform": "macos", "platformDescription": "macOS"
                    }),
                    "StopStream" => {
                        stops += 1;
                        json!({})
                    }
                    "GetStreamStatus" => json!({
                        "outputActive": false, "outputReconnecting": false,
                        "outputTimecode": "00:00:00.000", "outputDuration": 0,
                        "outputCongestion": 0.0, "outputBytes": 0,
                        "outputSkippedFrames": 0, "outputTotalFrames": 0
                    }),
                    other => panic!("unexpected OBS request: {other}"),
                };
                let mut reply = json!({"op": 7, "d": {
                    "requestType": request["d"]["requestType"], "requestId": request["d"]["requestId"],
                    "requestStatus": {"result": true, "code": 100}
                }});
                if request["d"]["requestType"] != "StopStream" {
                    reply["d"]["responseData"] = response;
                }
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
            assert_eq!(stops, 1, "stop must be sent only once");
        });
        app.advance_stop_at(now + Duration::from_secs(3), &tx);
        app.advance_stop_at(now + Duration::from_secs(4), &tx);
        let result = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, CoreEvent::ObsStopDone(Ok(()))));
        app.handle_ui_event(result);
        assert!(app.stop_flow.is_none());
        assert!(!app.quit_requested);
        server.await.unwrap();
    }

    #[tokio::test]
    pub(crate) async fn microphone_shortcuts_are_idempotent_and_require_remote_confirmation() {
        use futures_util::SinkExt;
        use serde_json::{Value, json};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        let temp = crate::test_support::private_tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.obs = ObsController::new(
            crate::obs::ObsConfiguration {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                ..Default::default()
            },
            temp.path().join("obs.json"),
            std::sync::Arc::new(crate::obs::FileObsPasswordStore::new(
                temp.path().join("obs.json"),
                false,
            )),
        );
        let muted = Arc::new(AtomicBool::new(false));
        let ignore_change = Arc::new(AtomicBool::new(false));
        let remote = muted.clone();
        let ignore = ignore_change.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            ws.send(Message::Text(
                json!({"op":0,"d":{"obsWebSocketVersion":"5.6.0","rpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            while let Some(Ok(message)) = ws.next().await {
                if message.is_close() {
                    break;
                }
                let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                let response = match request["d"]["requestType"].as_str().unwrap() {
                    "GetVersion" => {
                        json!({"obsVersion":"31.0.0","obsWebSocketVersion":"5.6.0","rpcVersion":1,"availableRequests":[],"supportedImageFormats":[],"platform":"macos","platformDescription":"macOS"})
                    }
                    "SetInputMute" => {
                        if !ignore.load(Ordering::SeqCst) {
                            remote.store(
                                request["d"]["requestData"]["inputMuted"].as_bool().unwrap(),
                                Ordering::SeqCst,
                            );
                        }
                        json!({})
                    }
                    "GetInputMute" => json!({"inputMuted":remote.load(Ordering::SeqCst)}),
                    other => panic!("unexpected OBS operation: {other}"),
                };
                let mut reply = json!({"op":7,"d":{
                    "requestType":request["d"]["requestType"],"requestId":request["d"]["requestId"],
                    "requestStatus":{"result":true,"code":100}
                }});
                if request["d"]["requestType"] != "SetInputMute" {
                    reply["d"]["responseData"] = response;
                }
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let (tx, mut rx) = mpsc::channel(4);
        app.input = "人工草稿".into();
        for (command, expected) in [("/mute", true), ("/mute", true), ("/unmute", false)] {
            app.open_commands();
            app.handle_paste(command);
            app.handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                tx.clone(),
            )
            .await
            .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap();
            app.handle_ui_event(result);
            assert!(
                matches!(app.notice_level, NoticeLevel::Success),
                "{}",
                app.notice
            );
            assert_eq!(muted.load(Ordering::SeqCst), expected);
            assert_eq!(app.input, "人工草稿");
        }
        ignore_change.store(true, Ordering::SeqCst);
        app.command("/mute", tx).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        app.handle_ui_event(result);
        assert!(
            matches!(app.notice_level, NoticeLevel::Error),
            "{}",
            app.notice
        );
        assert!(!muted.load(Ordering::SeqCst));
        server.abort();
        let _ = server.await;
    }

    #[test]
    pub(crate) fn gift_combo_renders_one_row_without_moving_past_newer_chat() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        for index in 0..2 {
            let gift = crate::domain::GiftDetails {
                name: "人气票".into(),
                action: "投喂".into(),
                blind_gift: None,
                receipts: [(format!("receipt-{index}"), 1)].into(),
                reported_quantity: 0,
            };
            let mut event = DanmuEvent::new(DanmuEventKind::Gift, gift.content());
            event.id = "one-gift-batch".into();
            event.gift = Some(Box::new(gift));
            app.ingest_event(event);
            if index == 0 {
                app.ingest_event(DanmuEvent::new(DanmuEventKind::Danmu, "后来的聊天"));
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let rendered = (0..20)
            .map(|y| {
                (0..100)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace(' ', "");
        assert_eq!(rendered.matches("人气票").count(), 1, "{rendered}");
        assert!(rendered.contains("人气票×2"), "{rendered}");
        assert!(
            rendered.find("人气票").unwrap() < rendered.find("后来的聊天").unwrap(),
            "{rendered}"
        );
    }

    #[test]
    pub(crate) fn optional_startup_checks_do_not_block_and_password_stays_private() {
        let palette = Palette::default();
        let mut startup = StartupView::new();
        startup.account = StartupCheck::Passed("未登录 · 监看模式");
        startup.room = StartupCheck::Passed("房间可访问");
        startup.metrics = StartupCheck::Skipped("未登录 · 已跳过");
        startup.obs = StartupCheck::Warning("需要密码 · /obs config password");
        startup.local = StartupCheck::Passed("已读取");
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();

        assert!(!startup.has_warning());
        startup.account = StartupCheck::Warning("账号未登录");
        startup.metrics = StartupCheck::Warning("指标不可用");
        assert!(!startup.has_warning());
        startup.room = StartupCheck::Warning("直播间不可访问");
        assert!(startup.has_warning());

        let password_gate = StartupGate::EnteringObsPassword(EditorInput::from("secret"));
        terminal
            .draw(|frame| {
                draw_startup(
                    frame,
                    palette,
                    0,
                    &startup,
                    &password_gate,
                    Duration::ZERO,
                    true,
                )
            })
            .unwrap();
        let password_frame = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(password_frame.contains("●●●●●●"), "{password_frame}");
        assert!(!password_frame.contains("secret"), "{password_frame}");
        assert!(StartupGate::Skipped.can_finish());
    }

    #[test]
    pub(crate) fn startup_animation_degrades_to_the_active_compact_check() {
        let startup = StartupView::new();
        let mut terminal = Terminal::new(TestBackend::new(30, 3)).unwrap();
        terminal
            .draw(|frame| {
                draw_startup(
                    frame,
                    Palette::default(),
                    2,
                    &startup,
                    &StartupGate::Checking,
                    Duration::from_secs(1),
                    false,
                )
            })
            .unwrap();
        let rendered = (0..terminal.backend().buffer().area.height)
            .map(|y| {
                (0..terminal.backend().buffer().area.width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("DANMU"));
        assert!(rendered.contains("Elazer"), "{rendered}");
        assert!(rendered.contains("elazer.wang"), "{rendered}");
        assert!(
            rendered.replace(' ', "").contains("恢复本地会话"),
            "{rendered}"
        );
    }
    #[test]
    pub(crate) fn input_header_renders_live_meter_and_hides_it_when_obs_disconnects() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.obs_status = Some(ObsStatus {
            current_scene: "直播".into(),
            stream: crate::obs::StreamState::Live,
            microphone: MicrophoneState::Unmuted,
            compatibility_warning: None,
        });
        app.microphone_level = Some(MicrophoneLevel { peak_db: -12.0 });
        let mut terminal = Terminal::new(TestBackend::new(120, 2)).unwrap();

        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, app.config.palette))
            .unwrap();
        let connected_header = (0..120)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        assert!(connected_header.contains("MIC"));
        assert!(connected_header.contains("-12"));

        app.obs_error = Some("OBS 未连接".into());
        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, app.config.palette))
            .unwrap();
        let disconnected_header = (0..120)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        assert!(!disconnected_header.contains("MIC"));
    }

    #[test]
    pub(crate) fn sanitizes_ime_format_characters() {
        assert_eq!(sanitize_input("你\u{200d}好\u{feff}".into()), "你好");
    }

    #[test]
    pub(crate) fn maps_known_emotes_and_preserves_unknown_codes() {
        assert_eq!(map_bili_emotes("[dog] [未知]"), "🐶 [未知]");
    }

    #[test]
    pub(crate) fn renders_scannable_qr_rows() {
        let rows = qr_lines("https://example.com/login").unwrap();
        assert!(rows.len() > 10);
        assert!(
            rows.iter()
                .any(|row| row.contains('█') || row.contains('▀') || row.contains('▄'))
        );
    }
    #[test]
    pub(crate) fn tui_login_uses_a_smaller_qr_encoding_than_standalone_login() {
        let payload = "x".repeat(131);
        let compact = compact_qr_lines(&payload).unwrap();
        let standard = qr_lines(&payload).unwrap();
        let compact_width = compact
            .iter()
            .map(|row| UnicodeWidthStr::width(row.as_str()))
            .max()
            .unwrap();
        let standard_width = standard
            .iter()
            .map(|row| UnicodeWidthStr::width(row.as_str()))
            .max()
            .unwrap();

        assert!(compact_width <= 45, "{compact_width}");
        assert!(compact.len() <= 23, "{}", compact.len());
        assert!(compact_width < standard_width);
        assert!(compact.len() < standard.len());
    }

    #[test]
    pub(crate) fn qr_overlay_degrades_instead_of_clipping_on_small_terminals() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let app = test_app(&temp, DanmuSession::new("1"));
        let rows = qr_lines(&"x".repeat(131)).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(44, 20)).unwrap();

        terminal
            .draw(|frame| draw_qr(frame, frame.area(), &rows, app.config.palette))
            .unwrap();

        let mut rendered = String::new();
        for y in 0..20 {
            for x in 0..44 {
                rendered.push_str(terminal.backend().buffer()[(x, y)].symbol());
            }
        }
        assert!(
            rendered.contains('终') && rendered.contains('足'),
            "{rendered}"
        );
        assert!(!rendered.contains('█'), "{rendered}");
    }
    #[test]
    pub(crate) fn displays_oldest_event_at_top_and_newest_at_bottom() {
        let mut older = DanmuEvent::new(DanmuEventKind::Danmu, "旧消息");
        older.id = "older".into();
        let mut newer = DanmuEvent::new(DanmuEventKind::Danmu, "新消息");
        newer.id = "newer".into();
        let events = vec![newer, older];
        let ids = display_events(&events)
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["older", "newer"]);
    }

    #[test]
    pub(crate) fn application_notices_render_below_the_danmu_frame() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        session.ingest(DanmuEvent::new(DanmuEventKind::Danmu, "真实弹幕"));
        let mut app = test_app(&temp, session);
        app.set_notice("测试错误提示", NoticeLevel::Error);
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let frame_top = lines.iter().position(|line| line.starts_with('╭')).unwrap();
        let frame_bottom = lines
            .iter()
            .enumerate()
            .skip(frame_top + 1)
            .find_map(|(row, line)| line.contains('╰').then_some(row))
            .unwrap();
        let notice_row = lines
            .iter()
            .position(|line| line.replace(' ', "").contains("测试错误提示"))
            .unwrap_or_else(|| panic!("notice missing from rendered rows: {lines:#?}"));

        assert!(notice_row > frame_bottom);
        assert!(!lines[notice_row].contains('│'));
    }

    #[test]
    pub(crate) fn activity_bursts_and_expiry_leave_chat_rows_unchanged() {
        for chat_layout in [false, true] {
            for (browsing, selecting) in [(false, false), (true, false), (false, true)] {
                let temp = crate::test_support::private_tempdir().unwrap();
                let now = Utc::now();
                let mut session = DanmuSession::with_options("1", 12, now);
                for index in 0..12 {
                    let mut event = DanmuEvent::new(
                        DanmuEventKind::Danmu,
                        format!("历史弹幕-{index:02}，这是一条需要保留的消息"),
                    );
                    event.timestamp = now - chrono::Duration::seconds(60 - index);
                    session.ingest(event);
                }
                let mut app = test_app(&temp, session);
                app.layout_chat = chat_layout;
                if browsing {
                    app.scroll_offset = 4;
                }
                if selecting {
                    app.selection_active = true;
                    app.selected = 4;
                }
                let palette = app.config.palette;
                let mut terminal = Terminal::new(TestBackend::new(42, 10)).unwrap();
                terminal
                    .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
                    .unwrap();
                let before = terminal.backend().buffer().clone();
                for index in 0..30 {
                    let kind = if index % 2 == 0 {
                        DanmuEventKind::Enter
                    } else {
                        DanmuEventKind::Like
                    };
                    let mut event = DanmuEvent::new(kind, "临时互动提示");
                    event.timestamp = now + chrono::Duration::minutes(1);
                    app.ingest_event(event);
                    terminal
                        .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
                        .unwrap();
                    let during = terminal.backend().buffer();
                    for y in 0..9 {
                        for x in 0..42 {
                            assert_eq!(
                                during[(x, y)],
                                before[(x, y)],
                                "chat={chat_layout}, browsing={browsing}, selecting={selecting}, cell=({x},{y})"
                            );
                        }
                    }
                    let mut footer = String::new();
                    let mut x = 0;
                    while x < 42 {
                        let symbol = during[(x, 9)].symbol();
                        footer.push_str(symbol);
                        x += UnicodeWidthStr::width(symbol).max(1) as u16;
                    }
                    assert!(footer.contains("观众"), "{footer}");
                }
                app.expire_notice_at(Instant::now() + Duration::from_secs(10));
                terminal
                    .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
                    .unwrap();
                assert_eq!(terminal.backend().buffer(), &before);
            }
        }
    }

    #[test]
    pub(crate) fn history_scrolling_skips_activity_even_after_it_expires() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let now = Utc::now();
        for index in 0..10 {
            let mut message = DanmuEvent::new(DanmuEventKind::Danmu, format!("message-{index}"));
            message.timestamp = now - chrono::Duration::seconds(30 - index * 2);
            app.ingest_event(message);
            let mut activity = DanmuEvent::new(DanmuEventKind::Like, "过期的点赞");
            activity.timestamp = now - chrono::Duration::seconds(29 - index * 2);
            app.ingest_event(activity);
        }
        for (older, expected) in [
            (true, "message-8"),
            (true, "message-7"),
            (false, "message-8"),
        ] {
            app.scroll_history(older);
            assert_eq!(
                app.session.recent_events[app.scroll_offset].content,
                expected
            );
        }
        app.unread_live_count = 2;
        app.scroll_history(false);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_live_count, 0);
    }

    #[test]
    pub(crate) fn polled_likes_cannot_overwrite_a_newer_realtime_total() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_client_event(BilibiliClientEvent::Likes(300));
        app.handle_ui_event(CoreEvent::Likes(Ok(Some(221))));

        assert_eq!(app.likes, Some(300));
    }

    #[test]
    pub(crate) fn viewer_reply_target_survives_history_reconciliation() {
        use serde_json::json;
        for target in ["另一位观众", "拾穗数据"] {
            let mut metadata = vec![serde_json::Value::Null; 16];
            metadata[15] = json!({"extra": json!({
                "show_reply": true, "reply_mid": 99, "reply_uname": target
            }).to_string()});
            let live = crate::bilibili::parse_command(&json!({
                "cmd": "DANMU_MSG", "info": [metadata, "这个问题怎么看？", [42, "观众"]]
            }))
            .unwrap();
            let mut history = DanmuEvent::new(DanmuEventKind::Danmu, "这个问题怎么看？");
            history.username = Some("观众".into());
            history.author_id = Some("42".into());
            history.timestamp = live.timestamp;
            history.origin = DanmuEventOrigin::History;
            let expected = format!("@{target} 这个问题怎么看？");
            assert_eq!(visible_event_content(&live), expected);
            for (first, second) in [(live.clone(), history.clone()), (history, live)] {
                let mut events = vec![first];
                assert!(reconcile_cross_origin_event(&mut events, &second));
                assert_eq!(visible_event_content(&events[0]), expected);
                assert_eq!(events[0].content, "这个问题怎么看？");
            }
        }
    }

    #[test]
    pub(crate) fn reply_metadata_does_not_drop_body_or_duplicate_text_mentions() {
        use serde_json::json;
        let cases = [
            (serde_json::Value::Null, "普通弹幕", "普通弹幕"),
            (json!("{broken"), "普通弹幕", "普通弹幕"),
            (
                json!(r#"{"show_reply":false,"reply_uname":"观众"}"#),
                "普通弹幕",
                "普通弹幕",
            ),
            (
                json!(r#"{"show_reply":true,"reply_uname":" "}"#),
                "普通弹幕",
                "普通弹幕",
            ),
            (
                json!(r#"{"reply_uname":"观众"}"#),
                "@观众 你好",
                "@观众 你好",
            ),
            (
                json!(r#"{"reply_uname":"观众"}"#),
                "@观众甲 你好",
                "@观众 @观众甲 你好",
            ),
        ];
        for (extra, body, expected) in cases {
            let mut metadata = vec![serde_json::Value::Null; 16];
            metadata[15] = json!({"extra": extra});
            let event = crate::bilibili::parse_command(&json!({
                "cmd": "DANMU_MSG", "info": [metadata, body, [42, "观众乙"]]
            }))
            .unwrap();
            assert_eq!(visible_event_content(&event), expected);
        }
    }

    #[test]
    pub(crate) fn event_badges_use_type_colors_and_strip_bilibili_markup() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Enter, "<%战区超人%> 来了");
        let app = test_app(&temp, DanmuSession::new("1"));
        let palette = app.config.palette;
        let rendered = event_lines(&event, &app, palette, 80);
        let line = &rendered.lines[0];
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("战区超人 来了"));
        assert!(!text.contains("<%"));
        assert!(!text.contains("%>"));
        assert!(
            line.spans
                .iter()
                .any(|span| span.style.bg == Some(palette.info))
        );
        assert_ne!(
            event_color(DanmuEventKind::Enter, palette),
            event_color(DanmuEventKind::Danmu, palette)
        );
        assert_ne!(
            event_color(DanmuEventKind::Gift, palette),
            event_color(DanmuEventKind::Superchat, palette)
        );
        assert_ne!(
            event_color(DanmuEventKind::Follow, palette),
            event_color(DanmuEventKind::Like, palette)
        );
    }

    #[test]
    pub(crate) fn broadcaster_name_has_identity_mark_and_pink_color() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.room = Some(RoomSnapshot {
            room_id: "1".into(),
            broadcaster_id: "42".into(),
            broadcaster_name: "停车拾穗".into(),
            title: "测试直播".into(),
            area: "知识".into(),
            live_started_at: None,
            live_status: RoomLiveStatus::Offline,
        });
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "主播消息");
        event.username = Some("停车拾穗".into());
        event.author_id = Some("42".into());
        let rendered = event_lines(&event, &app, app.config.palette, 80);
        let crown = rendered.lines[0]
            .spans
            .iter()
            .find(|span| span.content == "♚ ")
            .unwrap();
        let identity = rendered.lines[0]
            .spans
            .iter()
            .find(|span| span.content == "停车拾穗")
            .unwrap();
        assert_eq!(crown.style.fg, Some(app.config.palette.rank));
        assert_eq!(identity.style.fg, Some(app.config.palette.host));
    }

    #[tokio::test]
    pub(crate) async fn pin_command_keeps_the_selected_message_when_live_messages_arrive() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        let mut question = DanmuEvent::new(DanmuEventKind::Danmu, "需要重点回答的问题");
        question.username = Some("观众".into());
        let question_id = question.id.clone();
        session.ingest(question);
        let mut app = test_app(&temp, session);
        let (tx, _rx) = mpsc::channel(1);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT), tx.clone())
            .await
            .unwrap();
        for character in "/pin".chars() {
            app.handle_key(
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                tx.clone(),
            )
            .await
            .unwrap();
        }
        let mut incoming = DanmuEvent::new(DanmuEventKind::Danmu, "稍后到达的新消息");
        incoming.origin = DanmuEventOrigin::Live;
        app.handle_client_event(BilibiliClientEvent::Danmu(incoming));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(
            app.session.featured_event.as_ref().map(|event| &event.id),
            Some(&question_id)
        );
        assert!(app.input.is_empty());
    }

    #[tokio::test]
    pub(crate) async fn login_qr_consumes_keys_before_the_underlying_settings_panel() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.input = "人工草稿".into();
        app.open_settings().unwrap();
        let token = uuid::Uuid::new_v4();
        app.main_login = Some(MainLogin {
            token,
            task: tokio::spawn(std::future::pending()),
        });
        app.handle_ui_event(CoreEvent::LoginQr {
            token,
            url: "https://example.com/login".into(),
        });
        for code in [KeyCode::Enter, KeyCode::Char('x')] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(app.login_qr.is_none());
        assert!(app.assistant_panel.is_some());
        let notice = app.notice.clone();
        app.handle_ui_event(CoreEvent::LoginQr {
            token,
            url: "https://example.com/expired-login".into(),
        });
        app.handle_ui_event(CoreEvent::LoginDone {
            token,
            account: app.account.staged(),
            status: AccountStatus::SignedIn {
                display_name: "迟到账号".into(),
                user_id: "99".into(),
            },
        });
        assert!(app.login_qr.is_none());
        assert!(matches!(app.account_status, AccountStatus::SignedOut));
        assert_eq!(app.notice, notice);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.assistant_panel.is_none());
        assert_eq!(&*app.input, "人工草稿");
    }

    #[tokio::test]
    pub(crate) async fn escape_and_selection_behavior_remain_intact() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "问题");
        event.username = Some("观众".into());
        session.ingest(event);
        let mut app = test_app(&temp, session);
        let (tx, _rx) = mpsc::channel(1);

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(!app.selection_active);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT), tx.clone())
            .await
            .unwrap();
        assert!(app.selection_active);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.input.is_empty());
        let quit_selection = app
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(!quit_selection);
        assert!(!app.selection_active);
        for _ in 0..3 {
            let quit_app = app
                .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap();
            assert!(!quit_app);
        }
        assert!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), tx)
                .await
                .unwrap()
        );
    }
    #[test]
    pub(crate) fn successful_delivery_clears_only_transient_delivery_notices() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_ui_event(CoreEvent::DeliveryNotice("未确认送达".into()));
        assert!(app.notice_deadline.is_some());
        app.handle_ui_event(CoreEvent::DeliveryCompleted);
        assert!(app.notice.is_empty());
        app.handle_ui_event(CoreEvent::DeliveryNotice("再次未确认".into()));
        app.expire_notice_at(Instant::now() + DELIVERY_NOTICE_LIFETIME);
        assert!(app.notice.is_empty());
        app.handle_ui_event(CoreEvent::error("登录态需处理"));
        app.handle_ui_event(CoreEvent::DeliveryCompleted);
        assert_eq!(app.notice, "登录态需处理");
    }
    #[test]
    pub(crate) fn delivery_reminders_include_content_and_expire_once() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));

        app.handle_ui_event(CoreEvent::DeliveryRejected {
            content: "请手动重发这条消息".into(),
            message: "发送失败：还没有 B 站登录态".into(),
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Failed);
        assert!(app.notice.contains("请手动重发这条消息"));

        let rejection_deadline = app.notice_deadline.expect("发送失败提醒应自动消失");
        app.expire_notice_at(rejection_deadline);
        assert!(app.notice.is_empty());
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);

        let delivery = PendingDelivery::new(
            "待确认消息".into(),
            "拾穗数据".into(),
            "42".into(),
            Utc::now(),
        );
        let delivery_id = delivery.id.clone();
        let (confirmation, _receiver) = oneshot::channel();
        app.handle_ui_event(CoreEvent::DeliveryStarted {
            delivery,
            confirmation,
            registered: oneshot::channel().0,
        });
        app.handle_ui_event(CoreEvent::DeliveryHistory {
            events: Vec::new(),
            room_id: app.session.room_id.clone(),
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Verifying);
        app.handle_ui_event(CoreEvent::DeliveryTimedOut {
            delivery_ids: vec![delivery_id],
        });
        assert_eq!(app.delivery_status, DeliveryStatus::Uncertain);
        assert!(app.notice.contains("待确认消息"));

        let timeout_deadline = app.notice_deadline.expect("未确认提醒应自动消失");
        app.expire_notice_at(timeout_deadline);
        assert!(app.notice.is_empty());
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);
    }

    #[test]
    pub(crate) fn success_and_info_notices_and_confirmed_delivery_expire_after_three_seconds() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let now = Instant::now();
        for level in [NoticeLevel::Info, NoticeLevel::Success] {
            app.set_notice_at("已切换主题：chatroom", level, now);
            app.expire_notice_at(now + Duration::from_millis(2_999));
            assert_eq!(app.notice, "已切换主题：chatroom");
            app.expire_notice_at(now + Duration::from_secs(3));
            assert!(app.notice.is_empty());
        }
        app.set_delivery_status_at(DeliveryStatus::Delivered, now);
        app.expire_notice_at(now + Duration::from_millis(2_999));
        assert_eq!(app.delivery_status, DeliveryStatus::Delivered);
        app.expire_notice_at(now + Duration::from_secs(3));
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);
    }

    #[test]
    pub(crate) fn warning_and_error_notices_expire_at_the_delivery_notice_boundary() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let now = Instant::now();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        for level in [NoticeLevel::Warning, NoticeLevel::Error] {
            app.set_notice_at("限时异常提醒", level, now);
            app.expire_notice_at(now + DELIVERY_NOTICE_LIFETIME - Duration::from_nanos(1));
            assert_eq!(app.notice, "限时异常提醒");
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(format!("{:?}", terminal.backend().buffer()).contains("限时异常提醒"));
            app.expire_notice_at(now + DELIVERY_NOTICE_LIFETIME);
            assert!(app.notice.is_empty());
            assert!(app.notice_deadline.is_none());
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(!format!("{:?}", terminal.backend().buffer()).contains("限时异常提醒"));
        }
    }

    #[test]
    pub(crate) fn progress_notice_waits_for_completion_instead_of_expiring() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let now = Instant::now();
        app.set_notice_at("模型目录读取中", NoticeLevel::Progress, now);
        app.expire_notice_at(now + Duration::from_secs(600));
        assert_eq!(app.notice, "模型目录读取中");
        assert!(app.notice_deadline.is_none());
        app.set_notice_at("模型目录已读取", NoticeLevel::Success, now);
        app.expire_notice_at(now + Duration::from_secs(3));
        assert!(app.notice.is_empty());
    }

    #[test]
    pub(crate) fn overflowing_live_feed_keeps_newest_event_visible() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..30 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("历史消息-{index:02}"));
            event.username = Some(format!("观众-{index:02}"));
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
        let mut latest = DanmuEvent::new(DanmuEventKind::Danmu, "最新实时弹幕");
        latest.origin = DanmuEventOrigin::Live;
        latest.username = Some("新观众".into());
        app.handle_client_event(BilibiliClientEvent::Danmu(latest));

        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let palette = app.config.palette;
        terminal
            .draw(|frame| draw_events(frame, frame.area(), &mut app, palette, "Events"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace(' ', "");

        assert!(rendered.contains("最新实时弹幕"), "{rendered}");
        assert!(!rendered.contains("历史消息-00"), "{rendered}");
    }

    #[test]
    pub(crate) fn periodic_history_recovery_restores_a_message_missed_by_websocket() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let now = Utc::now();
        let mut session = DanmuSession::new("1");
        let mut previous = DanmuEvent::new(DanmuEventKind::Danmu, "上一条消息");
        previous.id = "previous".into();
        previous.timestamp = now - chrono::Duration::seconds(10);
        session.ingest(previous);
        let mut app = test_app(&temp, session);
        let mut recovered = DanmuEvent::new(DanmuEventKind::Danmu, "做点小玩意");
        recovered.id = "bili-recovered".into();
        recovered.platform_event_id = Some("recovered".into());
        recovered.origin = DanmuEventOrigin::History;
        recovered.timestamp = now;
        recovered.username = Some("停车拾穗".into());

        app.handle_client_event(BilibiliClientEvent::Danmu(recovered.clone()));
        app.handle_client_event(BilibiliClientEvent::Danmu(recovered));

        assert_eq!(app.session.recent_events[0].content, "做点小玩意");
        assert_eq!(app.session.recent_events.len(), 2);
    }

    #[test]
    pub(crate) fn incoming_live_event_preserves_the_message_being_browsed() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..10 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("消息-{index}"));
            event.id = format!("event-{index}");
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
        app.selection_active = true;
        app.selected = 5;
        let selected_id = app.session.recent_events[app.selected].id.clone();

        let mut latest = DanmuEvent::new(DanmuEventKind::Danmu, "最新实时弹幕");
        latest.origin = DanmuEventOrigin::Live;
        latest.id = "live-latest".into();
        app.handle_client_event(BilibiliClientEvent::Danmu(latest));

        assert_eq!(app.session.recent_events[app.selected].id, selected_id);
    }

    #[tokio::test]
    pub(crate) async fn history_idle_timeout_tracks_input_not_incoming_messages() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..8 {
            session.ingest(DanmuEvent::new(
                DanmuEventKind::Danmu,
                format!("历史-{index}"),
            ));
        }
        let mut app = test_app(&temp, session);
        let (tx, _rx) = mpsc::channel(1);
        app.config.history_idle_seconds = 60;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        // A fresh input must restart a previously expired idle period.
        app.last_user_activity = Instant::now() - Duration::from_secs(120);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        let input_at = app.last_user_activity;
        let anchor = app.session.recent_events[app.scroll_offset].id.clone();
        app.advance_history_at(input_at + Duration::from_secs(59));
        assert_eq!(app.session.recent_events[app.scroll_offset].id, anchor);
        assert!(app.is_browsing_history());
        let mut latest = DanmuEvent::new(DanmuEventKind::Danmu, "超时后应该看到的最新弹幕");
        latest.origin = DanmuEventOrigin::Live;
        app.handle_client_event(BilibiliClientEvent::Danmu(latest));
        assert_eq!(app.unread_live_count, 1);
        assert_eq!(app.session.recent_events[app.scroll_offset].id, anchor);
        app.advance_history_at(input_at + Duration::from_secs(60));
        assert!(!app.is_browsing_history());
        assert_eq!(app.unread_live_count, 0);
        assert_eq!(&*app.input, "x");

        app.config.history_idle_seconds = 0;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        app.advance_history_at(app.last_user_activity + Duration::from_secs(3600));
        assert!(app.is_browsing_history());
        assert!(
            !app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
                .await
                .unwrap()
        );
        assert!(!app.is_browsing_history());
        assert_eq!(&*app.input, "x");
    }

    #[test]
    pub(crate) fn history_idle_timeout_waits_for_modal_operation_to_finish() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        session.ingest(DanmuEvent::new(DanmuEventKind::Danmu, "正在准备回复的问题"));
        let mut app = test_app(&temp, session);
        app.selection_active = true;
        app.config.history_idle_seconds = 60;
        app.stop_flow = Some(StopFlow::Confirm {
            stop_selected: false,
        });
        let now = app.last_user_activity + Duration::from_secs(120);
        app.advance_history_at(now);
        assert!(app.selection_active);
        app.cancel_stop();
        app.advance_history_at(now + Duration::from_secs(59));
        assert!(app.selection_active);
        app.advance_history_at(now + Duration::from_secs(60));
        assert!(!app.is_browsing_history());
    }

    #[tokio::test]
    pub(crate) async fn end_returns_from_history_browsing_to_live_feed() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.selection_active = true;
        app.selected = 7;
        app.unread_live_count = 3;
        let (tx, _rx) = mpsc::channel(1);

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), tx)
            .await
            .unwrap();

        assert!(!app.selection_active);
        assert_eq!(app.selected, 0);
        assert_eq!(app.unread_live_count, 0);
    }

    #[tokio::test]
    pub(crate) async fn chinese_archive_search_completes_without_execution() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.input = "/搜索".into();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(app.input.trim(), "/find");
    }

    #[tokio::test]
    pub(crate) async fn paste_stays_in_draft_and_cannot_cross_modal_confirmation() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_paste("/quit\r\n/obs start\t\u{3}\u{10}");
        assert_eq!(app.input, "/quit /obs start ");
        assert!(!app.quit_requested);
        assert!(app.stop_flow.is_none());
        assert!(app.pending_deliveries.is_empty());

        app.input = "保留人工草稿".into();
        app.stop_flow = Some(StopFlow::Confirm {
            stop_selected: true,
        });
        app.handle_paste("\r\n/quit\u{1b}");
        assert!(matches!(
            app.stop_flow,
            Some(StopFlow::Confirm {
                stop_selected: true
            })
        ));
        assert_eq!(app.input, "保留人工草稿");
        app.stop_flow = None;
        app.login_qr = Some(vec!["QR".into()]);
        app.handle_paste("/quit\r");
        assert_eq!(app.input, "保留人工草稿");
        assert!(app.login_qr.is_some());
        app.login_qr = None;
        app.selection_active = true;
        app.handle_paste("不应编辑回复对象下的草稿\n");
        assert_eq!(app.input, "保留人工草稿");
    }

    #[tokio::test]
    pub(crate) async fn secret_paste_stays_hidden_and_bypasses_slash_completion() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(1);
        app.secret_target = Some(SecretTarget::ObsPassword);
        app.handle_paste("/obs config password\r\nsecret-marker\u{3}");
        let secret = app.input.to_string();
        assert_eq!(secret, "/obs config password secret-marker");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert_eq!(app.input, secret.as_str());
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let visible = format!("{:?}", terminal.backend().buffer());
        assert!(!visible.contains("secret-marker"));
        assert!(!visible.contains("/obs config password"));
        assert!(!app.notice.contains("secret-marker"));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.secret_target.is_none());
        assert!(app.input.is_empty());
        assert!(app.session.recent_events.is_empty());
    }

    #[tokio::test]
    pub(crate) async fn command_search_restores_cursor_and_never_sends_query_text() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, mut rx) = mpsc::channel(8);
        app.input = "草稿甲🙂乙".into();
        app.input.set_cursor(3);
        app.selection_active = true;
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_paste("没有匹配的中文搜索");
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.command_search_draft.is_some());
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);
        assert!(rx.try_recv().is_err());
        app.handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(!app.runner.running);
        assert!(!app.bridge.sending_enabled());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert_eq!(app.input, "草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
        assert!(app.selection_active);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_paste("/ai materials");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.assistant_panel.is_some());
        assert!(app.command_search_draft.is_none());
        assert_eq!(app.input, "草稿甲🙂乙");
        assert_eq!(app.input.cursor(), 3);
        assert_eq!(app.delivery_status, DeliveryStatus::Idle);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    pub(crate) async fn command_search_obs_submenu_and_password_preserve_the_original_draft() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(8);
        app.input = "未完成的人工回复".into();
        app.input.set_cursor(2);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.input.replace("/obs".into());
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert_eq!(app.input, "/obs ");
        assert!(app.command_search_draft.is_some());
        app.input.replace("/obs config password".into());
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(app.secret_target.is_some());
        assert!(app.input.is_empty());
        app.handle_paste("secret");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(app.input, "未完成的人工回复");
        assert_eq!(app.input.cursor(), 2);
        assert!(app.secret_target.is_none());
    }

    #[test]
    pub(crate) fn optional_ai_commands_stay_out_of_the_idle_palette() {
        let idle = command_suggestions("", false);
        assert!(idle.iter().any(|spec| spec.completion == "/settings"));
        assert!(idle.iter().any(|spec| spec.completion == "/diag"));
        assert!(!idle.iter().any(|spec| ai_command(spec)));

        let explicit = command_suggestions("/ai", false);
        assert!(explicit.iter().any(|spec| spec.completion == "/ai"));

        let active = command_suggestions("", true);
        assert!(active.iter().any(|spec| spec.completion == "/review"));
        assert!(!active.iter().any(|spec| spec.completion.contains(' ')));
        assert!(
            command_suggestions("模型", false)
                .iter()
                .any(|spec| spec.completion == "/ai model")
        );
        assert!(
            command_suggestions("静音", false)
                .iter()
                .any(|spec| spec.completion == "/mute")
        );
    }

    #[test]
    pub(crate) fn every_builtin_theme_reaches_the_rendered_terminal_surface() {
        let temp = crate::test_support::private_tempdir().unwrap();
        for theme_name in ["shisui", "catppuccin-mocha", "tokyo-night", "gruvbox-dark"] {
            let mut app = test_app(&temp, DanmuSession::new("1"));
            let (_, palette) = app.config.themes.resolve(theme_name).unwrap();
            app.config.theme_name = theme_name.into();
            app.config.palette = palette;
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

            terminal.draw(|frame| draw(frame, &mut app)).unwrap();

            assert_eq!(
                terminal.backend().buffer()[(40, 10)].bg,
                palette.background,
                "主题 {theme_name} 的背景色没有进入终端缓冲区"
            );
        }
    }
    #[test]
    pub(crate) fn obs_poll_failure_clears_stale_status_without_polluting_room_strip() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_ui_event(CoreEvent::ObsStatus(Ok(ObsStatus {
            current_scene: "直播".into(),
            stream: crate::obs::StreamState::Live,
            microphone: crate::obs::MicrophoneState::Muted,
            compatibility_warning: None,
        })));
        assert!(app.obs_status.is_some());
        assert!(app.obs_error.is_none());
        assert_eq!(app.runner.microphone_context()["state"], "muted");

        app.handle_ui_event(CoreEvent::ObsStatus(Err("OBS WebSocket 连接失败".into())));
        assert!(app.obs_status.is_none());
        assert_eq!(app.obs_error.as_deref(), Some("OBS WebSocket 连接失败"));
        assert!(app.obs_checked_at.is_some());
        let disconnected = app.runner.microphone_context();
        assert_eq!(disconnected["state"], "unknown");
        assert_eq!(disconnected["freshness"], "unavailable");
        assert!(disconnected["age_ms"].is_null());

        let mut terminal = Terminal::new(TestBackend::new(120, 2)).unwrap();
        let palette = app.config.palette;
        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, palette))
            .unwrap();
        let rendered = (0..terminal.backend().buffer().area.width)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>()
            .replace(' ', "");

        assert!(!rendered.contains("OBS"), "{rendered}");

        app.handle_ui_event(CoreEvent::ObsStatus(Ok(ObsStatus {
            current_scene: "直播".into(),
            stream: crate::obs::StreamState::Live,
            microphone: MicrophoneState::Unmuted,
            compatibility_warning: None,
        })));
        assert_eq!(app.runner.microphone_context()["state"], "unmuted");
        assert_eq!(
            app.runner.microphone_context()["speech_activity"],
            "unknown"
        );
    }

    #[test]
    pub(crate) fn long_input_wraps_inside_the_input_frame() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.input = format!("{}TAIL", "a".repeat(35)).into();
        assert_eq!(
            wrapped_cursor_position(&app.input, app.input.cursor(), 35),
            (1, 4)
        );
        assert_eq!(
            wrapped_input_lines("甲乙丙丁", 6),
            vec!["甲乙丙".to_string(), "丁".to_string()]
        );
        let palette = app.config.palette;
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();

        terminal
            .draw(|frame| draw_input(frame, frame.area(), &app, palette))
            .unwrap();

        let final_input_row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 2)].symbol())
            .collect::<String>();
        assert_eq!(final_input_row, format!("╰─ TAIL{}─╯", " ".repeat(31)));
    }

    #[test]
    pub(crate) fn official_custom_emote_metadata_is_visible_in_text_terminals() {
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, "你好[主播表情]");
        event.emotes.push(crate::domain::DanmuEmote {
            text: "[主播表情]".into(),
            fallback: "🙂".into(),
            image_url: url::Url::parse("https://example.com/emote.png").unwrap(),
            width: Some(48),
            height: Some(48),
            is_animated: false,
        });

        assert_eq!(map_event_emotes(&event), "你好🙂");
        assert_eq!(map_bili_emotes("[花] [委屈]"), "🌸 🥺");
    }

    #[tokio::test]
    pub(crate) async fn mouse_and_arrow_history_navigation_preserve_the_draft() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut session = DanmuSession::new("1");
        for index in 0..8 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("消息 {index}"));
            event.id = index.to_string();
            session.ingest(event);
        }
        let mut app = test_app(&temp, session);
        let wheel_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let wheel_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..wheel_up
        };

        app.handle_mouse(wheel_up);
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 6"
        );
        assert!(!app.selection_active);
        app.handle_mouse(wheel_down);
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.selection_active);

        app.selection_active = true;
        app.selected = 3;
        app.handle_mouse(wheel_up);
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 3"
        );
        assert!(!app.selection_active);
        let mut arrival = DanmuEvent::new(DanmuEventKind::Danmu, "新到达的消息");
        arrival.timestamp = Utc::now() + chrono::Duration::seconds(1);
        app.ingest_event(arrival);
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 3"
        );
        assert_eq!(app.unread_live_count, 1);

        for _ in 0..20 {
            app.handle_mouse(wheel_up);
        }
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 0"
        );
        for _ in 0..20 {
            app.handle_mouse(wheel_down);
        }
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.unread_live_count, 0);
        app.input = "草稿🙂保留".into();
        app.input.set_cursor(2);
        let (tx, _rx) = mpsc::channel(1);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert_eq!(
            app.session.recent_events[app.scroll_offset].content,
            "消息 7"
        );
        assert!(!app.selection_active);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.input, "草稿🙂保留");
        assert_eq!(app.input.cursor(), 2);
    }

    #[test]
    pub(crate) fn failed_online_refresh_preserves_the_last_successful_value() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.handle_ui_event(CoreEvent::OnlineViewers(Ok(Some(11))));
        app.handle_ui_event(CoreEvent::OnlineViewers(Err("暂时失败".into())));

        assert_eq!(app.online_viewers, Some(11));
        assert!(app.notice.contains("继续显示上次成功值"));
    }

    #[tokio::test]
    pub(crate) async fn keyboard_palette_restores_unicode_draft_and_direct_page_closes_once() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, mut rx) = mpsc::channel(8);
        app.input = "甲🙂乙".into();
        app.input.set_cursor(1);
        app.handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            tx.clone(),
        )
        .await
        .unwrap();
        app.handle_paste("模型");
        let index = app
            .command_suggestions()
            .iter()
            .position(|s| s.completion == "/ai model")
            .unwrap();
        for _ in 0..index {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), tx.clone())
                .await
                .unwrap();
        }
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert_eq!(app.input, "甲🙂乙");
        assert_eq!(app.input.cursor(), 1);
        assert!(app.assistant_panel.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(app.assistant_panel.is_none());
        app.command("/ai model", tx.clone()).await.unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        assert!(app.assistant_panel.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    pub(crate) async fn palette_opens_optional_argument_editor_and_preserves_unicode_parameters() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.open_commands();
        let (tx, _rx) = mpsc::channel(1);
        app.input.replace("/scene".into());
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        assert!(
            app.assistant_panel.is_none(),
            "Tab must not activate the editor"
        );
        app.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        )
        .await
        .unwrap();
        assert!(
            app.assistant_panel
                .as_ref()
                .is_some_and(assistant::Panel::is_editing)
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx.clone())
            .await
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tx)
            .await
            .unwrap();
        app.open_commands();
        app.input.replace("/find 档案 甲".into());
        // Exact typed arguments use the existing raw-command route, not completion replacement.
        assert!(
            matches!(app.handle_slash_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), SlashKeyAction::Submit(raw) if raw == "/find 档案 甲")
        );
    }

    #[tokio::test]
    pub(crate) async fn slash_obs_reports_progress_and_quit_exits() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        let (tx, _rx) = mpsc::channel(4);

        app.command("/obs", tx.clone()).await.unwrap();
        assert!(matches!(app.notice_level, NoticeLevel::Progress));
        app.command("/quit", tx).await.unwrap();
        assert!(app.quit_requested);
    }

    #[tokio::test]
    pub(crate) async fn explicit_display_setting_persists_even_when_runtime_override_already_matches()
     {
        let temp = crate::test_support::private_tempdir().unwrap();
        let mut app = test_app(&temp, DanmuSession::new("1"));
        app.config.save_value("show_time", true.into()).unwrap();
        app.show_time = false;
        let (tx, _rx) = mpsc::channel(1);
        app.command("/display time off", tx).await.unwrap();
        let saved: toml::Value =
            toml::from_str(&std::fs::read_to_string(&app.config.config_path).unwrap()).unwrap();
        assert_eq!(saved["show_time"].as_bool(), Some(false));
    }
}
