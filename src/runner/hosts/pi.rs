//! pi-acp 0.0.33 (svkozak/pi-acp) delegates RPC to PI_ACP_PI_COMMAND.
//! Keep the upstream ACP implementation; constrain its native Pi child instead.
use super::super::{acp::SettingValue, settings::Settings};
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub(super) const IDENTITIES: &[&str] = &["pi-acp"];
pub(super) const SEARCH_SUPPORTED: bool = true;

fn executable(name: &str) -> Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").context("缺少 PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| super::super::settings::executable_file(path))
        .with_context(|| format!("请先安装 {name}，拾穗不会自动安装"))
        .and_then(|path| Ok(std::fs::canonicalize(path)?))
}

fn quote(path: &Path) -> Result<String> {
    let path = path.to_str().context("Pi 路径必须为 UTF-8")?;
    ensure!(!path.contains(['\n', '\r', '\0']), "Pi 路径含控制字符");
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

// Pi accepts line comments and trailing commas. Normalize only outside strings.
fn read_configuration(path: &Path) -> Result<Option<serde_json::Value>> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("无法读取Pi原生配置；未启动Pi"),
    };
    ensure!(file.metadata()?.is_file(), "Pi配置必须为普通文件");
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "Pi配置超过1MiB，未启动Pi");
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        bytes.drain(..3);
    }
    for comments in [true, false] {
        let (mut quoted, mut escaped, mut index) = (false, false, 0);
        while index < bytes.len() {
            let byte = bytes[index];
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
            } else if byte == b'"' {
                quoted = true;
            } else if comments && byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
                while index < bytes.len() && bytes[index] != b'\n' {
                    bytes[index] = b' ';
                    index += 1;
                }
                continue;
            } else if !comments && byte == b',' {
                let next = bytes[index + 1..].iter().find(|b| !b.is_ascii_whitespace());
                if matches!(next, Some(b'}' | b']')) {
                    bytes[index] = b' ';
                }
            }
            index += 1;
        }
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "Pi配置JSON无效（行{}，列{}）；未显示原文",
            error.line(),
            error.column()
        )
    })?;
    ensure!(value.is_object(), "Pi配置根节点必须为对象");
    Ok(Some(value))
}

fn fields<'a>(
    value: &'a serde_json::Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    let object = value.as_object().context("Pi模型定义必须为对象")?;
    ensure!(
        object.keys().all(|key| allowed.contains(&key.as_str())),
        "Pi模型配置含未经验证字段；未导入、未启动Pi"
    );
    Ok(object)
}

fn credentials(value: &serde_json::Value) -> Result<()> {
    let object = value.as_object().context("Pi模型定义必须为对象")?;
    if let Some(key) = object.get("apiKey") {
        let key = key.as_str().context("Pi模型apiKey必须为字符串")?;
        ensure!(
            !key.trim_start().starts_with('!'),
            "Pi安全接入不执行命令型apiKey；请在原生Pi保存声明式认证，未执行命令、未显示凭据"
        );
    }
    if let Some(headers) = object.get("headers") {
        for value in headers
            .as_object()
            .context("Pi headers必须为对象")?
            .values()
        {
            let value = value.as_str().context("Pi header值必须为字符串")?;
            ensure!(
                !value.trim_start().starts_with('!'),
                "Pi安全接入不执行命令型header；请改用声明式认证，未执行命令、未显示凭据"
            );
        }
    }
    if let Some(url) = object.get("baseUrl") {
        ensure!(
            url.as_str().is_some_and(
                |s| url::Url::parse(s).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
            ),
            "Pi模型baseUrl必须为HTTP(S)地址"
        );
    }
    Ok(())
}

