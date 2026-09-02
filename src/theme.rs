use anyhow::{Context, Result, anyhow, bail};
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

const DEFAULT_THEMES_JSON: &str = include_str!("../assets/themes.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub time: Color,
    pub name: Color,
    pub content: Color,
    pub frame: Color,
    pub info: Color,
    pub rank: Color,
    pub background: Color,
    pub success: Color,
    pub host: Color,
    pub warning: Color,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            time: Color::Rgb(148, 163, 184),
            name: Color::Rgb(94, 234, 212),
            content: Color::Rgb(248, 250, 252),
            frame: Color::Rgb(51, 65, 85),
            info: Color::Rgb(34, 211, 238),
            rank: Color::Rgb(250, 204, 21),
            background: Color::Rgb(7, 10, 15),
            success: Color::Rgb(74, 222, 128),
            host: Color::Rgb(244, 114, 182),
            warning: Color::Rgb(251, 113, 133),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThemeCatalog {
    path: PathBuf,
    file: ThemeFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    selected: String,
    themes: BTreeMap<String, ThemeDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeDefinition {
    label: String,
    colors: ThemeColors,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeColors {
    time: String,
    name: String,
    content: String,
    frame: String,
    info: String,
    rank: String,
    background: String,
    success: String,
    host: String,
    warning: String,
}

impl ThemeCatalog {
    pub fn load(path: PathBuf) -> Result<Self> {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("创建主题配置目录失败：{}", parent.display()))?;
                }
                std::fs::write(&path, DEFAULT_THEMES_JSON)
                    .with_context(|| format!("创建默认主题配置失败：{}", path.display()))?;
                DEFAULT_THEMES_JSON.to_owned()
            }
            Err(error) => {
                return Err(error).with_context(|| format!("读取主题配置失败：{}", path.display()));
            }
        };
        let mut file: ThemeFile = serde_json::from_str(&text)
            .with_context(|| format!("主题配置 JSON 格式错误：{}", path.display()))?;
        if file.themes.is_empty() {
            bail!("主题配置至少需要一个主题：{}", path.display());
        }
        for (id, theme) in &file.themes {
            if id.trim().is_empty() {
                bail!("主题 ID 不能为空：{}", path.display());
            }
            if id.chars().any(char::is_whitespace) {
                bail!("主题 ID 不能包含空格：{id}");
            }
            if theme.label.trim().is_empty() {
                bail!("主题 {id} 缺少显示名称");
            }
            theme
                .palette()
                .with_context(|| format!("主题 {id} 的配色无效"))?;
        }
        let selected = canonical_id(&file, &file.selected)
            .ok_or_else(|| anyhow!("找不到已选主题：{}", file.selected))?;
        file.selected = selected;
        Ok(Self { path, file })
    }

    pub fn selected(&self) -> &str {
        &self.file.selected
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn choices(&self) -> Vec<String> {
        self.file
            .themes
            .iter()
            .map(|(id, theme)| format!("{id}（{}）", theme.label))
            .collect()
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.file
            .themes
            .iter()
            .map(|(id, theme)| (id.as_str(), theme.label.as_str()))
    }

    pub fn resolve(&self, requested: &str) -> Result<(String, Palette)> {
        let id =
            canonical_id(&self.file, requested).ok_or_else(|| anyhow!("未知主题：{requested}"))?;
        let palette = self
            .file
            .themes
            .get(&id)
            .expect("已检查主题存在")
            .palette()
            .with_context(|| format!("主题 {id} 的配色无效"))?;
        Ok((id, palette))
    }

    pub fn select(&mut self, requested: &str) -> Result<(String, Palette)> {
        let (id, palette) = self.resolve(requested)?;
        let previous = std::mem::replace(&mut self.file.selected, id.clone());
        if let Err(error) = self.save() {
            self.file.selected = previous;
            return Err(error);
        }
        Ok((id, palette))
    }

    pub fn reload(&mut self) -> Result<(String, Palette)> {
        let catalog = Self::load(self.path.clone())?;
        let selected = catalog.selected().to_owned();
        let (_, palette) = catalog.resolve(&selected)?;
        *self = catalog;
        Ok((selected, palette))
    }

    fn save(&self) -> Result<()> {
        let data = serde_json::to_vec_pretty(&self.file).context("序列化主题配置失败")?;
        std::fs::write(&self.path, data)
            .with_context(|| format!("保存主题配置失败：{}", self.path.display()))
    }
}

