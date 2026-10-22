use anyhow::{Result, ensure};
use reqwest::header::HeaderValue;

// No Debug, Display or Serialize: authentication never becomes diagnostics.
pub(crate) struct ApiKey(HeaderValue);
impl ApiKey {
    pub(crate) fn new(input: &str) -> Result<Self> {
        let input = input.trim();
        ensure!(
            !input.is_empty() && input.len() <= 4096,
            "Key长度须为1..4096字节"
        );
        ensure!(
            input.bytes().all(|b| (0x21..=0x7e).contains(&b)),
            "Key须为可见ASCII"
        );
        let mut value = HeaderValue::from_str(input).map_err(|_| anyhow::anyhow!("Key格式错误"))?;
        value.set_sensitive(true);
        Ok(Self(value))
    }
    pub(crate) fn header(&self) -> HeaderValue {
        self.0.clone()
    }
}

use super::settings::{ApiConfig, ApiProtocol, normalize_base_url};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};
const MAX_RECORD: u64 = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    format: u8,
    protocol: ApiProtocol,
    base_url: String,
    key: String,
}

// Resolve the public parent once. Only assistant-secrets itself requires 0700.
fn key_path(config_path: &Path, create: bool) -> Result<Option<PathBuf>> {
    let parent = config_path.parent().context("Key配置缺少父目录")?;
    let parent = match parent.canonicalize() {
        Ok(path) => path,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => anyhow::bail!("Key配置父目录不可读取"),
    };
    let meta = std::fs::metadata(&parent)?;
    ensure!(meta.is_dir(), "Key配置父路径不是目录");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
            "Key配置父目录须属当前用户且不可被组或其他人写入"
        );
    }
    let root = parent.join("assistant-secrets");
    match std::fs::symlink_metadata(&root) {
        Ok(meta) => {
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "Key私有目录无效或为符号链接"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
                    "Key私有目录须属当前用户且仅本人可访问（0700）"
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            crate::bridge::wire::private_root(&root)?;
        }
        Err(_) => anyhow::bail!("检查Key私有目录失败"),
    }
    Ok(Some(root.join("api-key.json")))
}

fn opened_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    ensure!(meta.is_file(), "Key文件不是普通文件");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.mode() & 0o077 == 0,
            "Key文件须由当前用户独占且仅本人可访问（0600）"
        );
    }
    Ok(())
}

pub(crate) fn load(config_path: &Path, api: &ApiConfig) -> Result<Option<ApiKey>> {
    ensure!(
        api.protocol != ApiProtocol::ChatgptSubscription,
        "ChatGPT订阅须使用独立OAuth登录"
    );
    let base_url = normalize_base_url(&api.base_url)?;
    let Some(path) = key_path(config_path, false)? else {
        return Ok(None);
    };
    crate::workspace_config::check_file(&path).context("Key文件不安全")?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => anyhow::bail!("读取Key文件失败"),
    };
    opened_file(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("读取Key文件失败"))?;
    ensure!(bytes.len() as u64 <= MAX_RECORD, "Key文件超过16KiB");
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Key文件格式损坏"))?;
    ensure!(record.format == 1, "Key文件格式未知");
    ensure!(
        record.protocol != ApiProtocol::ChatgptSubscription,
        "Key记录不能使用订阅OAuth协议"
    );
    let recorded_url = normalize_base_url(&record.base_url)?;
    let key = ApiKey::new(&record.key)?;
    if record.protocol != api.protocol || recorded_url != base_url {
        return Ok(None);
    }
    Ok(Some(key))
}

pub(crate) fn save(config_path: &Path, api: &ApiConfig, input: &str) -> Result<()> {
    ensure!(
        api.protocol != ApiProtocol::ChatgptSubscription,
        "ChatGPT订阅须使用独立OAuth登录"
    );
    let base_url = normalize_base_url(&api.base_url)?;
    let _checked = ApiKey::new(input)?;
    let path = key_path(config_path, true)?.context("缺少Key私有目录")?;
    crate::workspace_config::check_file(&path).context("Key文件不安全")?;
    let record = Record {
        format: 1,
        protocol: api.protocol,
        base_url,
        key: input.trim().to_owned(),
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| anyhow::anyhow!("Key文件编码失败"))?;
    ensure!(bytes.len() as u64 <= MAX_RECORD, "Key文件超过16KiB");
    crate::storage::write_private_atomic(&path, &bytes)
}

