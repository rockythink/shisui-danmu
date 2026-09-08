//! Official app-server integration, pinned to the reviewed no-environment protocol.
//! Never imports auth files or implements private subscription HTTP endpoints.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::watch,
};

pub const VERSION: &str = "0.147.0";
const FRAME_LIMIT: usize = 128 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);
const FEATURES: &[&str] = &[
    "shell_tool",
    "multi_agent",
    "multi_agent_v2",
    "plugins",
    "apps",
    "codex_hooks",
    "code_mode",
    "view_image",
    "image_generation",
    "remote_plugin",
    "tool_suggest",
    "memories",
    "remote_control",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub runtime: PathBuf,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(skip)]
    pub home: PathBuf,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            runtime: "codex".into(),
            model: None,
            effort: None,
            home: PathBuf::new(),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub model: String,
    pub display_name: String,
    pub supported_reasoning_efforts: Vec<Effort>,
    pub default_reasoning_effort: String,
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub hidden: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effort {
    pub reasoning_effort: String,
    pub description: String,
}
impl Model {
    pub fn validate_effort(&self, effort: &str) -> Result<()> {
        ensure!(
            self.supported_reasoning_efforts
                .iter()
                .any(|v| v.reasoning_effort == effort),
            "所选模型不支持该思考强度；请重新选择"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Default)]
pub struct Account {
    pub signed_in: bool,
    pub plan: Option<String>,
    pub quota: String,
}
fn account(value: Value) -> Result<Account> {
    let value = &value["account"];
    if value.is_null() {
        return Ok(Account::default());
    }
    ensure!(
        value["type"] == "chatgpt",
        "订阅后端拒绝API-key账号；请使用应用独立ChatGPT登录"
    );
    Ok(Account {
        signed_in: true,
        plan: value["planType"]
            .as_str()
            .filter(|v| super::safe_text(v))
            .map(str::to_owned),
        quota: String::new(),
    })
}
fn executable(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        ensure!(path.is_file(), "缺少codex运行时；请自行安装已支持版本");
        return Ok(path.to_owned());
    }
    ensure!(
        path.components().count() == 1,
        "codex运行时须为绝对路径或PATH名称"
    );
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|base| base.join(path))
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow::anyhow!("未找到codex；需要官方0.147.0，不会自动安装更新"))
}
fn isolated_command(binary: &Path, home: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("HOME", home)
        .env("CODEX_HOME", home)
        .env("LANG", "en_US.UTF-8")
        .env("TMPDIR", home.join("tmp"))
        .current_dir(home.join("workspace"));
    let mut paths = vec![binary.parent().unwrap_or(Path::new("/usr/bin")).to_owned()];
    paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}
