//! Native OpenCode ACP, audited at v1.2.15 and v1.18.30 (830d5eb53548).
//! Official source: anomalyco/opencode, config/config.ts, config/paths.ts,
//! plugin/index.ts, core/src/npm.ts, session/llm/request.ts and acp/service.ts.
//! Config overlays alone are NOT isolation: plugin arrays merge, dependencies
//! install before the overlay, and organization config can outrank the overlay.
use super::super::{acp::SettingValue, settings::Settings};
use anyhow::{Context, Result, bail, ensure};
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, Visitor},
};
use serde_json::json;
use std::{
    fmt, fs,
    io::Read,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::Command;

pub(super) const IDENTITIES: &[&str] = &["OpenCode"];
pub(super) const SEARCH_SUPPORTED: bool = false;

// Deliberately deserialize only the type. Provider names and credential fields
// are discarded, never copied into a Value/string snapshot or error message.
#[derive(Deserialize)]
struct AuthKind {
    #[serde(rename = "type")]
    kind: String,
}
struct PersonalAuth;
impl<'de> Deserialize<'de> for PersonalAuth {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Kinds;
        impl<'de> Visitor<'de> for Kinds {
            type Value = PersonalAuth;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("personal native authentication types")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                while map.next_key::<IgnoredAny>()?.is_some() {
                    let auth = map.next_value::<AuthKind>()?;
                    if !matches!(auth.kind.as_str(), "api" | "oauth") {
                        return Err(serde::de::Error::custom("unsupported authentication type"));
                    }
                }
                Ok(PersonalAuth)
            }
        }
        deserializer.deserialize_map(Kinds)
    }
}

fn check_personal_auth(path: &Path) -> Result<bool> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => bail!("无法检查 OpenCode 原生登录类型；未读取或输出凭据字段"),
    };
    // Do not include serde's input-bearing diagnostic, nor the provider names.
    ensure!(
        serde_json::from_reader::<_, PersonalAuth>(file.take(8 * 1024 * 1024)).is_ok(),
        "OpenCode 登录含组织 wellknown 远程引导、未知类型或损坏数据；本接入仅支持个人 api/oauth。请使用独立的个人原生登录配置；未启动宿主、未复制或输出凭据"
    );
    Ok(true)
}

fn check_managed_config() -> Result<()> {
    let system = if cfg!(target_os = "macos") {
        std::path::PathBuf::from("/Library/Application Support/opencode")
    } else if cfg!(windows) {
        std::env::var_os("ProgramData")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "C:\\ProgramData".into())
            .join("opencode")
    } else {
        "/etc/opencode".into()
    };
    for name in ["opencode.json", "opencode.jsonc"] {
        ensure!(
            !system.join(name).try_exists()?,
            "检测到 OpenCode 管理员配置；它可覆盖禁工具策略，请使用独立的无组织策略原生环境"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let root = Path::new("/Library/Managed Preferences");
        ensure!(
            !root.join("ai.opencode.managed.plist").try_exists()?,
            "OpenCode MDM 策略可覆盖本场隔离，未启动宿主"
        );
        match fs::read_dir(root) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        ensure!(
                            !entry
                                .path()
                                .join("ai.opencode.managed.plist")
                                .try_exists()?,
                            "检测到 OpenCode 用户 MDM 策略，未启动宿主"
                        );
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("无法排除 OpenCode MDM 策略"),
        }
    }
    Ok(())
}

fn native_version(cmd: &Command, workspace: &Path) -> Result<String> {
    let mut probe = std::process::Command::new(cmd.as_std().get_program());
    probe
        .arg("--version")
        .current_dir(workspace)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in cmd.as_std().get_envs() {
        if let Some(value) = value {
            probe.env(key, value);
        }
    }
    let mut child = probe.spawn().context("无法检查原生 OpenCode 版本")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("OpenCode --version 未在 5 秒内结束；未启动 ACP");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    ensure!(status.success(), "原生 OpenCode --version 失败；未启动 ACP");
    let mut version = String::new();
    child
        .stdout
        .take()
        .context("OpenCode 版本输出缺失")?
        .take(128)
        .read_to_string(&mut version)?;
    Ok(version.trim().to_owned())
}