pub(crate) fn clear(config_path: &Path) -> Result<()> {
    let Some(path) = key_path(config_path, false)? else {
        return Ok(());
    };
    crate::workspace_config::check_file(&path).context("Key文件不安全")?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => anyhow::bail!("清除Key文件失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "FAKE_PRIVATE_KEY_SENTINEL";
    fn api() -> ApiConfig {
        ApiConfig {
            model: "fixture".into(),
            ..ApiConfig::default()
        }
    }
    fn disk(root: &Path) -> PathBuf {
        root.join("assistant-secrets/api-key.json")
    }
    #[test]
    fn scopes_use_protocol_and_canonical_url_not_model() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let mut config = api();
        config.base_url = " HTTPS://API.OPENAI.COM:443/v1/// ".into();
        save(&path, &config, &format!(" {KEY} ")).unwrap();
        config.base_url = "https://api.openai.com/v1".into();
        config.model = "another-model".into();
        assert_eq!(
            load(&path, &config)
                .unwrap()
                .unwrap()
                .header()
                .to_str()
                .unwrap(),
            KEY
        );
        let record: Record =
            serde_json::from_slice(&std::fs::read(disk(root.path())).unwrap()).unwrap();
        assert_eq!(record.base_url, "https://api.openai.com/v1");
        config.protocol = ApiProtocol::AnthropicMessages;
        assert!(load(&path, &config).unwrap().is_none());
        config.protocol = ApiProtocol::OpenaiChat;
        config.base_url = "https://example.com/v1".into();
        assert!(load(&path, &config).unwrap().is_none());
        config.base_url = "http://localhost/v1".into();
        assert!(load(&path, &config).is_err());
        clear(&path).unwrap();
        assert!(!disk(root.path()).exists());
        assert!(root.path().join("assistant-secrets").is_dir());
        clear(&path).unwrap();
    }
    #[test]
    fn sensitive_header_and_invalid_input_preserve_old_record() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let config = api();
        save(&path, &config, KEY).unwrap();
        let previous = std::fs::read(disk(root.path())).unwrap();
        for input in [
            String::new(),
            "secret\nheader".into(),
            "私有".into(),
            "x".repeat(4097),
            "secret\0".into(),
        ] {
            assert!(save(&path, &config, &input).is_err());
            assert_eq!(std::fs::read(disk(root.path())).unwrap(), previous);
        }
        let header = load(&path, &config).unwrap().unwrap().header();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains(KEY));
    }
    #[test]
    fn corruption_is_error_even_when_connection_scope_does_not_match() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        save(&path, &api(), KEY).unwrap();
        for value in [
            serde_json::json!({"format":2,"protocol":"openai_chat","base_url":"https://x.test/v1","key":KEY}),
            serde_json::json!({"format":1,"protocol":"openai_chat","base_url":"bad-url","key":KEY}),
            serde_json::json!({"format":1,"protocol":"openai_chat","base_url":"https://x.test/v1","key":KEY, "private":KEY}),
            serde_json::json!({"format":1,"protocol":"openai_chat","base_url":"https://x.test/v1","key":""}),
        ] {
            crate::storage::write_private_atomic(
                &disk(root.path()),
                &serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
            let error = load(&path, &api()).err().unwrap();
            assert!(!format!("{error:#}").contains(KEY));
        }
        crate::storage::write_private_atomic(
            &disk(root.path()),
            &vec![b'x'; MAX_RECORD as usize + 1],
        )
        .unwrap();
        assert!(load(&path, &api()).is_err());
    }
    #[test]
    fn nonexistent_store_is_absent_but_invalid_url_still_errors() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        assert!(load(&path, &api()).unwrap().is_none());
        let config = ApiConfig {
            base_url: "bad-url".into(),
            ..api()
        };
        assert!(load(&path, &config).is_err());
        clear(&path).unwrap();
        assert!(!root.path().join("assistant-secrets").exists());
    }
    #[cfg(unix)]
    #[test]
    fn unix_private_root_file_and_public_parent_permissions_are_enforced() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        save(&path, &api(), KEY).unwrap();
        let private = root.path().join("assistant-secrets");
        assert_eq!(std::fs::metadata(&private).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::metadata(disk(root.path())).unwrap().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load(&path, &api()).is_err());
        assert!(save(&path, &api(), "next").is_err());
        assert!(clear(&path).is_err());
        assert_eq!(std::fs::metadata(&private).unwrap().mode() & 0o777, 0o755);
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(disk(root.path()), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(load(&path, &api()).is_err());
        assert!(clear(&path).is_err());
        std::fs::set_permissions(disk(root.path()), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(load(&path, &api()).is_err());
        assert!(save(&path, &api(), "next").is_err());
        assert!(clear(&path).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symbolic_links_hard_links_and_wrong_file_types_are_not_followed_or_deleted() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        save(&path, &api(), KEY).unwrap();
        let key = disk(root.path());
        let original = std::fs::read(&key).unwrap();
        let other = root.path().join("other");
        std::fs::rename(&key, &other).unwrap();
        symlink(&other, &key).unwrap();
        assert!(load(&path, &api()).is_err());
        assert!(save(&path, &api(), "next").is_err());
        assert!(clear(&path).is_err());
        assert_eq!(std::fs::read(&other).unwrap(), original);
        std::fs::remove_file(&key).unwrap();
        std::fs::hard_link(&other, &key).unwrap();
        assert!(load(&path, &api()).is_err());
        assert!(clear(&path).is_err());
        std::fs::remove_file(&key).unwrap();
        std::fs::create_dir(&key).unwrap();
        assert!(load(&path, &api()).is_err());
        assert!(save(&path, &api(), "next").is_err());
        assert!(clear(&path).is_err());
        std::fs::remove_dir(&key).unwrap();
        std::fs::rename(
            root.path().join("assistant-secrets"),
            root.path().join("saved-secrets"),
        )
        .unwrap();
        symlink(
            root.path().join("saved-secrets"),
            root.path().join("assistant-secrets"),
        )
        .unwrap();
        assert!(load(&path, &api()).is_err());
        assert!(save(&path, &api(), "next").is_err());
        assert!(clear(&path).is_err());
    }
    #[test]
    fn ipv6_default_port_scope_reuses_same_key() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let mut config = ApiConfig {
            base_url: "http://[0:0:0:0:0:0:0:1]:80/v1/".into(),
            ..api()
        };
        save(&path, &config, KEY).unwrap();
        config.base_url = "http://[::1]/v1".into();
        assert!(load(&path, &config).unwrap().is_some());
    }
    #[cfg(unix)]
    #[test]
    fn actual_atomic_write_failure_preserves_old_key() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        } // root bypasses ordinary directory write permissions.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        save(&path, &api(), KEY).unwrap();
        let old = std::fs::read(disk(root.path())).unwrap();
        let private = root.path().join("assistant-secrets");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = save(&path, &api(), "replacement");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(disk(root.path())).unwrap(), old);
        assert_eq!(
            load(&path, &api())
                .unwrap()
                .unwrap()
                .header()
                .to_str()
                .unwrap(),
            KEY
        );
    }
}
