//! @agentclientprotocol/claude-agent-acp 0.75.1 / Claude Agent SDK native CLI.
//! Upstream: https://github.com/agentclientprotocol/claude-agent-acp
use super::super::{acp::SettingValue, settings::Settings};
use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::path::Path;
use tokio::process::Command;

pub(super) const IDENTITIES: &[&str] = &["@agentclientprotocol/claude-agent-acp"];
pub(super) const SEARCH_SUPPORTED: bool = false;

pub(super) fn configure(cmd: &mut Command, settings: &Settings, _workspace: &Path) -> Result<()> {
    ensure!(
        !settings.web_search,
        "Claude 的搜索通知与权限合同尚未核实；请关闭网页搜索"
    );
    // Unlike --bare, native safe mode retains login/keychain authentication.
    // The adapter has no passthrough CLI flags; session options belong in _meta.
    cmd.env("CLAUDE_CODE_SAFE_MODE", "1");
    Ok(())
}

pub(super) fn session_meta(_settings: &Settings) -> Map<String, Value> {
    // createSession spreads these options after its defaults. tools is explicitly
    // preserved; strictMcpConfig suppresses ambient servers. Native safe mode
    // suppresses plugins, hooks, skills, agents and custom instructions, including
    // those not controlled by SDK settingSources. Do not replace HOME/auth.
    Map::from_iter([(
        "claudeCode".to_owned(),
        json!({"options": {
            "tools": [],
            "settingSources": [],
            "strictMcpConfig": true,
            "settings": {"disableAllHooks": true},
            "extraArgs": {"safe-mode": null, "disable-slash-commands": null}
        }}),
    )])
}

pub(super) fn permit_option(id: Option<&str>, value: &SettingValue) -> Result<bool> {
    ensure!(value.as_value_id().is_some(), "Claude 配置需要原生选项值");
    match id {
        // Effort updates the existing native query, preserving its chosen model.
        Some("model") => Ok(true),
        Some("effort") => Ok(false),
        _ => bail!("Claude 此选项会改变权限或扩展行为，未获授权"),
    }
}