fn model(value: &serde_json::Value, definition: bool) -> Result<()> {
    let object = fields(
        value,
        &[
            "id",
            "name",
            "api",
            "baseUrl",
            "reasoning",
            "thinkingLevelMap",
            "input",
            "cost",
            "contextWindow",
            "maxTokens",
            "samplingParams",
            "headers",
            "compat",
        ],
    )?;
    credentials(value)?;
    if definition {
        ensure!(
            object
                .get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            "Pi模型缺少有效id"
        );
    }
    if let Some(parameters) = object.get("samplingParams") {
        let parameters = fields(
            parameters,
            &[
                "temperature",
                "top_p",
                "top_k",
                "min_p",
                "presence_penalty",
                "frequency_penalty",
                "repetition_penalty",
                "seed",
            ],
        )?;
        ensure!(
            parameters.values().all(|v| v.is_number()),
            "Pi采样参数只能是已验证的数值参数，不能覆盖消息或工具"
        );
    }
    Ok(())
}

fn validate_models(value: &serde_json::Value) -> Result<()> {
    let root = fields(value, &["providers"])?;
    let providers = root
        .get("providers")
        .and_then(|v| v.as_object())
        .context("Pi models.json缺少providers对象")?;
    for provider in providers.values() {
        let object = fields(
            provider,
            &[
                "name",
                "baseUrl",
                "api",
                "apiKey",
                "headers",
                "authHeader",
                "compat",
                "models",
                "modelOverrides",
            ],
        )?;
        credentials(provider)?;
        if let Some(models) = object.get("models") {
            for value in models.as_array().context("Pi models必须为数组")? {
                model(value, true)?;
            }
        }
        if let Some(overrides) = object.get("modelOverrides") {
            for value in overrides
                .as_object()
                .context("Pi modelOverrides必须为对象")?
                .values()
            {
                model(value, false)?;
            }
        }
    }
    Ok(())
}

fn prepare_configuration(
    native: &Path,
    directory: &Path,
    settings: &Settings,
) -> Result<serde_json::Value> {
    use serde_json::json;
    let mut source =
        read_configuration(&native.join("settings.json"))?.unwrap_or_else(|| json!({}));
    let mut safe = json!({"enableSkillCommands":false,"quietStartup":true,"packages":[],"extensions":[],"skills":[],"promptTemplates":[],"themes":[]});
    for key in ["defaultProvider", "defaultModel", "defaultThinkingLevel"] {
        if let Some(value) = source.as_object_mut().unwrap().remove(key) {
            ensure!(
                value.as_str().is_some_and(|s| !s.trim().is_empty()
                    && s.len() <= 1024
                    && !s.chars().any(char::is_control)),
                "Pi原生默认选项必须为非空字符串"
            );
            safe[key] = value;
        }
    }
    if let Some(saved) = settings.native_preferences() {
        if let Some(value) = &saved.model {
            let (provider, model) = value
                .split_once('/')
                .context("已保存Pi模型缺少provider；请重新选择模型")?;
            ensure!(
                !provider.is_empty() && !model.is_empty(),
                "已保存Pi模型无效"
            );
            safe["defaultProvider"] = provider.into();
            safe["defaultModel"] = model.into();
        }
        if let Some(value) = &saved.thinking {
            safe["defaultThinkingLevel"] = value.clone().into();
        }
    }
    if let Some(level) = safe.get("defaultThinkingLevel") {
        ensure!(
            level.as_str().is_some_and(|s| matches!(
                s,
                "off" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )),
            "当前pi-acp只支持off/minimal/low/medium/high/xhigh；原生默认档位不受支持，未静默降级"
        );
    }
    let models = read_configuration(&native.join("models.json"))?;
    if let Some(models) = &models {
        validate_models(models)?;
    }
    super::private_write(
        &directory.join("settings.json"),
        &serde_json::to_vec(&safe)?,
    )?;
    if let Some(models) = models {
        super::private_write(
            &directory.join("models.json"),
            &serde_json::to_vec(&models)?,
        )?;
    }
    Ok(safe)
}

