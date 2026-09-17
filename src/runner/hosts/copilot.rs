//! Native @github/copilot 1.0.83, not the registry's language-server package.
//! https://github.com/github/copilot-cli; packaged ACP implementation and native
//! runtime were inspected and exercised without a paid model request.
use super::super::{acp::SettingValue, settings::Settings};
use anyhow::{Result, bail, ensure};
use std::path::Path;
use tokio::process::Command;

pub(super) const IDENTITIES: &[&str] = &["Copilot"];
pub(super) const SEARCH_SUPPORTED: bool = false;

pub(super) fn configure(cmd: &mut Command, settings: &Settings, workspace: &Path) -> Result<()> {
    ensure!(
        !settings.web_search,
        "Copilot 搜索的完整通知与权限合同尚未核实；请关闭网页搜索"
    );
    let home = workspace.join("copilot");
    super::private_write(
        &home.join("settings.json"),
        br#"{"disableAllHooks":true,"ide":{"autoConnect":false},"enabledPlugins":{},"extraKnownMarketplaces":{}}"#,
    )?;
    // No config.json symlink: native auth and legacy executable settings share
    // that file, and legacy settings override settings.json. Linking it would
    // silently restore hooks/plugins and permit writes to the user's config.
    // ACP session/new only queries native auth.current(): it never runs the
    // interactive login command. Keep gh's native stored-auth detection enabled;
    // --no-auto-login disables gh too. No credentials are copied into config.
    // An empty --available-tools resets to ALL tools; use an exact allowlist
    // containing one unregistered, unguessable name, with extensions disabled.
    cmd.arg(format!(
        "--available-tools=__shisui_text_only_{}",
        uuid::Uuid::new_v4().simple()
    ))
    .env("COPILOT_HOME", &home)
    .env("COPILOT_PLUGIN_DIR_ONLY", "true")
    .args([
        "--acp",
        "--stdio",
        "--disable-builtin-mcps",
        "--no-custom-instructions",
        "--no-auto-update",
        "--no-remote",
        "--no-remote-export",
        "--no-bash-env",
    ]);
    Ok(())
}

pub(super) fn permit_option(id: Option<&str>, value: &SettingValue) -> Result<bool> {
    ensure!(value.as_value_id().is_some(), "Copilot 配置需要原生选项值");
    match id {
        Some("model") => Ok(true),
        Some("reasoning_effort") => Ok(false),
        // In particular, autopilot enables allow-all; neither mode, allow_all,
        // nor agent selection is merely a presentation preference.
        _ => bail!("Copilot 此选项会改变权限或扩展行为，未获授权"),
    }
}