fn prepare_home(home: &Path) -> Result<()> {
    ensure!(home.is_absolute(), "应用隔离CODEX_HOME未初始化");
    std::fs::create_dir_all(home)?;
    ensure!(
        !std::fs::symlink_metadata(home)?.file_type().is_symlink(),
        "拒绝符号链接CODEX_HOME"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    }
    for name in ["workspace", "tmp"] {
        std::fs::create_dir_all(home.join(name))?;
    }
    let mut config = String::from(
        "cli_auth_credentials_store = \"file\"\ncheck_for_update_on_startup = false\nweb_search = \"disabled\"\napproval_policy = \"never\"\nsandbox_mode = \"read-only\"\nproject_doc_max_bytes = 0\n[agents]\nenabled = false\n[analytics]\nenabled = false\n[features]\n",
    );
    for feature in FEATURES {
        config.push_str(&format!("{feature} = false\n"));
    }
    // This file is exclusively runtime-owned; application preferences live elsewhere.
    let mut file = tempfile::NamedTempFile::new_in(home)?;
    std::io::Write::write_all(&mut file, config.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(home.join("config.toml"))?;
    Ok(())
}

pub struct Session {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    id: u64,
    notifications: std::collections::VecDeque<Value>,
    active_turn: Option<(String, String)>,
    home: PathBuf,
}
impl Session {
    pub async fn open(config: &Config) -> Result<Self> {
        let home = config.home.clone();
        tokio::task::spawn_blocking(move || prepare_home(&home)).await??;
        let binary = executable(&config.runtime)?;
        let version = tokio::time::timeout(
            TIMEOUT,
            isolated_command(&binary, &config.home)
                .arg("--version")
                .output(),
        )
        .await??;
        ensure!(
            version.status.success()
                && String::from_utf8_lossy(&version.stdout).trim()
                    == format!("codex-cli {VERSION}"),
            "codex版本不兼容；仅支持经验证的0.147.0，不自动更新"
        );
        let mut child = isolated_command(&binary, &config.home)
            .args(["app-server", "--listen", "stdio://"])
            .spawn()
            .context("启动隔离codex失败")?;
        let stdin = child.stdin.take().context("缺少stdio写入端")?;
        let stdout = BufReader::new(child.stdout.take().context("缺少stdio读取端")?);
        let mut session = Self {
            child: Some(child),
            stdin: Some(stdin),
            stdout,
            id: 0,
            notifications: Default::default(),
            active_turn: None,
            home: config.home.clone(),
        };
        let init = session.request("initialize", json!({"clientInfo":{"name":"shisui_danmu","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        let returned = init["codexHome"].as_str().context("运行时未返回隔离目录")?;
        ensure!(
            std::fs::canonicalize(returned)? == std::fs::canonicalize(&config.home)?,
            "运行时未使用应用隔离目录"
        );
        session.write(json!({"method":"initialized"})).await?;
        let effective = session
            .request("config/read", json!({"includeLayers":true}))
            .await?;
        validate_config(&effective)?;
        Ok(session)
    }
    async fn write(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        ensure!(bytes.len() <= FRAME_LIMIT, "协议请求过大");
        bytes.push(b'\n');
        self.stdin
            .as_mut()
            .context("stdio已关闭")?
            .write_all(&bytes)
            .await?;
        Ok(())
    }
    async fn read(&mut self) -> Result<Value> {
        let mut frame = Vec::new();
        loop {
            let buf = self.stdout.fill_buf().await?;
            ensure!(!buf.is_empty(), "codex进程退出；未重试或回退API");
            let end = buf.iter().position(|v| *v == b'\n').map(|i| i + 1);
            let count = end.unwrap_or(buf.len());
            ensure!(
                frame.len() + count <= FRAME_LIMIT,
                "codex协议帧超限；安全终止"
            );
            frame.extend_from_slice(&buf[..count]);
            self.stdout.consume(count);
            if end.is_some() {
                break;
            }
        }
        let value: Value = serde_json::from_slice(&frame).context("codex返回无效JSONL")?;
        if value.get("id").is_some() && value.get("method").is_some() {
            self.write(json!({"id":value["id"],"error":{"code":-32601,"message":"DANMU does not allow tools or approvals"}})).await?;
            bail!("运行时请求工具或批准；安全终止，未授权执行");
        }
        if let Some(kind) = value.pointer("/params/item/type").and_then(Value::as_str) {
            ensure!(
                matches!(kind, "userMessage" | "agentMessage" | "reasoning" | "plan"),
                "运行时出现工具活动；安全终止"
            );
        }
        Ok(value)
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        let id = self.id;
        self.write(json!({"id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let value = self.read().await?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    // Do not display server error text: it may contain token/URL/input data.
                    ensure!(
                        value.get("error").is_none(),
                        "Codex请求失败：{method}；检查登录/权益/限额，未回退API"
                    );
                    return value.get("result").cloned().context("协议缺少result");
                }
                ensure!(self.notifications.len() < 64, "运行时通知过多");
                self.notifications.push_back(value);
            }
        })
        .await
        .context("Codex请求超时；不重试")?
    }
    async fn notification(&mut self) -> Result<Value> {
        if let Some(value) = self.notifications.pop_front() {
            return Ok(value);
        }
        self.read().await
    }
    pub async fn account(&mut self) -> Result<Account> {
        let mut account = account(
            self.request("account/read", json!({"refreshToken":false}))
                .await?,
        )?;
        if account.signed_in {
            let limits = self.request("account/rateLimits/read", json!({})).await?;
            account.quota = ["primary", "secondary"]
                .iter()
                .map(|name| {
                    let window = &limits["rateLimits"][*name];
                    let used = window["usedPercent"]
                        .as_f64()
                        .map(|v| format!("{v}%"))
                        .unwrap_or_else(|| "未报告".into());
                    let reset = window["resetsAt"]
                        .as_i64()
                        .and_then(|v| chrono::DateTime::from_timestamp(v, 0))
                        .map(|v| v.to_rfc3339())
                        .unwrap_or_else(|| "未报告".into());
                    format!("{name}已用{used} 重置{reset}")
                })
                .collect::<Vec<_>>()
                .join(" · ");
        }
        Ok(account)
    }
    pub async fn logout(&mut self) -> Result<()> {
        self.request("account/logout", json!({})).await?;
        ensure!(!self.account().await?.signed_in, "退出尚未确认");
        Ok(())
    }
    pub async fn models(&mut self) -> Result<Vec<Model>> {
        ensure!(
            self.account().await?.signed_in,
            "请先完成应用独立ChatGPT订阅登录"
        );
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut models = Vec::new();
        loop {
            let page = self
                .request(
                    "model/list",
                    json!({"cursor":cursor,"limit":50,"includeHidden":false}),
                )
                .await?;
            let rows: Vec<Model> =
                serde_json::from_value(page["data"].clone()).context("模型列表契约无效")?;
            for model in &rows {
                ensure!(
                    super::safe_text(&model.model)
                        && super::safe_text(&model.display_name)
                        && model.model.len() <= 200
                        && model.display_name.len() <= 200,
                    "模型元信息含不安全文本"
                );
                ensure!(
                    model
                        .supported_reasoning_efforts
                        .iter()
                        .all(|v| super::safe_text(&v.reasoning_effort)
                            && v.reasoning_effort.len() < 64),
                    "思考强度元信息无效"
                );
            }
            models.extend(
                rows.into_iter()
                    .filter(|m| !m.hidden && m.input_modalities.iter().any(|v| v == "text")),
            );
            ensure!(models.len() <= 500, "模型列表超限");
            cursor = page["nextCursor"].as_str().map(str::to_owned);
            let Some(next) = &cursor else {
                break;
            };
            ensure!(
                cursors.insert(next.clone()) && cursors.len() < 20,
                "模型分页循环或超限"
            );
        }
        Ok(models)
    }
    pub async fn login_start(mut self) -> Result<Login> {
        let result = self
            .request("account/login/start", json!({"type":"chatgpt"}))
            .await?;
        let id = result["loginId"].as_str().context("登录缺少ID")?.to_owned();
        let url = result["authUrl"]
            .as_str()
            .context("登录缺少URL")?
            .to_owned();
        let parsed = url::Url::parse(&url)?;
        ensure!(
            parsed.scheme() == "https"
                && parsed.host_str() == Some("auth.openai.com")
                && parsed.username().is_empty()
                && parsed.password().is_none(),
            "拒绝非官方授权URL"
        );
        Ok(Login {
            session: self,
            id,
            url,
        })
    }
    pub async fn generate(
        &mut self,
        model: &str,
        effort: &str,
        policy: &str,
        input: Value,
        ceiling: u64,
    ) -> Result<(String, u64)> {
        let models = self.models().await?;
        let selected = models
            .iter()
            .find(|m| m.model == model)
            .context("所选模型不再可用；请重新选择，不自动替换")?;
        selected.validate_effort(effort)?;
        let limits = self.request("account/rateLimits/read", json!({})).await?;
        ensure!(!exhausted(&limits), "订阅额度耗尽；暂停，未切换账号或API");
        let mut config =
            json!({"web_search":"disabled","project_doc_max_bytes":0,"agents.enabled":false});
        for feature in FEATURES {
            config[format!("features.{feature}")] = json!(false);
        }
        let thread = self.request("thread/start", json!({"model":model,"allowProviderModelFallback":false,"cwd":self.home.join("workspace"),"ephemeral":true,"environments":[],"runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[],"dynamicTools":[],"baseInstructions":policy,"developerInstructions":"Return only bounded candidate JSON. No tools, local context, or execution.","config":config})).await?;
        ensure!(thread["model"] == model, "运行时替换了所选模型；拒绝生成");
        let thread_id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("缺少thread ID")?
            .to_owned();
        let turn = self.request("turn/start", json!({"threadId":thread_id,"model":model,"effort":effort,"environments":[],"input":[{"type":"text","text":serde_json::to_string(&input)?}],"outputSchema":{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["reply","search","defer"]},"text":{"type":"string","maxLength":300},"topic_id":{"type":"string","maxLength":100}},"required":["action","text","topic_id"]}})).await?;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("缺少turn ID")?
            .to_owned();
        self.active_turn = Some((thread_id.clone(), turn_id.clone()));
        let mut text = String::new();
        let mut tokens = None;
        loop {
            let event = tokio::time::timeout(TIMEOUT, self.notification())
                .await
                .context("订阅回合超时")??;
            let params = &event["params"];
            match event["method"].as_str().unwrap_or("") {
                "thread/tokenUsage/updated" => {
                    let used = params
                        .pointer("/tokenUsage/total/totalTokens")
                        .and_then(Value::as_u64)
                        .context("缺少真实token用量")?;
                    tokens = Some(used);
                    ensure!(used <= ceiling, "订阅回合用量超过预留；暂停并取消");
                }
                "account/rateLimits/updated" => ensure!(!exhausted(params), "订阅额度耗尽；暂停"),
                "item/completed"
                    if params.pointer("/item/type") == Some(&json!("agentMessage")) =>
                {
                    let part = params
                        .pointer("/item/text")
                        .and_then(Value::as_str)
                        .context("缺少候选正文")?;
                    ensure!(text.len() + part.len() <= 4096, "候选输出过大");
                    text.push_str(part);
                }
                "turn/completed"
                    if params.pointer("/turn/id").and_then(Value::as_str) == Some(&turn_id) =>
                {
                    ensure!(
                        params.pointer("/turn/status") == Some(&json!("completed")),
                        "订阅回合失败或中断；暂停，不回退API"
                    );
                    self.active_turn = None;
                    ensure!(!text.is_empty(), "回合没有候选文本");
                    return Ok((text, tokens.context("订阅未返回用量；保留预算预留并暂停")?));
                }
                "error" => bail!("订阅回合错误；检查权益/限额/登录，未重试或回退API"),
                _ => {}
            }
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let mut stdin = self.stdin.take();
        let active = self.active_turn.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let (Some(stdin), Some((thread, turn))) = (stdin.as_mut(), active) {
                    let frame = format!("{}\n", json!({"id":u64::MAX,"method":"turn/interrupt","params":{"threadId":thread,"turnId":turn}}));
                    let _ = tokio::time::timeout(Duration::from_millis(200), stdin.write_all(frame.as_bytes())).await;
                }
                let _ = child.start_kill(); let _ = child.wait().await;
            });
        } else {
            let _ = child.start_kill();
        }
    }
}
fn validate_config(effective: &Value) -> Result<()> {
    let config = &effective["config"];
    for feature in FEATURES {
        ensure!(
            config["features"][*feature] == false,
            "隔离运行时未禁用{feature}"
        );
    }
    ensure!(
        config["agents"]["enabled"] == false,
        "运行时未禁用模型选择的多智能体能力"
    );
    ensure!(config["web_search"] == "disabled", "运行时搜索未禁用");
    for field in ["mcp_servers", "hooks"] {
        ensure!(
            config
                .get(field)
                .is_none_or(|v| v.is_null() || v.as_object().is_some_and(|o| o.is_empty())),
            "运行时继承了{field}；拒绝启动订阅后端"
        );
    }
    Ok(())
}
fn exhausted(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("usedPercent")
                .and_then(Value::as_f64)
                .is_some_and(|v| v >= 100.0)
                || map.values().any(exhausted)
        }
        Value::Array(values) => values.iter().any(exhausted),
        _ => false,
    }
}
pub struct Login {
    session: Session,
    pub id: String,
    pub url: String,
}
impl Login {
    pub async fn finish(self, cancelled: watch::Receiver<bool>) -> Result<Account> {
        self.finish_timeout(cancelled, Duration::from_secs(180))
            .await
    }
    async fn finish_timeout(
        mut self,
        mut cancelled: watch::Receiver<bool>,
        timeout: Duration,
    ) -> Result<Account> {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => { self.session.request("account/login/cancel", json!({"loginId":self.id})).await?; bail!("登录已超时并取消；可重新发起"); }
                _ = cancelled.changed() => { self.session.request("account/login/cancel", json!({"loginId":self.id})).await?; bail!("登录已取消"); }
                event = self.session.notification() => {
                    let event = event?;
                    if event["method"] == "account/login/completed" && event["params"]["loginId"] == self.id {
                        ensure!(event["params"]["success"] == true, "官方登录未成功；可重新发起");
                        let account = self.session.account().await?;
                        ensure!(account.signed_in, "登录通知成功但账号尚不可用");
                        return Ok(account);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
