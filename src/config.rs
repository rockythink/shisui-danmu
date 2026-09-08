use crate::theme::{Palette, ThemeCatalog};
use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "danmu",
    version,
    about = "面向知识型主播的 B 站弹幕与直播监控工作台"
)]
pub struct Cli {
    /// 无网络回放JSON数组；不读取账号/OBS/默认配置，不安装或连接直播
    #[arg(long, value_name = "JSON路径")]
    pub replay: Option<PathBuf>,
    #[arg(value_name = "房间号")]
    pub positional_room: Option<String>,
    #[arg(short, long, value_name = "房间号")]
    pub room: Option<String>,
    #[arg(short = 'l', long, value_parser = parse_bool)]
    pub single_line: Option<bool>,
    #[arg(short = 's', long, value_parser = parse_bool)]
    pub show_time: Option<bool>,
    #[arg(long, value_parser = parse_bool)]
    pub show_name: Option<bool>,
    #[arg(long)]
    pub hide_name: bool,
    /// 浏览历史空闲多少秒后返回实时；0 表示仅手动返回
    #[arg(long, value_name = "秒数")]
    pub history_idle_seconds: Option<u32>,
    #[arg(short, long, value_name = "路径")]
    pub config: Option<PathBuf>,
    #[arg(long, value_name = "主题名")]
    pub theme: Option<String>,
    #[arg(long)]
    pub login: bool,
    #[arg(long)]
    pub logout: bool,
    #[arg(long)]
    pub configure_obs: bool,
}

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub config_path: PathBuf,
    pub autoreply: crate::autoreply::Config,
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
    #[serde(default)]
    autoreply: crate::autoreply::Config,
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
        let codex_home = default_path
            .parent()
            .context("应用配置目录无效")?
            .join("codex-subscription");
        let path = cli.config.clone().unwrap_or(default_path);
        let mut file: ConfigFile = match std::fs::read_to_string(&path) {
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
        file.autoreply.codex.home = codex_home;
        Ok(Self {
            config_path: path,
            autoreply: file.autoreply,
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
    pub fn save_autoreply(&self, config: &crate::autoreply::Config) -> Result<()> {
        let mut document: toml::Value = match std::fs::read_to_string(&self.config_path) {
            Ok(text) => toml::from_str(&text).context("配置格式错误；未覆盖")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                toml::Value::Table(Default::default())
            }
            Err(e) => return Err(e.into()),
        };
        document
            .as_table_mut()
            .context("配置根节点必须为表")?
            .insert("autoreply".into(), toml::Value::try_from(config)?);
        let parent = self.config_path.parent().context("配置目录无效")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut file, toml::to_string_pretty(&document)?.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&self.config_path)?;
        Ok(())
    }
}

fn parse_bool(value: &str) -> std::result::Result<bool, String> {
    Ok(!matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
}