pub(super) fn validate_initial_options(
    defaults: &serde_json::Value,
    options: &super::super::acp::Options,
) -> Result<()> {
    let provider = defaults.get("defaultProvider").and_then(|v| v.as_str());
    let model = defaults.get("defaultModel").and_then(|v| v.as_str());
    if provider.is_none() && model.is_none() {
        return Ok(());
    }
    let selected = options
        .config
        .as_ref()
        .and_then(|options| options.iter().find(|o| o.id == "model"))
        .context("Pi未报告模型选项；未调用推理")?;
    ensure!(
        selected
            .choices
            .iter()
            .any(|choice| choice.value == selected.current),
        "Pi当前模型不在原生可用列表；请检查默认模型定义与认证，未调用推理"
    );
    let pair = selected
        .current
        .as_value_id()
        .and_then(|value| value.0.split_once('/'));
    ensure!(
        pair.is_some_and(|(p, m)| provider.is_none_or(|expected| expected == p)
            && model.is_none_or(|expected| expected == m)),
        "Pi未采用已选模型；请检查原生provider/模型定义/认证，未调用推理或回退其他模型"
    );
    Ok(())
}

pub(super) fn configure(
    cmd: &mut Command,
    settings: &Settings,
    workspace: &Path,
) -> Result<serde_json::Value> {
    ensure!(cfg!(unix), "Pi 安全启动器目前需要 Unix");
    let pi = executable("pi")?;
    let node = executable("node")?;
    ensure!(
        pi.extension()
            .is_some_and(|ext| ext == "js" || ext == "mjs"),
        "请使用官方 Node.js 版 Pi"
    );
    let agent_dir = workspace.join("pi-agent");
    let native = std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or(PathBuf::from(std::env::var_os("HOME").context("缺少 HOME")?).join(".pi/agent"));
    ensure!(native.is_absolute(), "PI_CODING_AGENT_DIR必须为绝对路径");
    let defaults = prepare_configuration(&native, &agent_dir, settings)?;
    let auth = native.join("auth.json");
    // Link only authentication. Model metadata and three defaults are private snapshots.
    #[cfg(unix)]
    if auth.is_file() {
        std::os::unix::fs::symlink(&auth, agent_dir.join("auth.json"))?;
    }
    let launcher = workspace.join("pi-text-launcher");
    let rpc_filter = workspace.join("pi-rpc-filter.js");
    super::private_write(&rpc_filter, include_bytes!("pi_rpc_filter.js"))?;
    // Do not forward adapter arguments: only a fresh RPC session is authorized.
    // Node's permission model also blocks shell-valued credential helpers.
    let mut native_options = String::new();
    if settings.web_search {
        let extension = workspace.join("pi-web-search.js");
        super::private_write(&extension, include_bytes!("pi_search.js"))?;
        native_options.push_str(&format!(
            " --no-builtin-tools --extension {}",
            quote(&extension)?
        ));
    } else {
        native_options.push_str(" --no-tools");
    }
    for (field, flag) in [
        ("defaultProvider", "--provider"),
        ("defaultModel", "--model"),
        ("defaultThinkingLevel", "--thinking"),
    ] {
        if let Some(value) = defaults.get(field).and_then(|v| v.as_str()) {
            native_options.push_str(&format!(" {flag} {}", quote(Path::new(value))?));
        }
    }
    let script = format!(
        "#!/bin/sh
set -o pipefail
{} --permission --allow-fs-read='*' --allow-fs-write={} --allow-fs-write={} {} --mode rpc --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve --offline{native_options} | {} {}
",
        quote(&node)?,
        quote(workspace)?,
        quote(&auth)?,
        quote(&pi)?,
        quote(&node)?,
        quote(&rpc_filter)?,
    );
    super::private_write(&launcher, script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
    }
    cmd.env("PI_ACP_PI_COMMAND", launcher)
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .env("PI_OFFLINE", "1");
    Ok(defaults)
}

