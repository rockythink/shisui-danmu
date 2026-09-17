//! Official ACP 2.1.0 / stable protocol v1. Only application policy lives here.
use super::{
    hosts,
    settings::{Host, Settings},
};
use agent_client_protocol::{self as sdk, Agent, Channel, Client, ConnectionTo, TransportFrame};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
pub use sdk::schema::v1::SessionConfigOptionValue as SettingValue;
use sdk::schema::{ProtocolVersion, v1::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

pub const FRAME_LIMIT: usize = 256 * 1024;
const TEXT_LIMIT: usize = 256 * 1024;
// Monotonic connection budgets bound even SDK-internal unbounded queues BEFORE admission.
const CONNECTION_BYTES: usize = 4 * 1024 * 1024;
const CONNECTION_FRAMES: usize = 4096;
#[cfg(not(test))]
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
// Real Python fixture processes need startup headroom on loaded macOS hosts.
const RPC_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(not(test))]
const PROMPT_TIMEOUT: Duration = Duration::from_secs(180);
#[cfg(test)]
const PROMPT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(not(test))]
const CANCEL_TIMEOUT: Duration = Duration::from_secs(6);
#[cfg(test)]
const CANCEL_TIMEOUT: Duration = Duration::from_millis(300);

pub fn value_label(value: &SettingValue) -> String {
    match value {
        SettingValue::ValueId { value } => value.to_string(),
        SettingValue::Boolean { value } => value.to_string(),
        _ => "未支持".into(),
    }
}
fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect()
}
fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control),
        "ACP标识无效"
    );
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: SettingValue,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub current: SettingValue,
    pub choices: Vec<Choice>,
    pub unsupported: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub config: Option<Vec<ConfigOption>>,
    pub modes: Option<(String, Vec<Choice>)>,
}
impl Options {
    fn replace_config(&mut self, options: Vec<SessionConfigOption>) -> Result<()> {
        ensure!(options.len() <= 32, "ACP配置超过32项");
        let mut rows = Vec::with_capacity(options.len());
        let mut ids = HashSet::new();
        for o in options {
            let id = o.id.to_string();
            identifier(&id)?;
            ensure!(ids.insert(id.clone()), "重复配置ID");
            let mut choices = Vec::new();
            let current = match o.kind {
                SessionConfigKind::Select(s) => {
                    identifier(&s.current_value.to_string())?;
                    match s.options {
                        SessionConfigSelectOptions::Ungrouped(values) => {
                            for v in values {
                                identifier(&v.value.to_string())?;
                                choices.push(Choice {
                                    value: v.value.into(),
                                    name: label(&v.name),
                                });
                            }
                        }
                        SessionConfigSelectOptions::Grouped(groups) => {
                            for g in groups {
                                for v in g.options {
                                    identifier(&v.value.to_string())?;
                                    choices.push(Choice {
                                        value: v.value.into(),
                                        name: format!("{} / {}", label(&g.name), label(&v.name)),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                    s.current_value.into()
                }
                SessionConfigKind::Boolean(b) => {
                    choices.push(Choice {
                        value: false.into(),
                        name: "关闭".into(),
                    });
                    choices.push(Choice {
                        value: true.into(),
                        name: "开启".into(),
                    });
                    b.current_value.into()
                }
                _ => continue,
            };
            // Frame and connection byte limits bound native catalogs, not model count.
            let mut values = HashSet::with_capacity(choices.len());
            for choice in &choices {
                if let Some(value) = choice.value.as_value_id() {
                    ensure!(values.insert(value.0.as_ref()), "重复配置值");
                }
            }
            let category = o.category.map(|c| match c {
                SessionConfigOptionCategory::Model => "model".into(),
                SessionConfigOptionCategory::Mode => "mode".into(),
                SessionConfigOptionCategory::ThoughtLevel => "thought_level".into(),
                SessionConfigOptionCategory::ModelConfig => "model_config".into(),
                SessionConfigOptionCategory::Other(s) => label(&s),
                _ => "其他".into(),
            });
            rows.push(ConfigOption {
                id,
                name: label(&o.name),
                category,
                current,
                unsupported: choices.is_empty(),
                choices,
            });
        }
        self.config = Some(rows);
        self.modes = None;
        Ok(())
    }
    fn setup(
        &mut self,
        config: Option<Vec<SessionConfigOption>>,
        modes: Option<SessionModeState>,
    ) -> Result<()> {
        if let Some(config) = config {
            self.replace_config(config)?;
        }
        if self.config.is_none()
            && let Some(m) = modes
        {
            ensure!(m.available_modes.len() <= 32, "旧mode选项过多");
            identifier(&m.current_mode_id.to_string())?;
            let mut values = Vec::new();
            for v in m.available_modes {
                identifier(&v.id.to_string())?;
                values.push(Choice {
                    value: SettingValue::value_id(v.id.to_string()),
                    name: label(&v.name),
                });
            }
            self.modes = Some((m.current_mode_id.to_string(), values));
        }
        Ok(())
    }
    pub fn validate_change(&self, c: &Change) -> Result<()> {
        let choices = if let Some(id) = &c.id {
            let o = self
                .config
                .as_ref()
                .and_then(|rows| rows.iter().find(|o| &o.id == id))
                .context("当前Agent未报告该选项")?;
            ensure!(!o.unsupported, "未知类型不可更改");
            &o.choices
        } else {
            ensure!(self.config.is_none(), "有configOptions时禁止旧mode回退");
            &self.modes.as_ref().context("未报告旧mode")?.1
        };
        ensure!(
            choices.iter().any(|v| v.value == c.value),
            "请求值已不在最新选项中"
        );
        Ok(())
    }
    pub fn matches(&self, c: &Change) -> bool {
        if let Some(id) = &c.id {
            self.config
                .as_ref()
                .and_then(|v| v.iter().find(|o| &o.id == id))
                .is_some_and(|o| o.current == c.value)
        } else {
            self.modes
                .as_ref()
                .is_some_and(|(v, _)| c.value.as_value_id().is_some_and(|id| id.to_string() == *v))
        }
    }
}
#[cfg(test)]
mod option_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn large_native_catalogs_preserve_last_choice_and_reject_duplicates_atomically() {
        let values: Vec<_> = (0..257)
            .map(|i| json!({"value":format!("provider/model-{i}"),"name":format!("Model {i}")}))
            .collect();
        for grouped in [false, true] {
            let mut row = json!({"id":"model","name":"Model","category":"model",
                "type":"select","currentValue":"provider/model-0","options":values});
            if grouped {
                row["options"] = json!([
                    {"group":"first","name":"First","options":&values[..128]},
                    {"group":"second","name":"Second","options":&values[128..]}
                ]);
            }
            let mut options = Options::default();
            options
                .replace_config(vec![serde_json::from_value(row.clone()).unwrap()])
                .unwrap();
            options
                .validate_change(&Change {
                    id: Some("model".into()),
                    value: "provider/model-256".into(),
                    new_context: true,
                })
                .unwrap();
            let previous = options.clone();
            if grouped {
                row["options"][1]["options"][0] = values[0].clone();
            } else {
                row["options"][128] = values[0].clone();
            }
            assert!(
                options
                    .replace_config(vec![serde_json::from_value(row).unwrap()])
                    .is_err()
            );
            assert_eq!(options, previous);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub session: Option<String>,
    pub agent: String,
    pub version: String,
    pub options: Options,
    pub capabilities: AgentCapabilities,
    pub authentication: Vec<String>,
    pub connected: bool,
    pub restoration_error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct Restore {
    pub session: String,
    pub capabilities: AgentCapabilities,
}
#[derive(Debug, Clone)]
pub struct Change {
    pub id: Option<String>,
    pub value: SettingValue,
    pub new_context: bool,
}
#[derive(Debug)]
pub struct Prompt {
    pub round_id: String,
    pub text: String,
    pub message_ids: Arc<[String]>,
    pub cancel_epoch: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub message_id: String,
    pub text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    round_id: String,
    candidates: Vec<Candidate>,
}
#[derive(Debug, thiserror::Error)]
#[error("{kind}候选消息ID（最多80字符）：{id:?}")]
struct InvalidCandidateTarget {
    kind: &'static str,
    id: String,
}
pub enum Answer {
    Candidates(Vec<Candidate>),
    NoMessage,
    Rejected(String),
}
pub fn candidates(text: &str, round: &str, ids: &[String]) -> Result<Vec<Candidate>> {
    let plan: Plan = serde_json::from_str(text).with_context(|| {
        // Persist enough evidence in assistantStopped before the private runtime retires.
        // Only malformed output allocates a preview; Debug escapes terminal controls.
        let preview: String = text.chars().take(160).collect();
        format!(
            "正文不是完整候选JSON；收到{}字节，正文前160字符={preview:?}",
            text.len()
        )
    })?;
    ensure!(plan.round_id == round, "候选属于旧轮次");
    ensure!(plan.candidates.len() <= 8, "最多8条候选");
    let mut seen = HashSet::new();
    for c in &plan.candidates {
        let kind = if !ids.contains(&c.message_id) {
            Some("未知本批")
        } else if !seen.insert(&c.message_id) {
            Some("重复本批")
        } else {
            None
        };
        if let Some(kind) = kind {
            return Err(InvalidCandidateTarget {
                kind,
                id: c.message_id.chars().take(80).collect(),
            }
            .into());
        }
        crate::bridge::Bridge::validate_reply_text(&c.text)?;
    }
    Ok(plan.candidates)
}
pub enum Action {
    Prompt(Prompt),
    Configure(Change),
}
pub enum Event {
    Ready,
    Started {
        round_id: String,
    },
    Answer {
        round_id: String,
        result: Result<Answer, String>,
    },
    Configured(Result<(), String>),
    Closed(Result<(), String>),
}
struct Control {
    cancel: watch::Receiver<u64>,
    stop: watch::Receiver<bool>,
}
pub struct Workspace {
    pub runtime: Arc<tempfile::TempDir>,
}
impl Workspace {
    #[cfg(all(test, unix))]
    pub fn isolated(runtime: Arc<tempfile::TempDir>) -> Self {
        Self { runtime }
    }
}
#[cfg(unix)]
impl Drop for Workspace {
    fn drop(&mut self) {
        hosts::cleanup(self.runtime.path());
    }
}
pub struct Handle {
    pub actions: mpsc::Sender<Action>,
    pub events: mpsc::Receiver<Event>,
    pub snapshot: watch::Receiver<Snapshot>,
    cancel: watch::Sender<u64>,
    stop: watch::Sender<bool>,
    pub task: JoinHandle<()>,
}
impl Handle {
    pub fn spawn(settings: Settings, root: Workspace, restore: Option<Restore>) -> Self {
        let (actions, rx) = mpsc::channel(1);
        let (tx, events) = mpsc::channel(8);
        let (publish, snapshot) = watch::channel(Snapshot::default());
        let (cancel, cancel_rx) = watch::channel(0);
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            let result = run(
                settings,
                root,
                restore,
                rx,
                &tx,
                publish,
                Control {
                    cancel: cancel_rx,
                    stop: stop_rx,
                },
            )
            .await
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::Closed(result)).await;
        });
        Self {
            actions,
            events,
            snapshot,
            cancel,
            stop,
            task,
        }
    }
    pub fn epoch(&self) -> u64 {
        *self.cancel.borrow()
    }
    pub fn cancel(&self) {
        self.cancel.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub fn stop(&self) {
        self.cancel();
        let _ = self.stop.send(true);
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}
struct OwnedGroup(i32);
impl OwnedGroup {
    fn stop(&mut self) {
        let pid = std::mem::replace(&mut self.0, 0);
        #[cfg(unix)]
        if pid > 1 {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            } // SAFETY: only our child, started in its own process group.
            unsafe {
                kill(-pid, 9);
            }
        }
        #[cfg(not(unix))]
        let _ = pid;
    }
}
impl Drop for OwnedGroup {
    fn drop(&mut self) {
        self.stop();
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Setup,
    New,
    Load,
    Resume,
    Idle,
    Prompt,
    Configure,
    Close,
}
struct SearchCall {
    title: String,
    input: Option<serde_json::Value>,
    status: ToolCallStatus,
}

fn pi_search_input(input: Option<&serde_json::Value>, complete: bool) -> Result<()> {
    let Some(input) = input else {
        ensure!(!complete, "Pi搜索执行前缺少query");
        return Ok(());
    };
    let object = input.as_object().context("Pi搜索参数必须是对象")?;
    ensure!(object.len() <= 1, "Pi搜索含未授权参数");
    if let Some(query) = object.get("query") {
        let query = query.as_str().context("Pi搜索query必须是字符串")?;
        ensure!(
            query.chars().take(241).count() <= 240 && !query.chars().any(char::is_control),
            "Pi搜索query超限或含控制字符"
        );
        ensure!(!complete || !query.trim().is_empty(), "Pi搜索query不能为空");
    } else {
        ensure!(
            !complete
                && (object.is_empty()
                    || object
                        .get("partialArgs")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s.len() <= 2048)),
            "Pi搜索只接受query"
        );
    }
    Ok(())
}

fn search_content(content: &[ToolCallContent]) -> bool {
    content.iter().all(|item| {
        matches!(item,
            ToolCallContent::Content(c) if matches!(c.content, ContentBlock::Text(_))
        )
    })
}

struct State {
    // Process-launch permissions never follow model-supplied config updates.
    host: Host,
    // Pi may persist a fallback into its settings file; retain the launch selection here.
    initial_options: Option<serde_json::Value>,
    search_enabled: bool,
    searches: HashMap<ToolCallId, SearchCall>,
    seen_searches: HashSet<ToolCallId>,
    snapshot: Snapshot,
    publish: watch::Sender<Snapshot>,
    phase: Phase,
    text: String,
    message: Option<MessageId>,
    identity_seen: bool,
    seen: HashSet<MessageId>,
    early: Option<SessionId>,
    failure: Option<String>,
}
impl State {
    fn publish(&self) {
        self.publish.send_replace(self.snapshot.clone());
    }
    fn searches_finished(&self) -> bool {
        self.searches.values().all(|call| {
            matches!(
                call.status,
                ToolCallStatus::Completed | ToolCallStatus::Failed
            )
        })
    }

    fn search_kind(&self) -> Result<ToolKind> {
        ensure!(
            self.search_enabled && self.phase == Phase::Prompt,
            "本轮未授权原生搜索工具"
        );
        match self.host {
            Host::Omp => Ok(ToolKind::Fetch),
            Host::Gemini => Ok(ToolKind::Search),
            Host::Pi => Ok(ToolKind::Other),
            _ => bail!("该宿主尚未核实受控搜索契约"),
        }
    }

    fn start_search(&mut self, call: ToolCall) -> Result<()> {
        let kind = self.search_kind()?;
        ensure!(call.kind == kind, "Agent调用非搜索原生工具");
        identifier(&call.tool_call_id.to_string())?;
        ensure!(
            !self.seen_searches.contains(&call.tool_call_id),
            "搜索工具ID重放"
        );
        ensure!(
            call.locations.is_empty() && search_content(&call.content) && call.raw_output.is_none(),
            "搜索工具携带文件/终端或提前结果"
        );
        match self.host {
            Host::Omp => {
                ensure!(
                    call.status == ToolCallStatus::Pending,
                    "OMP搜索初始状态无效"
                );
                let input = call
                    .raw_input
                    .as_ref()
                    .and_then(serde_json::Value::as_object)
                    .context("OMP搜索缺少参数对象")?;
                ensure!(
                    input
                        .get("query")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|q| !q.trim().is_empty()),
                    "OMP搜索缺少查询"
                );
                for (key, value) in input {
                    let valid = match key.as_str() {
                        "query" => value.is_string(),
                        "recency" => {
                            matches!(value.as_str(), Some("day" | "week" | "month" | "year"))
                        }
                        "limit" | "max_tokens" | "temperature" | "num_search_results" => {
                            value.is_number()
                        }
                        _ => false,
                    };
                    ensure!(valid, "OMP搜索含未授权参数");
                }
            }
            Host::Gemini => {
                ensure!(
                    call.status == ToolCallStatus::InProgress && call.raw_input.is_none(),
                    "Gemini搜索初始状态或参数形态无效"
                );
                ensure!(
                    call.title
                        .strip_prefix("Searching the web for: \"")
                        .and_then(|title| title.strip_suffix('"'))
                        .is_some_and(|query| !query.trim().is_empty()),
                    "Gemini搜索工具形态不匹配"
                );
            }
            Host::Pi => {
                ensure!(
                    call.title == "web_search" && self.searches.is_empty(),
                    "Pi仅允许每轮一次受控web_search"
                );
                ensure!(
                    matches!(
                        call.status,
                        ToolCallStatus::Pending | ToolCallStatus::InProgress
                    ),
                    "Pi搜索初始状态无效"
                );
                pi_search_input(
                    call.raw_input.as_ref(),
                    call.status != ToolCallStatus::Pending,
                )?;
            }
            _ => unreachable!(),
        }
        // ACP has no stable programmatic tool name here. The native registry/policy
        // is the pre-execution boundary; these checks only admit its known wire shape.
        self.seen_searches.insert(call.tool_call_id.clone());
        self.searches.insert(
            call.tool_call_id,
            SearchCall {
                title: call.title,
                input: call.raw_input,
                status: call.status,
            },
        );
        // A search is the only permitted assistant-message boundary inside a round.
        // Never concatenate pre-tool commentary with the eventual candidate JSON.
        if let Some(id) = self.message.take() {
            self.seen.insert(id);
        }
        self.identity_seen = false;
        self.text.clear();
        Ok(())
    }

    fn update_search(&mut self, update: ToolCallUpdate) -> Result<()> {
        let kind = self.search_kind()?;
        let call = self
            .searches
            .get_mut(&update.tool_call_id)
            .context("未知或跨轮搜索工具update")?;
        ensure!(
            matches!(
                call.status,
                ToolCallStatus::Pending | ToolCallStatus::InProgress
            ),
            "搜索结束后收到update"
        );
        let mut fields = update.fields;
        // Pi streams incomplete arguments while pending; freeze them at execution start.
        if self.host == Host::Pi && call.status == ToolCallStatus::Pending {
            let input = fields.raw_input.as_ref().or(call.input.as_ref());
            pi_search_input(
                input,
                fields.status.is_some_and(|s| s != ToolCallStatus::Pending),
            )?;
            if fields.raw_input.is_some() {
                call.input = fields.raw_input.take();
            }
        }
        ensure!(
            fields.kind.is_none_or(|value| value == kind)
                && fields
                    .title
                    .as_ref()
                    .is_none_or(|value| value == &call.title)
                && fields
                    .raw_input
                    .as_ref()
                    .is_none_or(|value| Some(value) == call.input.as_ref()),
            "搜索update尝试更改工具身份或参数"
        );
        ensure!(
            fields.locations.as_ref().is_none_or(Vec::is_empty)
                && fields.content.as_deref().is_none_or(search_content),
            "搜索update携带文件或终端内容"
        );
        if let Some(status) = fields.status {
            ensure!(
                matches!(
                    status,
                    ToolCallStatus::InProgress | ToolCallStatus::Completed | ToolCallStatus::Failed
                ) || (status == ToolCallStatus::Pending && call.status == ToolCallStatus::Pending),
                "搜索工具状态回退或未知"
            );
            // Completed means the call ended, not that grounding returned evidence.
            // Gemini omits grounding here; OMP may return details.error as completed.
            call.status = status;
        }
        Ok(())
    }

    fn update(&mut self, n: SessionNotification) -> Result<()> {
        if let Some(id) = &self.snapshot.session {
            ensure!(id == &n.session_id.to_string(), "跨ACP会话update");
        } else {
            ensure!(self.phase == Phase::New, "会话建立前收到update");
            ensure!(
                self.early.as_ref().is_none_or(|s| s == &n.session_id),
                "多个新会话ID"
            );
            self.early = Some(n.session_id);
        }
        match n.update {
            SessionUpdate::ConfigOptionUpdate(u) => {
                self.snapshot.options.replace_config(u.config_options)?;
                self.publish();
            }
            SessionUpdate::CurrentModeUpdate(u) if self.snapshot.options.config.is_none() => {
                if let Some((value, _)) = &mut self.snapshot.options.modes {
                    *value = u.current_mode_id.to_string();
                    self.publish();
                }
            }
            SessionUpdate::AgentMessageChunk(c) => {
                if matches!(self.phase, Phase::Load | Phase::New) {
                    return Ok(());
                }
                ensure!(self.phase == Phase::Prompt, "终态后/恢复期间迟到正文");
                let ContentBlock::Text(text) = c.content else {
                    bail!("候选只接收文本");
                };
                if self.host == Host::Pi
                    && c.meta
                        .as_ref()
                        .and_then(|meta| meta.get("piAcp"))
                        .and_then(|value| value.pointer("/notify/level"))
                        .and_then(serde_json::Value::as_str)
                        == Some("error")
                {
                    let preview: String = text.text.chars().take(160).collect();
                    bail!("Pi原生运行失败：{preview:?}");
                }
                ensure!(self.searches_finished(), "搜索未结束就收到候选正文");
                if self.identity_seen {
                    ensure!(self.message == c.message_id, "不同ACP消息不能拼接");
                } else {
                    ensure!(
                        c.message_id
                            .as_ref()
                            .is_none_or(|id| !self.seen.contains(id)),
                        "ACP消息ID重放"
                    );
                    self.message = c.message_id;
                    self.identity_seen = true;
                }
                ensure!(
                    self.text.len() + text.text.len() <= TEXT_LIMIT,
                    "正文超过256KiB"
                );
                self.text.push_str(&text.text);
            }
            SessionUpdate::ToolCall(call) => self.start_search(call)?,
            SessionUpdate::ToolCallUpdate(update) => self.update_search(update)?,
            _ => {}
        }
        Ok(())
    }
}
type Shared = Arc<Mutex<State>>;
fn state(shared: &Shared) -> std::sync::MutexGuard<'_, State> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn sdk_error(e: impl std::fmt::Display) -> sdk::Error {
    sdk::Error::internal_error().data(format!("{e:#}"))
}
fn failure(shared: &Shared) -> Result<()> {
    if let Some(e) = &state(shared).failure {
        bail!("{e}");
    }
    Ok(())
}
async fn limited<T>(
    future: impl Future<Output = Result<T, sdk::Error>>,
    cancel: &mut watch::Receiver<u64>,
    stop: &mut watch::Receiver<bool>,
) -> Result<T> {
    ensure!(!*stop.borrow(), "ACP连接正在清退");
    cancel.borrow_and_update();
    tokio::select! {biased;_=cancel.changed()=>bail!("ACP操作已取消"),_=stop.changed()=>bail!("ACP操作已停止"),r=tokio::time::timeout(RPC_TIMEOUT,future)=>r.context("ACP请求超时，未自动重试")?.map_err(|e|anyhow::anyhow!("ACP请求错误{}；服务端敏感详情不转印",e.code))}
}
async fn new_session(
    cx: &ConnectionTo<Agent>,
    shared: &Shared,
    root: &Path,
    settings: &Settings,
    cancel: &mut watch::Receiver<u64>,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    {
        let mut s = state(shared);
        s.phase = Phase::New;
        s.snapshot.session = None;
        s.snapshot.options = Options::default();
        s.early = None;
        s.seen.clear();
    }
    let mut request = NewSessionRequest::new(root.to_path_buf());
    request.meta = hosts::session_meta(settings);
    let response = limited(cx.send_request(request).block_task(), cancel, stop).await?;
    let mut s = state(shared);
    identifier(&response.session_id.to_string())?;
    ensure!(
        s.early.as_ref().is_none_or(|id| id == &response.session_id),
        "新会话回执归属错误"
    );
    s.snapshot.session = Some(response.session_id.to_string());
    s.snapshot
        .options
        .setup(response.config_options, response.modes)?;
    hosts::validate_initial_options(
        settings.host,
        s.initial_options.as_ref(),
        &s.snapshot.options,
    )?;
    s.phase = Phase::Idle;
    s.publish();
    Ok(())
}
async fn close_session(cx: &ConnectionTo<Agent>, shared: &Shared) -> Result<()> {
    let id = {
        let mut s = state(shared);
        s.phase = Phase::Close;
        if s.snapshot.capabilities.session_capabilities.close.is_none() {
            return Ok(());
        }
        s.snapshot.session.clone()
    };
    if let Some(id) = id {
        tokio::time::timeout(
            RPC_TIMEOUT,
            cx.send_request(CloseSessionRequest::new(id)).block_task(),
        )
        .await??;
    }
    Ok(())
}
async fn prompt(
    cx: &ConnectionTo<Agent>,
    shared: &Shared,
    p: &Prompt,
    events: &mpsc::Sender<Event>,
    cancel: &mut watch::Receiver<u64>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Answer> {
    ensure!(
        !*stop.borrow() && *cancel.borrow_and_update() == p.cancel_epoch,
        "排队prompt已取消，未请求模型"
    );
    let session = {
        let mut s = state(shared);
        s.phase = Phase::Prompt;
        s.text.clear();
        s.message = None;
        s.identity_seen = false;
        s.searches.clear();
        s.snapshot.session.clone().context("缺少ACP会话")?
    };
    let request = cx.send_request(PromptRequest::new(
        session.clone(),
        vec![ContentBlock::Text(TextContent::new(p.text.clone()))],
    ));
    events
        .send(Event::Started {
            round_id: p.round_id.clone(),
        })
        .await?;
    let response = request.block_task();
    tokio::pin!(response);
    let mut cancelled = false;
    let mut deadline = tokio::time::Instant::now() + PROMPT_TIMEOUT;
    let response = loop {
        tokio::select! {biased;
            _=cancel.changed(),if !cancelled=>{cancelled=true;cx.send_notification(CancelNotification::new(session.clone()))?;deadline=tokio::time::Instant::now()+CANCEL_TIMEOUT;},
            _=stop.changed(),if !cancelled=>{cancelled=true;cx.send_notification(CancelNotification::new(session.clone()))?;deadline=tokio::time::Instant::now()+CANCEL_TIMEOUT;},
            _=tokio::time::sleep_until(deadline)=>bail!(if cancelled{"取消未在宽限内收敛，清退自有进程"}else{"ACP prompt超时，未自动重试"}),
            _=tokio::time::sleep(Duration::from_millis(20)),if !cancelled=>{if state(shared).failure.is_some(){cancelled=true;cx.send_notification(CancelNotification::new(session.clone()))?;deadline=tokio::time::Instant::now()+CANCEL_TIMEOUT;}},
            result=&mut response=>break result.map_err(|e|anyhow::anyhow!("ACP prompt错误{}",e.code))?
        }
    };
    failure(shared)?;
    ensure!(
        !cancelled && response.stop_reason == StopReason::EndTurn,
        "prompt取消或非完整终态，丢弃正文并退役连接"
    );
    let mut s = state(shared);
    ensure!(
        s.searches_finished(),
        "prompt结束时仍有未完成搜索，丢弃正文"
    );
    let result = if !s.identity_seen && s.text.trim().is_empty() {
        // A complete turn with no body is not proof of a deliberate no-reply decision.
        // Keep the connection usable, but distinguish it in the round audit.
        Ok(Answer::NoMessage)
    } else {
        match candidates(&s.text, &p.round_id, &p.message_ids) {
            Ok(candidates) => Ok(Answer::Candidates(candidates)),
            // A fully ended, correctly scoped turn may contain invalid domain targets.
            // Reject the entire batch; no candidate is admitted and no retry is requested.
            Err(error) if error.is::<InvalidCandidateTarget>() => {
                Ok(Answer::Rejected(format!("{error:#}")))
            }
            Err(error) => Err(error),
        }
    };
    if let Some(id) = s.message.take() {
        s.seen.insert(id);
    }
    s.text.clear();
    s.phase = Phase::Idle;
    result
}
async fn configure(
    cx: &ConnectionTo<Agent>,
    shared: &Shared,
    root: &Path,
    settings: &Settings,
    c: &Change,
    cancel: &mut watch::Receiver<u64>,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    state(shared).snapshot.options.validate_change(c)?;
    let boundary = hosts::permit_option(settings.host, c.id.as_deref(), &c.value)?;
    ensure!(!boundary || c.new_context, "换模型必须明确新上下文");
    if c.new_context {
        close_session(cx, shared).await?;
        new_session(cx, shared, root, settings, cancel, stop).await?;
        state(shared).snapshot.options.validate_change(c)?;
    }
    let session = {
        let mut s = state(shared);
        s.phase = Phase::Configure;
        s.snapshot.session.clone().context("缺少ACP会话")?
    };
    if let Some(id) = &c.id {
        let response = limited(
            cx.send_request(SetSessionConfigOptionRequest::new(
                session,
                id.clone(),
                c.value.clone(),
            ))
            .block_task(),
            cancel,
            stop,
        )
        .await?;
        state(shared)
            .snapshot
            .options
            .replace_config(response.config_options)?;
    } else {
        let id = c.value.as_value_id().context("旧mode只接受ID")?;
        limited(
            cx.send_request(SetSessionModeRequest::new(session, id.to_string()))
                .block_task(),
            cancel,
            stop,
        )
        .await?;
    }
    failure(shared)?;
    let mut s = state(shared);
    s.publish();
    ensure!(
        s.snapshot.options.matches(c),
        "Agent回执未报告请求值，未视为生效"
    );
    s.phase = Phase::Idle;
    s.snapshot.restoration_error = None;
    s.publish();
    Ok(())
}
async fn restore_preferences(
    cx: &ConnectionTo<Agent>,
    shared: &Shared,
    root: &Path,
    settings: &Settings,
    cancel: &mut watch::Receiver<u64>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Option<String>> {
    let Some(saved) = settings.native_preferences() else {
        return Ok(None);
    };
    // Model first: its acknowledgement refreshes the dependent thinking choices.
    let (model, thinking) = hosts::preference_ids(settings.host);
    for (id, value) in [(model, &saved.model), (thinking, &saved.thinking)] {
        let Some(value) = value else {
            continue;
        };
        let Some(id) = id else {
            return Ok(Some(
                "该宿主未提供可安全恢复的原生选项；未覆盖保存值，请重新选择宿主或参数".into(),
            ));
        };
        let mut change = Change {
            id: Some(id.into()),
            value: SettingValue::ValueId {
                value: value.clone().into(),
            },
            new_context: false,
        };
        let valid = state(shared)
            .snapshot
            .options
            .validate_change(&change)
            .and_then(|()| {
                hosts::permit_option(settings.host, change.id.as_deref(), &change.value)
            });
        match valid {
            Ok(boundary) => change.new_context = boundary,
            Err(error) => {
                return Ok(Some(format!(
                    "保存的{id}={value}当前不可用：{error}；请在原生选项中重新选择，未覆盖保存值"
                )));
            }
        }
        if !state(shared).snapshot.options.matches(&change) {
            configure(cx, shared, root, settings, &change, cancel, stop).await?;
        }
    }
    Ok(None)
}
async fn ingress(
    stdout: tokio::process::ChildStdout,
    send: impl Fn(TransportFrame) -> Result<()>,
) -> Result<()> {
    let mut lines = FramedRead::new(stdout, LinesCodec::new_with_max_length(FRAME_LIMIT));
    let mut bytes = 0;
    let mut count = 0;
    while let Some(line) = lines.next().await {
        let line = line.context("ACP行超过256KiB或非UTF8")?;
        bytes += line.len();
        count += 1;
        ensure!(
            bytes <= CONNECTION_BYTES && count <= CONNECTION_FRAMES,
            "ACP连接输入预算耗尽，SDK入队前中止"
        );
        let frame = TransportFrame::parse_json(&line);
        ensure!(
            matches!(frame, TransportFrame::Single(_)),
            "ACP损坏帧或不支持的批量帧，拒绝继续"
        );
        send(frame)?;
        // Yield between frames; aggregate lifetime budgets still bound scheduler/consumer stalls.
        tokio::task::yield_now().await;
    }
    bail!("ACP stdout EOF，不能视为空候选")
}
async fn egress(
    stdin: tokio::process::ChildStdin,
    mut rx: impl futures_util::Stream<Item = TransportFrame> + Unpin,
) -> Result<()> {
    let mut lines = FramedWrite::new(stdin, LinesCodec::new_with_max_length(FRAME_LIMIT));
    let mut total = 0;
    while let Some(frame) = rx.next().await {
        let line = frame.to_json()?;
        total += line.len();
        ensure!(
            line.len() <= FRAME_LIMIT && total <= CONNECTION_BYTES,
            "ACP输出预算耗尽"
        );
        tokio::time::timeout(Duration::from_secs(5), lines.send(line))
            .await
            .context("ACP写入超时")??;
    }
    Ok(())
}
async fn stderr_budget(mut stderr: tokio::process::ChildStderr) -> Result<()> {
    let mut buffer = [0; 4096];
    let mut total = 0;
    loop {
        let n = stderr.read(&mut buffer).await?;
        if n == 0 {
            return std::future::pending().await;
        }
        total += n;
        ensure!(
            total <= 2 * 1024 * 1024,
            "Agent stderr超过2MiB；未转印潜在敏感日志"
        );
    }
}
async fn run(
    settings: Settings,
    root: Workspace,
    restore: Option<Restore>,
    mut actions: mpsc::Receiver<Action>,
    events: &mpsc::Sender<Event>,
    publish: watch::Sender<Snapshot>,
    control: Control,
) -> Result<()> {
    let Control {
        mut cancel,
        mut stop,
    } = control;
    // Native discovery never receives the editable project root.
    let cwd = root.runtime.path();
    let (mut command, initial_options) = hosts::command(&settings, cwd)?;
    let mut child = command
        .current_dir(cwd)
        .spawn()
        .context("无法启动原生ACP入口")?;
    let mut group = OwnedGroup(i32::try_from(child.id().context("无PID")?)?);
    let stdout = child.stdout.take().context("无stdout")?;
    let stdin = child.stdin.take().context("无stdin")?;
    let stderr = child.stderr.take().context("无stderr")?;
    let (client, physical) = Channel::duplex();
    let (input_tx, output_rx) = (physical.tx, physical.rx);
    let incoming = move |frame| input_tx.unbounded_send(frame).context("SDK输入通道关闭");
    let shared = Arc::new(Mutex::new(State {
        host: settings.host,
        initial_options,
        search_enabled: settings.web_search,
        searches: HashMap::new(),
        seen_searches: HashSet::new(),
        snapshot: Snapshot::default(),
        publish,
        phase: Phase::Setup,
        text: String::new(),
        message: None,
        identity_seen: false,
        seen: HashSet::new(),
        early: None,
        failure: None,
    }));
    let updates = shared.clone();
    let permissions = shared.clone();
    let client_run=Client.builder().name("shisui-danmu")
        .on_receive_notification(async move |n:SessionNotification,_|{let mut s=state(&updates);if let Err(e)=s.update(n){s.failure=Some(e.to_string());}Ok(())},sdk::on_receive_notification!())
        .on_receive_request(async move |_r:RequestPermissionRequest,responder,_|{responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled))?;state(&permissions).failure=Some("原生工具权限请求已拒绝；不提交本轮".into());Ok(())},sdk::on_receive_request!())
        .connect_with(client,async |cx:ConnectionTo<Agent>| {
            let operation=async {
                let caps=ClientCapabilities::new().session(ClientSessionCapabilities::new().config_options(SessionConfigOptionsCapabilities::new().boolean(BooleanConfigOptionCapabilities::new())));
                let init=limited(cx.send_request(InitializeRequest::new(ProtocolVersion::V1).client_capabilities(caps)).block_task(),&mut cancel,&mut stop).await?;
                ensure!(init.protocol_version==ProtocolVersion::V1,"未协商ACP v1");
                let info=init.agent_info.context("Agent未报告实现身份")?;
                {let mut s=state(&shared);s.snapshot.agent=label(&info.name);s.snapshot.version=label(&info.version);s.publish();}
                hosts::verify_identity(settings.host,&info).with_context(||format!("Agent实际报告：{} {}",label(&info.name),label(&info.version)))?;
                {let mut s=state(&shared);s.snapshot.capabilities=init.agent_capabilities;
                    ensure!(init.auth_methods.len()<=16,"认证选项过多");s.snapshot.authentication=init.auth_methods.iter().map(|a|format!("{}（未自动认证）",label(a.name()))).collect();s.publish();}
                if let Some(previous)=restore {
                    identifier(&previous.session)?;let now=state(&shared).snapshot.capabilities.clone();state(&shared).snapshot.session=Some(previous.session.clone());
                    if now.session_capabilities.resume.is_some() && previous.capabilities.session_capabilities.resume.is_some(){
                        state(&shared).phase=Phase::Resume;
                        let mut request = ResumeSessionRequest::new(previous.session, cwd.to_path_buf());
                        request.meta = hosts::session_meta(&settings);
                        let r=limited(cx.send_request(request).block_task(),&mut cancel,&mut stop).await?;
                        state(&shared).snapshot.options.setup(r.config_options,r.modes)?;
                    } else if now.load_session&&previous.capabilities.load_session {
                        state(&shared).phase=Phase::Load;
                        let mut request = LoadSessionRequest::new(previous.session, cwd.to_path_buf());
                        request.meta = hosts::session_meta(&settings);
                        let r=limited(cx.send_request(request).block_task(),&mut cancel,&mut stop).await?;
                        state(&shared).snapshot.options.setup(r.config_options,r.modes)?;
                    }
                    else{bail!("新旧Agent未共同声明恢复能力，须明确新上下文");}
                }else{new_session(&cx,&shared,cwd,&settings,&mut cancel,&mut stop).await?;}
                let restoration_error = restore_preferences(&cx, &shared, cwd, &settings, &mut cancel, &mut stop).await?;
                state(&shared).snapshot.restoration_error = restoration_error;
                failure(&shared)?;{let mut s=state(&shared);s.phase=Phase::Idle;s.snapshot.connected=true;s.publish();}events.send(Event::Ready).await?;
                let mut context: Option<serde_json::Value> = None;
                let mut context_session = state(&shared).snapshot.session.clone();
                loop {
                    failure(&shared)?;if *stop.borrow(){break;}
                    let action=tokio::select!{biased;_=stop.changed()=>break,_=cx.incoming_closed()=>bail!("SDK连接关闭"),a=actions.recv()=>{let Some(a)=a else{break;};a},_=tokio::time::sleep(Duration::from_millis(20))=>continue};
                    match action {
                        Action::Prompt(mut p)=>{
                            let result = async {
                                if context_session != state(&shared).snapshot.session { context = None; }
                                let mut payload: serde_json::Value = serde_json::from_str(&p.text)?;
                                if let Some(next) = payload.get("operator_context").cloned() {
                                    if context.as_ref().is_some_and(|previous| previous != &next) {
                                        // ACP has no replace-context operation. Retire the old
                                        // session before introducing edited operator documents.
                                        let mut restored = settings.clone();
                                        let mut preferences = super::settings::NativePreferences::scope(&settings);
                                        preferences.capture(&state(&shared).snapshot.options);
                                        restored.remember_native(preferences);
                                        close_session(&cx, &shared).await?;
                                        new_session(&cx, &shared, cwd, &restored, &mut cancel, &mut stop).await?;
                                        if let Some(error) = restore_preferences(&cx, &shared, cwd, &restored, &mut cancel, &mut stop).await? { bail!("编辑后的上下文参数恢复失败：{error}"); }
                                    } else if context.as_ref() == Some(&next) {
                                        payload.as_object_mut().context("轮次必须是JSON对象")?.remove("operator_context");
                                        payload["operator_context_unchanged"] = true.into();
                                        if let Some(contract) = payload.get_mut("contract").and_then(serde_json::Value::as_object_mut) {
                                            contract.remove("instruction");
                                        }
                                    }
                                    context = Some(next);
                                    context_session = state(&shared).snapshot.session.clone();
                                    let mut bytes = Vec::new();
                                    serde::Serialize::serialize(&payload, &mut serde_json::Serializer::with_formatter(&mut bytes, super::InputJson))?;
                                    p.text = String::from_utf8(bytes)?;
                                }
                                prompt(&cx,&shared,&p,events,&mut cancel,&mut stop).await
                            }.await;
                            let failed=result.is_err();events.send(Event::Answer{round_id:p.round_id,result:result.map_err(|e|format!("{e:#}"))}).await?;ensure!(!failed,"prompt未完整结束，旧连接退役");
                        },
                        Action::Configure(c)=>{let result=configure(&cx,&shared,cwd,&settings,&c,&mut cancel,&mut stop).await;let failed=result.is_err();events.send(Event::Configured(result.map_err(|e|format!("{e:#}")))).await?;ensure!(!failed,"配置未收敛，旧连接退役");}
                    }
                }Ok::<(),anyhow::Error>(())
            }.await;
            let _=tokio::time::timeout(Duration::from_secs(1),close_session(&cx,&shared)).await;
            operation.map_err(sdk_error)
        });
    let result = tokio::select! {r=client_run=>r.map_err(anyhow::Error::from),r=ingress(stdout,incoming)=>r,r=egress(stdin,output_rx)=>r,r=stderr_budget(stderr)=>r};
    {
        let mut s = state(&shared);
        s.snapshot.connected = false;
        s.publish();
    }
    // Dropping the SDK/transport futures closes stdin. Give only our child a short exit window.
    let _ = tokio::time::timeout(Duration::from_millis(300), child.wait()).await;
    group.stop();
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

#[cfg(all(test, unix))]
#[path = "search_tests.rs"]
mod search_tests;
