//! Official ACP adapter 1.10.0 -> native Codex app-server 0.153.x.
//! https://github.com/agentclientprotocol/codex-acp (4eb3cb6)
//! Its "read-only" mode actually sends workspaceWrite on turn/start. The narrow
//! native-protocol boundary below corrects this BEFORE native tool dispatch;
//! ACP remains implemented entirely by the upstream adapter.
use super::super::{acp::SettingValue, settings::Settings};
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub(super) const IDENTITIES: &[&str] = &["@agentclientprotocol/codex-acp"];
pub(super) const SEARCH_SUPPORTED: bool = false;

fn executable(name: &str) -> Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|path| super::super::settings::executable_file(path))
        .with_context(|| format!("Codex 安全接入需要已安装的 {name}；不会自动下载"))?
        .canonicalize()
        .with_context(|| format!("无法解析 {name} 可执行文件"))
}

pub(super) fn configure(cmd: &mut Command, settings: &Settings, workspace: &Path) -> Result<()> {
    ensure!(
        !settings.web_search,
        "Codex 搜索的完整通知与权限合同尚未核实；请关闭网页搜索"
    );
    let native = executable("codex")?;
    let node = executable("node")?;
    let original = PathBuf::from(std::env::var_os("HOME").context("缺少 HOME")?).join(".codex");
    let home = workspace.join("codex");
    let user_home = workspace.join("codex-user");
    std::fs::create_dir_all(&user_home)?;
    // Isolate ~/.agents discovery as well as CODEX_HOME startup configuration.
    // Authentication is a narrow native-owned link, never read or copied here.
    super::private_write(&home.join("config.toml"), CONFIG.as_bytes())?;
    let auth = original.join("auth.json");
    if auth.is_file() {
        let link = home.join("auth.json");
        if link.symlink_metadata().is_ok() {
            ensure!(std::fs::read_link(&link)? == auth, "Codex 认证链接发生变化");
        } else {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&auth, &link)?;
            #[cfg(not(unix))]
            bail!("Codex 安全认证链接需要 Unix 文件系统");
        }
    }
    let proxy = workspace.join("codex-app-server");
    // Use the resolved interpreter, not an inherited PATH shim.
    let interpreter = node.to_str().context("Node 路径不是 UTF-8")?;
    ensure!(
        !interpreter.contains(['\n', '\r', ' ']),
        "Node 路径不适用于安全启动脚本"
    );
    super::private_write(&proxy, format!("#!{interpreter}\n{PROXY}").as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o700))?;
    }
    cmd.env("HOME", &user_home)
        .env("CODEX_HOME", &home)
        .env("CODEX_PATH", &proxy)
        .env("SHISUI_CODEX_NATIVE", &native)
        .env("SHISUI_CODEX_WORKSPACE", workspace)
        .env("INITIAL_AGENT_MODE", "read-only")
        .env("NO_BROWSER", "1");
    Ok(())
}

pub(super) fn permit_option(id: Option<&str>, value: &SettingValue) -> Result<bool> {
    ensure!(value.as_value_id().is_some(), "Codex 配置需要原生选项值");
    match id {
        Some("model") => Ok(true),
        Some("reasoning_effort") => Ok(false),
        _ => bail!("Codex 此选项会改变权限或扩展行为，未获授权"),
    }
}

const CONFIG: &str = r#"approval_policy = "never"
sandbox_mode = "read-only"
web_search = "disabled"
project_doc_max_bytes = 0
notify = []
allow_login_shell = false
[orchestrator.skills]
enabled = false
[orchestrator.mcp]
enabled = false
[skills]
include_instructions = false
[skills.bundled]
enabled = false
[agents]
enabled = false
[tools.update_plan]
enabled = false
[tools.experimental_request_user_input]
enabled = false
[features]
shell_tool = false
unified_exec = false
shell_snapshot = false
view_image = false
image_generation = false
multi_agent = false
multi_agent_v2 = false
collab = false
apps = false
plugins = false
plugin_hooks = false
hooks = false
codex_hooks = false
js_repl = false
code_mode = false
code_mode_only = false
computer_use = false
memories = false
skill_mcp_dependency_install = false
skip_host_skill_discovery = true
"#;

