//! Official deepseek-ai/deepseek-harness 0.1.5-alpha.1 profile transport.
//! An explicit empty-root composition prevents future base-bundle tool additions.
use super::super::{acp::SettingValue, settings::Settings};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::path::Path;
use tokio::process::Command;

pub(super) const IDENTITIES: &[&str] = &["deepseek-harness-acp"];
pub(super) const SEARCH_SUPPORTED: bool = false;

pub(super) fn configure(cmd: &mut Command, settings: &Settings, workspace: &Path) -> Result<()> {
    ensure!(!settings.web_search, "DSH 当前仅启用无工具文本模式");
    let home = workspace.join("dsh-home");
    let profile = home.join("profiles/shisui-text");
    let credential_path = std::env::var_os("HOME").context("缺少 HOME")?;
    let credential_path = Path::new(&credential_path).join(".dsh/.credentials.yaml");
    super::private_write(
        &profile.join("package.json"),
        &serde_json::to_vec(&json!({
            "name": "shisui-text", "private": true, "type": "module",
            "dsh": {"profile": {"bundles": [], "patchReload": "startup"}}
        }))?,
    )?;
    let mut entries = Vec::new();
    for (id, name) in [
        ("timer", "@deepseek-ai/cordis-plugin-timer"),
        ("llm", "@deepseek-ai/dsh-llm"),
        ("session", "@deepseek-ai/dsh-session"),
        ("session-projection", "@deepseek-ai/dsh-session-projection"),
        ("agent", "@deepseek-ai/dsh-agent"),
        ("tools", "@deepseek-ai/dsh-tools"),
        (
            "deepseek-llm-api-extensions",
            "@deepseek-ai/dsh-deepseek-llm-api-extensions",
        ),
        (
            "session-log-deepseek",
            "@deepseek-ai/dsh-session-log-deepseek",
        ),
        ("llm-deepseek", "@deepseek-ai/dsh-llm-deepseek"),
        ("acp-app-startup", "@deepseek-ai/dsh-acp-app"),
    ] {
        entries.push(json!({"id": id, "name": name}));
    }
    entries.extend([
        json!({"id":"credentials", "name":"@deepseek-ai/dsh-credentials-local", "config":{"path":credential_path,"watch":false}}),
        json!({"id":"system-prompt", "name":"@deepseek-ai/dsh-system-prompt", "config":{"includeHarnessIdentity":false,"includeRuntimeContext":false,"personaPrefix":"You are a helpful text assistant. No tools are available."}}),
        json!({"id":"agent-loop", "name":"@deepseek-ai/dsh-agent-loop", "config":{"agents":[]}}),
        json!({"id":"sessions", "name":"@deepseek-ai/dsh-session-persistence-jsonl", "config":{"root":home.join("sessions"),"compression":"none"}}),
        json!({"id":"acp", "name":"@deepseek-ai/dsh-acp", "inject":["acpAppStartup"], "config":{"provider":"deepseek-official","model":"deepseek-v4-flash"}}),
    ]);
    // JSON is a YAML subset. There are no !!js expressions, user includes, hooks,
    // MCP mounts, filesystem/shell providers, tool implementations or base bundle.
    super::private_write(
        &profile.join("cordis.patch.yml"),
        &serde_json::to_vec(&json!([{"insert":entries}]))?,
    )?;
    cmd.env("DSH_HOME", &home)
        .args(["--profile", "shisui-text"]);
    Ok(())
}

pub(super) fn permit_option(id: Option<&str>, value: &SettingValue) -> Result<bool> {
    ensure!(
        value.as_value_id().is_some(),
        "DSH 配置必须是已公布的选择值"
    );
    match id {
        Some("model") => Ok(true),
        Some("reasoning_effort") => Ok(false),
        _ => bail!("此 DSH 选项没有经过安全验证"),
    }
}
