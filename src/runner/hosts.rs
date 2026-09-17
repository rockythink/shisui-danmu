//! Native ACP launch policies. Authentication and inference remain in the selected host.
use super::acp::SettingValue;
use super::settings::{Host, Settings, Source};
use agent_client_protocol::schema::v1::Implementation;
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    io::{Read, Write},
    path::Path,
    process::Stdio,
};
use tokio::process::Command;

mod amp;
mod claude;
mod codex;
mod copilot;
mod dsh;
mod opencode;
mod pi;

pub fn supports_search(host: Host) -> bool {
    match host {
        Host::Omp | Host::Gemini => true,
        Host::Claude => claude::SEARCH_SUPPORTED,
        Host::Codex => codex::SEARCH_SUPPORTED,
        Host::Copilot => copilot::SEARCH_SUPPORTED,
        Host::Cursor => false,
        Host::Dsh => dsh::SEARCH_SUPPORTED,
        Host::OpenCode => opencode::SEARCH_SUPPORTED,
        Host::Pi => pi::SEARCH_SUPPORTED,
        Host::Amp => amp::SEARCH_SUPPORTED,
    }
}

pub fn session_meta(settings: &Settings) -> Option<serde_json::Map<String, serde_json::Value>> {
    match settings.host {
        Host::Claude => Some(claude::session_meta(settings)),
        _ => None,
    }
}

#[cfg(unix)]
pub fn cleanup(workspace: &Path) {
    opencode::cleanup(workspace);
}

/// Stable application preferences map to each host's exact, audited native IDs.
pub fn preference_ids(host: Host) -> (Option<&'static str>, Option<&'static str>) {
    match host {
        Host::Omp => (Some("model"), Some("thinking")),
        Host::Claude | Host::OpenCode => (Some("model"), Some("effort")),
        Host::Codex | Host::Copilot | Host::Dsh => (Some("model"), Some("reasoning_effort")),
        Host::Pi => (Some("model"), Some("thought_level")),
        Host::Amp => (Some("amp-mode"), None),
        Host::Gemini | Host::Cursor => (None, None),
    }
}

fn adapter_package(host: Host) -> Option<(&'static str, &'static str)> {
    match host {
        Host::Claude => Some(("@agentclientprotocol/claude-agent-acp", "0.75.1")),
        Host::Codex => Some(("@agentclientprotocol/codex-acp", "1.10.0")),
        Host::Pi => Some(("pi-acp", "0.0.33")),
        Host::Amp => Some(("amp-acp", "0.9.0")),
        Host::Dsh => Some(("@deepseek-ai/dsh", "0.1.5-alpha.1")),
        _ => None,
    }
}

/// Prevent an ordinary native CLI from bypassing its adapter's private launcher.
/// This verifies an installed package entry, not the publisher's signature.
fn check_adapter_entry(host: Host, program: &Path) -> Result<()> {
    let Some((package, version)) = adapter_package(host) else {
        return Ok(());
    };
    let program = program.canonicalize()?;
    for directory in program.ancestors().skip(1).take(4) {
        let path = directory.join("package.json");
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 65536, "ACP安装包元数据过大");
        let metadata: serde_json::Value =
            serde_json::from_slice(&bytes).context("ACP安装包元数据损坏")?;
        ensure!(
            metadata["name"] == package && metadata["version"] == version,
            "安全接入要求已验证的 {package}@{version} 安装包入口；不运行普通CLI或未知版本包装器"
        );
        let bin = metadata["bin"]
            .as_str()
            .or_else(|| metadata["bin"][host.executable()].as_str())
            .context("安装包没有声明所选ACP入口")?;
        ensure!(
            directory.join(bin).canonicalize()? == program,
            "所选程序不是安装包声明的ACP入口"
        );
        return Ok(());
    }
    bail!("请指定 {package}@{version} 已安装包的真实入口，不要指定普通CLI或自定义shell包装器")
}

fn private_write(path: &Path, value: &[u8]) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("缺少私有目录")?)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if path.exists() {
        ensure!(
            std::fs::read(path)? == value,
            "专用工作目录快照变化，须新建上下文"
        );
        return Ok(());
    }
    options.open(path)?.write_all(value)?;
    Ok(())
}

