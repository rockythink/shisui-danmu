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

/// Replace a private file only after the complete contents are durable. The temporary file
/// lives beside the destination, so rename is atomic and a failed write preserves the old file.
pub(crate) fn write_private_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("私有文件没有父目录")?;
    std::fs::create_dir_all(parent).context("创建私有文件目录失败")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("创建私有暂存文件失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(bytes).context("写入私有文件失败")?;
    temporary.as_file().sync_all().context("同步私有文件失败")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("原子替换私有文件失败")?;
    Ok(())
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