pub(super) fn permit_option(id: Option<&str>, value: &SettingValue) -> Result<bool> {
    let value = value.as_value_id().context("Pi 配置必须是已公布的选择值")?;
    match id {
        Some("model") => Ok(true),
        Some("thought_level")
            if matches!(
                value.0.as_ref(),
                "off" | "minimal" | "low" | "medium" | "high" | "xhigh"
            ) =>
        {
            Ok(false)
        }
        _ => bail!("此 Pi 选项没有经过安全验证"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rpc_filter_keeps_successful_text_separate_from_retries_tools_and_errors() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let Ok(node) = executable("node") else {
            return;
        };
        let text = |delta: &str| json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":delta}});
        let plan = r#"{"round_id":"round","candidates":[]}"#;
        let events = [
            json!({"type":"agent_start"}),
            text("failed partial {"),
            json!({"type":"auto_retry_start","attempt":1}),
            text("tool commentary"),
            json!({"type":"message_update","assistantMessageEvent":{"type":"toolcall_start","toolCall":{"id":"search","name":"web_search"}}}),
            json!({"type":"tool_execution_start","toolCallId":"search","toolName":"web_search"}),
            json!({"type":"tool_execution_end","toolCallId":"search","result":{"content":[]},"isError":false}),
            text(plan),
            json!({"type":"auto_retry_end","success":true}),
            json!({"type":"agent_settled"}),
            json!({"type":"agent_start"}),
            text("unsuccessful partial"),
            json!({"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"503 unavailable"}}),
            json!({"type":"agent_settled"}),
            json!({"type":"agent_start"}),
            text("before compaction"),
            json!({"type":"auto_compaction_start"}),
            json!({"type":"auto_compaction_end"}),
            text(plan),
            json!({"type":"agent_settled"}),
        ];
        let mut child = Command::new(node)
            .env_clear()
            .args([
                "--input-type=module",
                "-e",
                include_str!("pi_rpc_filter.js"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        for event in events {
            writeln!(input, "{event}").unwrap();
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let text: Vec<_> = events
            .iter()
            .filter(|event| event["assistantMessageEvent"]["type"] == "text_delta")
            .map(|event| event["assistantMessageEvent"]["delta"].as_str().unwrap())
            .collect();
        assert_eq!(text, [plan, plan]);
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "tool_execution_end"
                    && event["toolCallId"] == "search")
        );
        let failures: Vec<_> = events
            .iter()
            .filter(|event| {
                event["type"] == "extension_ui_request" && event["notifyType"] == "error"
            })
            .collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0]["message"].as_str().unwrap().contains("503"));
    }

    #[test]
    fn native_jsonc_keeps_strings_and_redacts_invalid_configuration() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("models.json");
        std::fs::write(
            &path,
            r#"{
            // Native Pi accepts line comments and trailing commas.
            "url": "https://example.test/a//b",
            "quoted": "a\\\"//b",
            "items": [1, 2,],
        }"#,
        )
        .unwrap();
        let value = read_configuration(&path).unwrap().unwrap();
        assert_eq!(value["url"], "https://example.test/a//b");
        assert_eq!(value["quoted"], "a\\\"//b");
        assert_eq!(value["items"], json!([1, 2]));
        std::fs::write(
            &path,
            r#"{"apiKey": "fixture-secret", "broken": fixture-secret}"#,
        )
        .unwrap();
        let error = read_configuration(&path).unwrap_err().to_string();
        assert!(!error.contains("fixture-secret"));
    }

    #[test]
    fn executable_credentials_and_request_overrides_fail_before_snapshot_creation() {
        for provider in [
            json!({"apiKey": "!echo fixture-secret"}),
            json!({"headers": {"Authorization": "!echo fixture-secret"}}),
            json!({"models": [{"id": "model", "headers": {"X-Key": "!echo fixture-secret"}}]}),
            json!({"modelOverrides": {"model": {"headers": {"X-Key": "!echo fixture-secret"}}}}),
            json!({"models": [{"id": "model", "samplingParams": {"tools": []}}]}),
            json!({"models": [{"id": "model", "extensions": ["fixture-secret"]}]}),
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("models.json");
            let bytes = serde_json::to_vec(&json!({"providers": {"p": provider}})).unwrap();
            std::fs::write(&source, &bytes).unwrap();
            let private = root.path().join("private");
            let error = prepare_configuration(root.path(), &private, &Settings::default())
                .unwrap_err()
                .to_string();
            assert!(!error.contains("fixture-secret"));
            assert!(!private.exists());
            assert_eq!(std::fs::read(source).unwrap(), bytes);
        }
    }
}