// Native protocol v2: ThreadStartParams.sandbox and TurnStartParams.sandboxPolicy
// have DIFFERENT schemas. Neither an ACP permission notification nor a hook
// (which fails open on execution failure) is the enforcement boundary.
const PROXY: &str = r#"'use strict';
const { spawn, spawnSync } = require('node:child_process');
if (process.argv.length !== 3 || process.argv[2] !== 'app-server') process.exit(64);
const native = process.env.SHISUI_CODEX_NATIVE;
const cwd = process.env.SHISUI_CODEX_WORKSPACE;
if (!native || !cwd) process.exit(64);
// Older servers silently ignore environments: []; never fall back to their tools.
const version = spawnSync(native, ['--version'], {
  cwd, env: process.env, encoding: 'utf8', timeout: 5000, maxBuffer: 4096
});
if (version.status !== 0 || version.stdout.trim() !== 'codex-cli 0.153.4') {
  process.stderr.write('Text-only Codex requires verified native version 0.153.4\n');
  process.exit(78);
}
const child = spawn(native, ['app-server', '--stdio'], {
  cwd, stdio: ['pipe', 'pipe', 'inherit'], env: process.env
});
const readonly = { type: 'readOnly' };
const allowed = new Set([
  'initialize', 'initialized', 'account/read', 'account/rateLimits/read',
  'config/read', 'model/list', 'skills/list', 'skills/extraRoots/set',
  'mcpServerStatus/list', 'thread/list', 'thread/loaded/list', 'thread/read',
  'thread/turns/list', 'thread/start', 'thread/resume', 'thread/fork',
  'thread/unsubscribe', 'thread/archive', 'thread/name/set',
  'turn/start', 'turn/interrupt', 'turn/steer'
]);
let stopping = false;
let killTimer;
function stop() {
  if (stopping) return;
  stopping = true;
  child.stdin.end();
  child.kill('SIGTERM');
  killTimer = setTimeout(() => child.kill('SIGKILL'), 1000);
}
for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) process.on(signal, stop);
child.on('error', () => { process.stderr.write('Codex native app-server could not start\n'); process.exitCode = 1; stop(); });
child.on('exit', code => {
  clearTimeout(killTimer);
  process.stdin.destroy();
  process.exitCode = stopping ? 0 : (code || 1);
});
process.stdout.on('error', stop);
child.stdin.on('error', stop);
function send(stream, value) {
  return stream.write(JSON.stringify(value) + '\n');
}
function reject(id) {
  return { id, error: { code: -32601, message: 'Unavailable in text-only host' } };
}
function fromAdapter(msg) {
  // Server-owned permission requests never leave this boundary, so an adapter
  // response cannot approve them. Unknown methods fail closed, not passthrough.
  if (typeof msg.method !== 'string') { stop(); return; }
  if (!allowed.has(msg.method)) {
    if ('id' in msg) send(process.stdout, reject(msg.id));
    return;
  }
  const p = msg.params || {};
  if (['thread/start', 'thread/resume', 'thread/fork'].includes(msg.method)) {
    // Preserve only native model preferences, never adapter-supplied tool or
    // provider/config overrides. Native authentication stays in auth.json.
    msg.params = { threadId: p.threadId, model: p.model, serviceTier: p.serviceTier,
      cwd, sandbox: 'read-only', approvalPolicy: 'never', environments: [],
      approvalsReviewer: 'user', ephemeral: true };
  } else if (msg.method === 'turn/start') {
    msg.params = { threadId: p.threadId, input: p.input, model: p.model,
      effort: p.effort, summary: p.summary, serviceTier: p.serviceTier,
      outputSchema: p.outputSchema, cwd, approvalPolicy: 'never',
      approvalsReviewer: 'user', sandboxPolicy: readonly, environments: [] };
  } else if (msg.method === 'skills/extraRoots/set') {
    msg.params = { extraRoots: [] };
  }
  return send(child.stdin, msg);
}
function fromNative(msg) {
  if (typeof msg.method === 'string' && 'id' in msg) {
    if (msg.method === 'item/fileChange/requestApproval' || msg.method === 'item/commandExecution/requestApproval') {
      send(child.stdin, { id: msg.id, result: { decision: 'decline' } });
    } else if (msg.method === 'item/permissions/requestApproval') {
      send(child.stdin, { id: msg.id, result: { permissions: {}, scope: 'turn' } });
    } else send(child.stdin, reject(msg.id));
    return;
  }
  return send(process.stdout, msg);
}
function pump(stream, destination, handle) {
  let pending = Buffer.alloc(0);
  stream.on('data', chunk => {
    pending = Buffer.concat([pending, chunk]);
    for (;;) {
      const end = pending.indexOf(10);
      if (end < 0) break;
      if (end > 1048576) { stop(); return; }
      const line = pending.subarray(0, end);
      pending = pending.subarray(end + 1);
      if (!line.length) continue;
      let value;
      try { value = JSON.parse(line.toString('utf8')); }
      catch { stop(); return; }
      if (!value || typeof value !== 'object' || Array.isArray(value)) { stop(); return; }
      if (handle(value) === false) {
        stream.pause();
        destination.once('drain', () => stream.resume());
      }
    }
    if (pending.length > 1048576 || destination.writableLength > 8388608) stop();
  });
  stream.on('error', stop);
  stream.on('end', stop);
}
pump(process.stdin, child.stdin, fromAdapter);
pump(child.stdout, process.stdout, fromNative);
"#;
