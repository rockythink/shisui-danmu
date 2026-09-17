use crate::theme::{Palette, ThemeCatalog};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "danmu",
    version,
    about = "面向知识型主播的 B 站弹幕与直播监控工作台",
    after_help = "常用：danmu 123456　进入直播间\n      danmu --login　登录弹幕台主账号（与浏览器登录分开）\n\n进入后输入 /settings 修改长期偏好，/help 查看操作。开关值 true 表示开启，false 表示关闭。显示与阅读参数仅覆盖本次启动；登录与 OBS 向导会保存配置。"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// 私有本地实例目录；Agent/MCP连接此处的现用实例
    #[arg(long, global = true)]
    pub instance: Option<PathBuf>,
    /// 无网络回放JSON数组；不读取账号/OBS/默认配置，不安装或连接直播
    #[arg(long, value_name = "JSON路径")]
    pub replay: Option<PathBuf>,
    /// 要查看的直播间房间号，例如 danmu 123456
    #[arg(value_name = "房间号")]
    pub positional_room: Option<String>,
    /// 指定房间号，优先于位置参数和已保存配置
    #[arg(short, long, value_name = "房间号")]
    pub room: Option<String>,
    /// 列表元信息与正文同行；长正文仍可换行（本次有效）
    #[arg(short = 'l', long, value_parser = clap::builder::BoolishValueParser::new(), value_name = "true|false")]
    pub single_line: Option<bool>,
    /// 显示每条弹幕的时间；建议开启以区分旧消息（本次有效）
    #[arg(short = 's', long, value_parser = clap::builder::BoolishValueParser::new(), value_name = "true|false")]
    pub show_time: Option<bool>,
    /// 显示发言者昵称；建议开启以辨认对话对象（本次有效）
    #[arg(long, value_parser = clap::builder::BoolishValueParser::new(), value_name = "true|false")]
    pub show_name: Option<bool>,
    /// 本次隐藏昵称，优先于 --show-name
    #[arg(long)]
    pub hide_name: bool,
    /// 浏览历史空闲多少秒后返回实时；0 表示仅手动返回
    #[arg(long, value_name = "秒数")]
    pub history_idle_seconds: Option<u32>,
    /// 使用指定的 TOML 配置文件
    #[arg(short, long, value_name = "路径")]
    pub config: Option<PathBuf>,
    /// 本次使用的主题；长期主题请在 /settings 选择
    #[arg(long, value_name = "主题名")]
    pub theme: Option<String>,
    /// 扫码登录并保存弹幕台主账号，不修改浏览器登录
    #[arg(long)]
    pub login: bool,
    /// 退出弹幕台主账号；不退出浏览器账号
    #[arg(long)]
    pub logout: bool,
    /// 交互配置 OBS 连接与麦克风；密码隐藏输入并单独保存
    #[arg(long)]
    pub configure_obs: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 显式准备私有适配器；不启动助手、不修改原生配置或登录
    Setup {
        #[command(subcommand)]
        adapter: SetupAdapter,
    },
    /// 无房间隔离终端，仅人工输入及LocalTransport；必须指定新建私有实例目录
    Local {
        /// 启动即打开助手设置面板；不启动模型或开启发送
        #[arg(long)]
        assistant: bool,
    },
    /// 连接已运行实例，执行status/messages/report/reply/result；参数为JSON对象
    Agent {
        operation: String,
        #[arg(default_value = "{}")]
        json: String,
    },
    /// stdio MCP；不启动TUI，不登录、不读取平台凭据
    Mcp,
}

#[derive(Debug, Subcommand)]
pub enum SetupAdapter {
    /// 私有安装已核定的 pi-acp@0.0.33；需要 Node.js 20+ 与 npm
    Pi,
}

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub config_path: PathBuf,
    pub instance: Option<PathBuf>,
    pub history_idle_seconds: u32,
    pub room_id: String,
    pub single_line: bool,
    pub chat_layout: bool,
    pub show_time: bool,
    pub show_name: bool,
    pub palette: Palette,
    pub theme_name: String,
    pub themes: ThemeCatalog,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    history_idle_seconds: Option<u32>,
    #[serde(alias = "roomID", alias = "roomid")]
    room_id: Option<String>,
    #[serde(alias = "singleLine", alias = "singleline")]
    single_line: Option<bool>,
    #[serde(alias = "chatLayout", alias = "chatlayout")]
    chat_layout: Option<bool>,
    #[serde(alias = "showTime", alias = "showtime")]
    show_time: Option<bool>,
    #[serde(alias = "showName", alias = "showname")]
    show_name: Option<bool>,
    #[serde(alias = "hideName", alias = "hidename")]
    hide_name: Option<bool>,
}

impl TerminalConfig {
    pub fn load(cli: &Cli, default_path: PathBuf, themes_path: PathBuf) -> Result<Self> {
        let path = cli.config.clone().unwrap_or(default_path);
        let file: ConfigFile = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("配置文件格式错误：{}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigFile::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("读取配置文件失败：{}", path.display()));
            }
        };
        let room_id = cli
            .room
            .clone()
            .or(cli.positional_room.clone())
            .or(file.room_id)
            .unwrap_or_default()
            .trim()
            .to_string();
        if room_id.parse::<u64>().is_err() || room_id == "0" {
            bail!("缺少有效房间号；运行 danmu --help 查看用法");
        }
        let themes = ThemeCatalog::load(themes_path)?;
        let requested_theme = cli.theme.as_deref().unwrap_or(themes.selected());
        let (theme_name, palette) = themes.resolve(requested_theme)?;