pub(super) fn check_gemini_system_policy(root: &Path) -> Result<()> {
    ensure!(
        !root.join("settings.json").try_exists()?,
        "存在Gemini系统管理设置，无法保证私有安全覆盖生效；不覆盖企业策略"
    );
    match std::fs::read_dir(root.join("policies")) {
        Ok(entries) => {
            for entry in entries {
                ensure!(
                    entry?.path().extension().is_none_or(|e| e != "toml"),
                    "存在Gemini系统管理策略，原生CLI会忽略私有admin-policy；不覆盖企业策略"
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub fn native_prompt_text(path: &Path) -> Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .context("原生规则文件无法读取")?
        .take(32769)
        .read_to_string(&mut text)?;
    ensure!(
        !text.trim().is_empty() && text.len() <= 32768,
        "原生规则文件须为非空UTF-8且不超过32KiB"
    );
    Ok(text)
}

pub fn command(
    settings: &Settings,
    workspace: &Path,
) -> Result<(Command, Option<serde_json::Value>)> {
    settings.ready()?;
    let program = settings.program()?;
    check_adapter_entry(settings.host, &program)?;
    let mut cmd = Command::new(program);
    cmd.current_dir(workspace)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // User's selected native host owns authentication. Never forward this developer's
    // API keys, session IDs, provider overrides, executable hooks, or NODE_OPTIONS.
    for key in [
        "HOME",
        "USERPROFILE",
        "PATH",
        "LANG",
        "LC_ALL",
        "SYSTEMROOT",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("TMPDIR", workspace);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let mut initial_options = None;
    match settings.host {
        Host::Omp => {
            let mut overlay = json!({"disabledProviders":["native","omp-plugins","claude","agent-plugins","codex","agents","claude-plugins","gemini","opencode","cursor","windsurf","cline","github","vscode","agents-md","claude-md","mcp-json","ssh-json","builtin-defaults"],"memory":{"backend":"off"},"compaction":{"enabled":false},"autolearn":{"enabled":false},"speechgen":{"enabled":false},"externalThinking":false,"goal":{"enabled":false},"web_search":{"enabled":settings.web_search}});
            overlay["advisor"] = json!({"enabled":false});
            overlay["ttsr"] = json!({"enabled":false,"builtinRules":false});
            if !matches!(settings.source, Source::OmpProfile(_)) {
                overlay["personality"] = json!("none");
            }
            let config = workspace.join("omp-overlay.json");
            private_write(&config, &serde_json::to_vec(&overlay)?)?;
            cmd.arg("acp");
            if settings.web_search {
                cmd.args(["--tools", "web_search"]);
            } else {
                cmd.arg("--no-tools");
            }
            cmd.args([
                "--no-extensions",
                "--no-skills",
                "--no-rules",
                "--no-lsp",
                "--no-pty",
                "--no-title",
                "--no-prewalk",
                "--approval-mode",
                "always-ask",
                "--config",
            ])
            .arg(config)
            .arg("--session-dir")
            .arg(workspace.join("sessions"));
            match &settings.source {
                Source::Default => {
                    cmd.arg("--system-prompt").arg(crate::workspace::CONTRACT);
                }
                Source::OmpProfile(name) => {
                    cmd.args(["--profile", name]);
                }
                Source::NativePrompt(path) => {
                    let text = native_prompt_text(path)?;
                    let snapshot = workspace.join("native-system.txt");
                    private_write(&snapshot, text.as_bytes())?;
                    cmd.arg("--system-prompt").arg(snapshot);
                }
            }
            if !matches!(settings.source, Source::OmpProfile(_)) {
                let agent = workspace.join("omp-agent");
                // An existing empty config prevents OMP's legacy agent.db settings
                // migration. Only the native database is linked for auth refresh;
                // user config, rules, skills, plugins and history are not copied.
                if !agent.join("config.yml").exists() {
                    private_write(&agent.join("config.yml"), b"{}\n")?;
                }
                #[cfg(unix)]
                {
                    let home = directories::BaseDirs::new().context("无法定位OMP认证主目录")?;
                    let auth = home.home_dir().join(".omp/agent/agent.db");
                    if auth.is_file() && !agent.join("agent.db").exists() {
                        std::os::unix::fs::symlink(auth, agent.join("agent.db"))?;
                    }
                }
                cmd.env("PI_CODING_AGENT_DIR", agent);
            }
        }
        Host::Gemini => {
            let system = if cfg!(target_os = "macos") {
                "/Library/Application Support/GeminiCli"
            } else if cfg!(target_os = "windows") {
                r"C:\ProgramData\gemini-cli"
            } else {
                "/etc/gemini-cli"
            };
            // Gemini treats an empty filename list as GEMINI.md. Use a stable
            // private-runtime-specific name, never a user's default context file.
            use sha2::Digest;
            check_gemini_system_policy(Path::new(system))?;
            let mut native = json!({"tools":{"core":[],"exclude":["*"],"allowed":[],"discoveryCommand":"","callCommand":""},"hooksConfig":{"enabled":false},"skills":{"enabled":false},"mcp":{"serverCommand":"","allowed":[""],"excluded":[""]},"context":{"fileName":[]},"experimental":{"enableAgents":false,"extensionReloading":false,"plan":false}});
            native["context"]["fileName"] = json!(format!(
                ".shisui-no-native-context-{:x}.md",
                sha2::Sha256::digest(workspace.as_os_str().as_encoded_bytes())
            ));
            if settings.web_search {
                native["tools"]["core"] = json!(["google_web_search"]);
                native["tools"]["exclude"] = json!([]);
            }
            private_write(
                &workspace.join("gemini-system-settings.json"),
                &serde_json::to_vec(&native)?,
            )?;
            // ACP and the native process both use the private runtime directory.
            // File-based admin.* is ignored by Gemini; a system settings overlay and
            // complementary MCP allow/block lists disable even an empty server name.
            cmd.env(
                "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
                workspace.join("gemini-system-settings.json"),
            );
            // Core filtering does not cover Gemini built-in subagents. The private
            // admin-tier deny also outranks user always-allow without editing it.
            let policy = workspace.join("search-only.toml");
            let allow_search = if settings.web_search {
                "[[rule]]\ntoolName = \"google_web_search\"\ndecision = \"allow\"\npriority = 999\n\n"
            } else {
                ""
            };
            private_write(
                &policy,
                format!("{allow_search}[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 998\n").as_bytes(),
            )?;
            cmd.args(["--acp", "--extensions", "none", "--admin-policy"])
                .arg(policy);
        }
        Host::Claude => claude::configure(&mut cmd, settings, workspace)?,
        Host::Codex => codex::configure(&mut cmd, settings, workspace)?,
        Host::Copilot => copilot::configure(&mut cmd, settings, workspace)?,
        Host::Cursor => {
            bail!("Cursor 本轮未接入：原生端缺少完整工具与 hooks 禁用接口，请选择其他宿主")
        }
        Host::Dsh => dsh::configure(&mut cmd, settings, workspace)?,
        Host::OpenCode => opencode::configure(&mut cmd, settings, workspace)?,
        Host::Pi => initial_options = Some(pi::configure(&mut cmd, settings, workspace)?),
        Host::Amp => amp::configure(&mut cmd, settings, workspace)?,
    }
    Ok((cmd, initial_options))
}

pub fn validate_initial_options(
    host: Host,
    initial_options: Option<&serde_json::Value>,
    options: &super::acp::Options,
) -> Result<()> {
    if host == Host::Pi {
        pi::validate_initial_options(initial_options.context("缺少Pi启动选项快照")?, options)?;
    }
    Ok(())
}

pub fn verify_identity(host: Host, info: &Implementation) -> Result<()> {
    // Self-reported identity catches a wrong launch target; it is not attestation.
    // Execution restrictions are established before any ACP session is created.
    let names: &[&str] = match host {
        Host::Omp => &["oh-my-pi"],
        Host::Gemini => &["gemini-cli"],
        Host::Claude => claude::IDENTITIES,
        Host::Codex => codex::IDENTITIES,
        Host::Copilot => copilot::IDENTITIES,
        Host::Cursor => bail!("Cursor 本轮未接入"),
        Host::Dsh => dsh::IDENTITIES,
        Host::OpenCode => opencode::IDENTITIES,
        Host::Pi => pi::IDENTITIES,
        Host::Amp => amp::IDENTITIES,
    };
    ensure!(
        names.contains(&info.name.as_str()),
        "ACP身份不匹配：请检查宿主选择与ACP程序路径；未创建会话或调用模型"
    );
    let version = match host {
        Host::Claude => Some("0.75.1"),
        Host::Codex => Some("1.10.0"),
        Host::Copilot => Some("1.0.83"),
        _ => None,
    };
    if let Some(version) = version {
        ensure!(
            info.version == version,
            "该原生安全契约仅验证版本 {version}；未知版本未创建会话或调用模型"
        );
    }
    Ok(())
}

/// Categories are presentation metadata, not authority to change native permissions.
/// Only understood option semantics can change native state; discovering a new
/// option or upgrading a host does not automatically grant it permission.
pub fn permit_option(host: Host, id: Option<&str>, value: &SettingValue) -> Result<bool> {
    match host {
        Host::Claude => return claude::permit_option(id, value),
        Host::Codex => return codex::permit_option(id, value),
        Host::Copilot => return copilot::permit_option(id, value),
        Host::Cursor => bail!("Cursor 本轮未接入"),
        Host::Dsh => return dsh::permit_option(id, value),
        Host::OpenCode => return opencode::permit_option(id, value),
        Host::Pi => return pi::permit_option(id, value),
        Host::Amp => return amp::permit_option(id, value),
        Host::Omp | Host::Gemini => {}
    }
    let id_value = value
        .as_value_id()
        .context("此原生boolean选项尚未核实安全影响，保持继承")?;
    match (host, id, id_value.0.as_ref()) {
        (Host::Omp, Some("model"), _) => Ok(true),
        (Host::Omp, Some("thinking"), _) => Ok(false),
        (Host::Omp, Some("mode"), "default") | (Host::Gemini, None, "default") => Ok(false),
        _ => bail!(
            "此原生选项的权限/持久化影响未核实，保持继承。Gemini旧模型接口会写回宿主全局配置，未请求更改"
        ),
    }
}
