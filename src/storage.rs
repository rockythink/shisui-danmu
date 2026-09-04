use anyhow::{Context, Result};
use directories::BaseDirs;
#[cfg(not(target_os = "macos"))]
use directories::ProjectDirs;
use std::path::PathBuf;

pub const NAMESPACE: &str = "cc.ss-data.ShisuiDanmuTerminal";

#[derive(Debug, Clone)]
pub struct StoragePaths {
    pub support_dir: PathBuf,
    pub account_session: PathBuf,
    pub obs_configuration: PathBuf,
    pub sessions_dir: PathBuf,
    pub config_file: PathBuf,
    pub themes_file: PathBuf,
}

impl StoragePaths {
    pub fn discover() -> Result<Self> {
        let support_dir = support_directory()?;
        let config_dir = config_directory()?;
        let config_file = config_dir.join("config.toml");
        Ok(Self {
            account_session: support_dir.join("BilibiliAccount").join("session.json"),
            obs_configuration: support_dir.join("obs-control.json"),
            sessions_dir: support_dir.join("Sessions"),
            support_dir,
            config_file,
            themes_file: config_dir.join("themes.json"),
        })
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.support_dir).context("创建 TUI 数据目录失败")?;
        std::fs::create_dir_all(&self.sessions_dir).context("创建会话目录失败")?;
        if let Some(parent) = self.config_file.parent() {
            std::fs::create_dir_all(parent).context("创建配置目录失败")?;
        }
        Ok(())
    }
}

fn support_directory() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let base = BaseDirs::new().context("无法定位用户目录")?;
        Ok(base
            .home_dir()
            .join("Library/Application Support")
            .join(NAMESPACE))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let project = ProjectDirs::from("cc", "ss-data", "ShisuiDanmuTerminal")
            .context("无法定位应用数据目录")?;
        Ok(project.data_dir().to_path_buf())
    }
}

fn config_directory() -> Result<PathBuf> {
    let base = BaseDirs::new().context("无法定位用户配置目录")?;
    Ok(base.config_dir().join("shisui-danmu"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_terminal_namespace() {
        let paths = StoragePaths::discover().unwrap();
        #[cfg(target_os = "macos")]
        assert!(paths.support_dir.to_string_lossy().contains(NAMESPACE));
        #[cfg(target_os = "linux")]
        assert!(paths.support_dir.ends_with("shisuidanmuterminal"));
        #[cfg(target_os = "windows")]
        assert!(
            paths
                .support_dir
                .to_string_lossy()
                .contains("ShisuiDanmuTerminal")
        );
        assert!(
            paths
                .account_session
                .ends_with("BilibiliAccount/session.json")
        );
        assert!(paths.themes_file.ends_with("shisui-danmu/themes.json"));
    }
}