impl ThemeDefinition {
    fn palette(&self) -> Result<Palette> {
        Ok(Palette {
            time: parse_color(&self.colors.time).context("time")?,
            name: parse_color(&self.colors.name).context("name")?,
            content: parse_color(&self.colors.content).context("content")?,
            frame: parse_color(&self.colors.frame).context("frame")?,
            info: parse_color(&self.colors.info).context("info")?,
            rank: parse_color(&self.colors.rank).context("rank")?,
            background: parse_color(&self.colors.background).context("background")?,
            success: parse_color(&self.colors.success).context("success")?,
            host: parse_color(&self.colors.host).context("host")?,
            warning: parse_color(&self.colors.warning).context("warning")?,
        })
    }
}

fn canonical_id(file: &ThemeFile, requested: &str) -> Option<String> {
    file.themes
        .keys()
        .find(|id| id.eq_ignore_ascii_case(requested.trim()))
        .cloned()
}

fn parse_color(value: &str) -> Result<Color> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("颜色必须是 #RRGGBB：{value}");
    }
    Ok(Color::Rgb(
        u8::from_str_radix(&hex[0..2], 16)?,
        u8::from_str_radix(&hex[2..4], 16)?,
        u8::from_str_radix(&hex[4..6], 16)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn creates_default_catalog_with_professional_dark_themes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("themes.json");
        let catalog = ThemeCatalog::load(path.clone()).unwrap();

        assert!(path.exists());
        assert_eq!(catalog.selected(), "shisui");
        assert_eq!(catalog.resolve("SHISUI").unwrap().1, Palette::default());
        assert_eq!(catalog.choices().len(), 4);
        assert!(
            catalog
                .choices()
                .iter()
                .any(|name| name.contains("Catppuccin"))
        );
        assert!(
            catalog
                .choices()
                .iter()
                .any(|name| name.contains("Tokyo Night"))
        );
        assert!(
            catalog
                .choices()
                .iter()
                .any(|name| name.contains("Gruvbox"))
        );
    }

    #[test]
    fn loads_a_user_defined_theme_from_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("themes.json");
        let custom = json!({
            "selected": "studio",
            "themes": {
                "studio": {
                    "label": "演播室",
                    "colors": {
                        "time": "#112233",
                        "name": "#223344",
                        "content": "#F1F2F3",
                        "frame": "#334455",
                        "info": "#445566",
                        "rank": "#DDAA22",
                        "background": "#010203",
                        "success": "#55CC88",
                        "host": "#CC66AA",
                        "warning": "#FF6677"
                    }
                }
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&custom).unwrap()).unwrap();

        let catalog = ThemeCatalog::load(path).unwrap();
        let (id, palette) = catalog.resolve("studio").unwrap();
        assert_eq!(id, "studio");
        assert_eq!(palette.background, Color::Rgb(1, 2, 3));
        assert_eq!(palette.content, Color::Rgb(241, 242, 243));
    }

    #[test]
    fn rejects_invalid_custom_colors_with_theme_and_role_context() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("themes.json");
        let mut custom: serde_json::Value = serde_json::from_str(DEFAULT_THEMES_JSON).unwrap();
        custom["themes"]["shisui"]["colors"]["warning"] = json!("red");
        std::fs::write(&path, serde_json::to_vec_pretty(&custom).unwrap()).unwrap();

        let error = ThemeCatalog::load(path).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("主题 shisui"));
        assert!(message.contains("warning"));
        assert!(message.contains("#RRGGBB"));
    }

    #[test]
    fn selecting_a_theme_persists_for_the_next_launch() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("themes.json");
        let mut catalog = ThemeCatalog::load(path.clone()).unwrap();

        let (id, palette) = catalog.select("TOKYO-NIGHT").unwrap();
        assert_eq!(id, "tokyo-night");
        assert_eq!(palette.background, Color::Rgb(26, 27, 38));
        assert_eq!(ThemeCatalog::load(path).unwrap().selected(), "tokyo-night");
    }
    #[test]
    fn reload_applies_user_json_edits_without_restarting() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("themes.json");
        let mut catalog = ThemeCatalog::load(path.clone()).unwrap();
        let mut custom: serde_json::Value = serde_json::from_str(DEFAULT_THEMES_JSON).unwrap();
        custom["selected"] = json!("gruvbox-dark");
        custom["themes"]["gruvbox-dark"]["colors"]["info"] = json!("#123456");
        std::fs::write(&path, serde_json::to_vec_pretty(&custom).unwrap()).unwrap();

        let (id, palette) = catalog.reload().unwrap();

        assert_eq!(id, "gruvbox-dark");
        assert_eq!(palette.info, Color::Rgb(18, 52, 86));
    }
}