        Ok(Self {
            config_path: path,
            instance: cli.instance.clone(),
            history_idle_seconds: cli
                .history_idle_seconds
                .or(file.history_idle_seconds)
                .unwrap_or(0),
            room_id,
            single_line: cli.single_line.or(file.single_line).unwrap_or(true),
            chat_layout: file.chat_layout.unwrap_or(false),
            show_time: cli.show_time.or(file.show_time).unwrap_or(true),
            show_name: if cli.hide_name {
                false
            } else {
                cli.show_name
                    .or(file.show_name)
                    .unwrap_or(!file.hide_name.unwrap_or(false))
            },
            palette,
            theme_name,
            themes,
        })
    }
    pub(crate) fn save_value(&self, key: &str, mut value: toml_edit::Value) -> Result<()> {
        use std::io::Write;
        let text = match std::fs::read_to_string(&self.config_path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).context("读取TUI配置失败"),
        };
        let _: ConfigFile = toml::from_str(&text).context("TUI配置无效，未覆盖原文件")?;
        let mut document = text.parse::<toml_edit::DocumentMut>()?;
        document.remove("assistant_card");
        let aliases: &[&str] = match key {
            "chat_layout" => &["chatLayout", "chatlayout"],
            "show_name" => &["showName", "showname"],
            "show_time" => &["showTime", "showtime"],
            _ => &[],
        };
        let target = if document.contains_key(key) {
            key
        } else {
            aliases
                .iter()
                .copied()
                .find(|alias| document.contains_key(alias))
                .unwrap_or(key)
        };
        if let Some(previous) = document.get(target).and_then(toml_edit::Item::as_value) {
            *value.decor_mut() = previous.decor().clone();
        }
        document[target] = toml_edit::Item::Value(value);
        let parent = self.config_path.parent().context("TUI配置缺少父目录")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(document.to_string().as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&self.config_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn invalid_switch_spelling_is_not_silently_enabled() {
        let cli = Cli::try_parse_from([
            "danmu",
            "123",
            "--show-name",
            "off",
            "--show-time",
            "yes",
            "--single-line",
            "0",
        ])
        .unwrap();
        assert_eq!(
            (cli.show_name, cli.show_time, cli.single_line),
            (Some(false), Some(true), Some(false))
        );
        let error = Cli::try_parse_from(["danmu", "123", "--show-name", "flase"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn creates_and_uses_the_default_theme_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let themes_path = temp.path().join("themes.json");
        let cli = Cli::try_parse_from(["danmu", "123"]).unwrap();
        let config =
            TerminalConfig::load(&cli, temp.path().join("config.toml"), themes_path.clone())
                .unwrap();

        assert_eq!(config.room_id, "123");
        assert_eq!(config.theme_name, "shisui");
        assert_eq!(config.palette, Palette::default());
        assert!(themes_path.exists());
        assert!(Cli::try_parse_from(["danmu", "123", "--time-color", "#112233"]).is_err());
    }

    #[test]
    fn command_line_theme_overrides_the_json_selection() {
        let temp = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from(["danmu", "123", "--theme", "tokyo-night"]).unwrap();
        let config = TerminalConfig::load(
            &cli,
            temp.path().join("config.toml"),
            temp.path().join("themes.json"),
        )
        .unwrap();

        assert_eq!(config.theme_name, "tokyo-night");
        assert_eq!(
            config.palette.background,
            ratatui::style::Color::Rgb(26, 27, 38)
        );
    }

    #[test]
    fn loads_layout_keys_but_keeps_theme_selection_in_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(
            &path,
            "roomid = '456'
singleline = false
showname = false
theme = 'light'
timecolor = '#112233'
",
        )
        .unwrap();
        let cli = Cli::try_parse_from(["danmu"]).unwrap();
        let config = TerminalConfig::load(&cli, path, temp.path().join("themes.json")).unwrap();

        assert_eq!(config.room_id, "456");
        assert!(!config.single_line);
        assert!(!config.show_name);
        assert_eq!(config.theme_name, "shisui");
        assert_eq!(config.palette, Palette::default());
    }

    #[test]
    fn ui_preferences_reopen_without_losing_user_comments_or_unknown_sections() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(
            &path,
            "# user notes\nroomid = '456'\nshowname = true # keep this\n[custom]\nvalue = 'mine'\n",
        )
        .unwrap();
        let cli = Cli::try_parse_from(["danmu"]).unwrap();
        let themes = root.path().join("themes.json");
        let config = TerminalConfig::load(&cli, path.clone(), themes.clone()).unwrap();
        for (key, value) in [
            ("show_name", false),
            ("show_time", false),
            ("chat_layout", true),
        ] {
            config.save_value(key, value.into()).unwrap();
        }
        config
            .save_value("history_idle_seconds", 123_i64.into())
            .unwrap();
        let reopened = TerminalConfig::load(&cli, path.clone(), themes).unwrap();
        assert!(!reopened.show_name && !reopened.show_time && reopened.chat_layout);
        assert_eq!(reopened.history_idle_seconds, 123);
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# user notes") && saved.contains("# keep this"));
        assert_eq!(
            toml::from_str::<toml::Value>(&saved).unwrap()["custom"]["value"].as_str(),
            Some("mine")
        );
        std::fs::write(&path, "room_id = [broken").unwrap();
        assert!(config.save_value("show_name", true.into()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "room_id = [broken");
    }
}