pub(super) fn configure(cmd: &mut Command, settings: &Settings, workspace: &Path) -> Result<()> {
    ensure!(
        cfg!(unix),
        "此平台尚无已验证的 OpenCode 只读配置目录隔离；请在非 root 的 macOS/Linux 原生环境运行"
    );
    ensure!(
        !settings.web_search,
        "OpenCode 的完整 search 通知与权限链未核实；请关闭联网搜索"
    );
    check_managed_config()?;
    let home = std::env::var_os("HOME").context("缺少 HOME，无法定位个人 OpenCode 登录")?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| Path::new(&home).join(".local/share"));
    let native_auth = data.join("opencode/auth.json");
    let has_auth = check_personal_auth(&native_auth)?;
    let isolated_home = workspace.join("opencode-home");
    let config_root = workspace.join("opencode-config");
    let config = config_root.join("opencode");
    let private_data = workspace.join("opencode-data");
    fs::create_dir_all(&isolated_home)?;
    fs::create_dir_all(private_data.join("opencode"))?;
    super::private_write(
        &config.join(".gitignore"),
        b"node_modules\npackage.json\npackage-lock.json\nbun.lock\n",
    )?;
    // Both audited native versions return before dependency installation when
    // access(W_OK) fails. This is not a fabricated installed-package marker.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            unsafe { libc_geteuid() } != 0,
            "OpenCode 隔离不能以 root 运行：root 可绕过只读目录权限"
        );
        fs::set_permissions(&config, fs::Permissions::from_mode(0o500))?;
    }
    if has_auth {
        // Share only native personal login, NOT the native database/account
        // organization state. Credential refresh remains OpenCode's operation.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&native_auth, private_data.join("opencode/auth.json"))?;
    }
    let overlay = json!({
        "permission":"deny", "default_agent":"danmu", "agent":{"danmu":{
            "mode":"primary", "permission":"deny", "description":"弹幕助手（禁用所有工具）"
        }}, "plugin":[], "mcp":{}, "lsp":false, "formatter":false,
        "instructions":[], "share":"disabled", "autoupdate":false,
        "snapshot":false, "compaction":{"auto":false,"prune":false}
    });
    cmd.current_dir(workspace)
        .env("HOME", &isolated_home)
        .env("XDG_CONFIG_HOME", &config_root)
        .env("XDG_DATA_HOME", &private_data)
        .env("XDG_CACHE_HOME", workspace.join("opencode-cache"))
        .env("XDG_STATE_HOME", workspace.join("opencode-state"))
        .env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&overlay)?)
        .env("OPENCODE_PERMISSION", r#"{"*":"deny"}"#)
        .env("OPENCODE_PURE", "1")
        .env("OPENCODE_DISABLE_PROJECT_CONFIG", "1")
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .env("OPENCODE_DISABLE_MODELS_FETCH", "1")
        .env("OPENCODE_DISABLE_LSP_DOWNLOAD", "1")
        .env("OPENCODE_DISABLE_CLAUDE_CODE", "1")
        .env("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1")
        .env("OPENCODE_EXPERIMENTAL_DISABLE_FILEWATCHER", "1");
    let version = native_version(cmd, workspace)?;
    let current = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>();
    ensure!(
        version == "1.2.15"
            || matches!(current.as_deref(), Ok([1, minor, patch]) if (*minor, *patch) >= (18, 30)),
        "此 OpenCode 版本的插件启动契约未核实；支持已核实的 1.2.15 或 1.18.30+ 1.x，请自行升级官方 CLI（不会自动安装）"
    );
    if version == "1.2.15" {
        // Old release: skips downloaded Anthropic auth plugin, while compiled
        // Codex/Copilot/GitLab auth remains native. New release: --pure skips
        // external imports, leaving its compiled authentication plugins intact.
        cmd.env("OPENCODE_DISABLE_DEFAULT_PLUGINS", "1");
    }
    cmd.arg("acp");
    Ok(())
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

pub(super) fn permit_option(id: Option<&str>, value: &SettingValue) -> Result<bool> {
    let value = value
        .as_value_id()
        .context("OpenCode 未核实 boolean 原生选项")?;
    match (id, value.0.as_ref()) {
        (Some("model"), _) => Ok(true),
        (Some("effort"), _) => Ok(false),
        (Some("mode") | None, "danmu") => Ok(false),
        _ => bail!("该 OpenCode 原生选项不在安全会话契约内；不能切换到可执行工具的 agent/mode"),
    }
}

/// Called after the child is gone, before the private TempDir is removed.
#[cfg(unix)]
pub(super) fn cleanup(workspace: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let config = workspace.join("opencode-config/opencode");
        if fs::symlink_metadata(&config).is_ok_and(|metadata| metadata.is_dir()) {
            let _ = fs::set_permissions(config, fs::Permissions::from_mode(0o700));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn personal_auth_types_reject_remote_bootstrap_without_exposing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, br#"{"provider":{"type":"api","key":"private-sentinel"},"other":{"type":"oauth","access":"private-sentinel"}}"#).unwrap();
        assert!(check_personal_auth(&path).unwrap());
        fs::write(&path, br#"{"provider":{"type":"wellknown","token":"private-sentinel","key":"PRIVATE_TOKEN"}}"#).unwrap();
        let error = check_personal_auth(&path).unwrap_err().to_string();
        assert!(!error.contains("private-sentinel"));
        fs::write(
            &path,
            br#"{"provider":{"type":"oauth","type":"wellknown","token":"private-sentinel"}}"#,
        )
        .unwrap();
        assert!(check_personal_auth(&path).is_err());
    }
}
