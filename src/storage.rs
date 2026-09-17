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
    #[cfg(windows)]
    persist_atomic_windows(temporary, path).context("原子替换私有文件失败")?;
    #[cfg(not(windows))]
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("原子替换私有文件失败")?;
    Ok(())
}

#[cfg(windows)]
fn persist_atomic_windows(
    mut temporary: tempfile::NamedTempFile,
    path: &std::path::Path,
) -> std::io::Result<()> {
    use std::{
        fs::OpenOptions,
        io,
        mem::{size_of, size_of_val},
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_NORMAL, FILE_BASIC_INFO, FILE_RENAME_INFO, FILE_WRITE_ATTRIBUTES,
        FileBasicInfo, FileRenameInfoEx, SetFileInformationByHandle,
    };

    // Win32 resolves relative names against the process cwd, which may be on another drive.
    let absolute = std::path::absolute(path)?;
    let invalid_name = || io::Error::new(io::ErrorKind::InvalidInput, "无效原子替换文件名");
    let mut name: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(invalid_name());
    }
    let name_bytes = u32::try_from(size_of_val(name.as_slice())).map_err(|_| invalid_name())?;
    name.push(0);
    let buffer_size = size_of::<FILE_RENAME_INFO>() + size_of_val(name.as_slice());
    let api_size = u32::try_from(buffer_size).map_err(|_| invalid_name())?;
    let mut buffer = vec![0usize; buffer_size.div_ceil(size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    let file = OpenOptions::new()
        .access_mode(DELETE | FILE_WRITE_ATTRIBUTES)
        .open(temporary.path())?;
    let attributes = FILE_BASIC_INFO {
        FileAttributes: FILE_ATTRIBUTE_NORMAL,
        ..Default::default()
    };
    // Keep the target's locks/handles alive. MoveFileEx cannot replace an open destination;
    // FileRenameInfoEx with POSIX semantics atomically replaces its directory entry instead.
    const FILE_RENAME_REPLACE_IF_EXISTS: u32 = 0x1;
    const FILE_RENAME_POSIX_SEMANTICS: u32 = 0x2;
    // SAFETY: usize storage is pointer-aligned and large enough for the Win32 header and
    // UTF-16 tail. All pointers stay valid through the synchronous calls; the handle is owned.
    unsafe {
        (*info).Anonymous.Flags = FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS;
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            std::ptr::from_ref(&attributes).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        if SetFileInformationByHandle(
            file.as_raw_handle(),
            FileRenameInfoEx,
            buffer.as_ptr().cast(),
            api_size,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    temporary.disable_cleanup(true);
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
    fn atomic_replace_keeps_the_locked_original_and_publishes_complete_contents() {
        use std::io::{Read, Seek, SeekFrom};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("快照.json");
        write_private_atomic(&path, b"original").unwrap();
        let mut original = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs2::FileExt::lock_exclusive(&original).unwrap();
        write_private_atomic(&path, b"complete replacement").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"complete replacement");
        original.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
    }

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
