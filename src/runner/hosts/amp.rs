//! tao12345666333/amp-acp 0.9.0, using its native CLI transport.
//! Native Amp wildcard tool denial plus an OS no-child-process boundary.
use super::super::{acp::SettingValue, settings::Settings};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub(super) const IDENTITIES: &[&str] = &["amp-acp"];
pub(super) const SEARCH_SUPPORTED: bool = false;

fn quote(path: &Path) -> Result<String> {
    let path = path.to_str().context("Amp 路径必须为 UTF-8")?;
    ensure!(!path.contains(['\n', '\r', '\0']), "Amp 路径含控制字符");
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

pub(super) fn configure(cmd: &mut Command, settings: &Settings, workspace: &Path) -> Result<()> {
    ensure!(!settings.web_search, "Amp 当前仅启用无工具文本模式");
    let native = std::env::split_paths(&std::env::var_os("PATH").context("缺少 PATH")?)
        .map(|dir| dir.join("amp"))
        .find(|path| super::super::settings::executable_file(path))
        .context("请先安装 Amp CLI，拾穗不会自动安装")?;
    let native = std::fs::canonicalize(native)?;
    let home = workspace.join("amp-home");
    let config = home.join(".config");
    let data = home.join(".local/share");
    let settings_file = config.join("amp/settings.json");
    super::private_write(
        &settings_file,
        &serde_json::to_vec(&json!({
            "amp.tools.disable": ["*"],
            "amp.permissions": [{"tool":"*","action":"reject"}],
            "amp.dangerouslyAllowAll": false,
            "amp.mcpServers": {},
            "amp.skills.disableClaudeCodeSkills": true,
            "amp.updates.mode": "disabled",
            "amp.notifications.enabled": false,
            "amp.notifications.system.enabled": false,
            "amp.remoteThreadCreation.enabled": false
        }))?,
    )?;
    let user_home = PathBuf::from(std::env::var_os("HOME").context("缺少 HOME")?);
    #[cfg(unix)]
    for (source, destination) in [
        (
            user_home.join(".local/share/amp/secrets.json"),
            data.join("amp/secrets.json"),
        ),
        (
            user_home.join(".config/amp-acp/credentials.json"),
            config.join("amp-acp/credentials.json"),
        ),
    ] {
        if source.is_file() {
            std::fs::create_dir_all(destination.parent().context("缺少认证目录")?)?;
            std::os::unix::fs::symlink(source, destination)?;
        }
    }
    #[cfg(not(target_os = "macos"))]
    bail!("Amp 安全启动器目前需要 macOS 沙箱来禁止自动打开认证浏览器");
    ensure!(
        data.join("amp/secrets.json").is_file()
            || config.join("amp-acp/credentials.json").is_file(),
        "请先在您自己的终端完成 amp login；拾穗不会自动打开认证浏览器"
    );
    let sandbox = workspace.join("amp-no-exec.sb");
    let native_literal = serde_json::to_string(native.to_str().context("Amp 路径必须为 UTF-8")?)?;
    super::private_write(&sandbox, format!("(version 1)\n(allow default)\n(deny process-fork)\n(deny process-exec)\n(allow process-exec (literal {native_literal}))\n(deny appleevent-send)\n(deny file* (regex #\"/global-skills(/|$)\"))\n").as_bytes())?;
    let launcher = workspace.join("amp-text-launcher");
    // Amp plugins use a same-binary Bun child (BUN_BE_BUN=1); deny fork as
    // well as exec so neither remote plugins nor browser login can launch.
    let script = format!(
        r#"#!/bin/sh
thread=
mode=low
if [ "$1" = threads ]; then
  [ "$2" = continue ] || exit 64
  case "$3" in T-*) thread=$3 ;; *) exit 64 ;; esac
  shift 3
fi
while [ "$#" -gt 0 ]; do
  case "$1" in
    --execute|--stream-json|--no-archive-after-execute) shift ;;
    --mode)
      case "$2" in low|medium|high|ultra) mode=$2 ;; *) exit 64 ;; esac
      shift 2 ;;
    *) exit 64 ;;
  esac
done
set -- --settings-file {} --no-ide --no-notifications --no-remote-control-terminal --execute --stream-json --no-archive-after-execute --mode "$mode"
if [ -n "$thread" ]; then set -- threads continue "$thread" "$@"; fi
exec /usr/bin/sandbox-exec -f {} {} "$@"
"#,
        quote(&settings_file)?,
        quote(&sandbox)?,
        quote(&native)?
    );
    super::private_write(&launcher, script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
    }
    // Private cwd is required for session/new too: no .amp project/plugin roots.
    cmd.current_dir(workspace)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", data)
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("AMP_ACP_TRANSPORT", "cli")
        .env("AMP_CLI_PATH", launcher)
        .env("AMP_SETTINGS_FILE", settings_file)
        .env("AMP_REMOTE_CONTROL_TERMINAL", "0");
    Ok(())
}

pub(super) fn permit_option(id: Option<&str>, value: &SettingValue) -> Result<bool> {
    let value = value
        .as_value_id()
        .context("Amp 配置必须是已公布的选择值")?;
    match (id, value.0.as_ref()) {
        (Some("amp-mode"), "low" | "medium" | "high" | "ultra") => Ok(true),
        (Some("permission"), "default") => Ok(false),
        _ => bail!("此 Amp 选项会绕过安全策略或尚未经验证"),
    }
}
