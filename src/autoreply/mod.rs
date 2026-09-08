//! Event-driven reply actor. It sees bounded public messages, never application credentials or tools.
pub mod codex;
pub mod provider;
use crate::{
    bilibili::{SEND_SEGMENT_LIMIT, segment_message},
    delivery::Permit,
    domain::{DanmuEvent, DanmuEventKind, DanmuEventOrigin},
    persistence::SessionJournal,
};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub provider: Provider,
    pub mode: Mode,
    pub codex: codex::Config,
    pub model: Option<provider::Model>,
    pub search: Option<provider::Search>,
    pub token_budget: u64,
    pub request_token_ceiling: u64,
    pub request_budget: u64,
    pub search_budget: u64,
    pub cost_budget: Option<u64>,
    pub global_interval_seconds: u64,
    pub person_interval_seconds: u64,
    pub candidate_seconds: u64,
    pub cache_seconds: u64,
    pub faq: Vec<Faq>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: Provider::Chatgpt,
            mode: Mode::Suggest,
            codex: codex::Config::default(),
            model: None,
            search: None,
            token_budget: 32_000,
            request_token_ceiling: 8_000,
            request_budget: 12,
            search_budget: 3,
            cost_budget: None,
            global_interval_seconds: 10,
            person_interval_seconds: 60,
            candidate_seconds: 45,
            cache_seconds: 120,
            faq: vec![],
        }
    }
}
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Chatgpt,
    Api,
}
impl Config {
    pub fn model_label(&self) -> String {
        match self.provider {
            Provider::Chatgpt => format!(
                "ChatGPT订阅 · {} · {}",
                self.codex.model.as_deref().unwrap_or("尚未选择账号模型"),
                self.codex.effort.as_deref().unwrap_or("尚未选择思考强度")
            ),
            Provider::Api => format!(
                "API · {}",
                self.model
                    .as_ref()
                    .map(|m| m.id.as_str())
                    .unwrap_or("尚未配置endpoint/model/专用凭据")
            ),
        }
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Faq {
    pub question: String,
    pub answer: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub title: String,
    pub url: url::Url,
    pub excerpt: String,
    pub published_at: Option<DateTime<Utc>>,
    pub retrieved_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Suggest,
    Approve,
    Auto,
    Paused,
}
impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Suggest => "建议",
            Self::Approve => "人工批准",
            Self::Auto => "低风险自动",
            Self::Paused => "暂停",
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gate {
    pub session: String,
    pub live: bool,
    pub busy: bool,
    pub broadcaster: String,
}
#[derive(Debug, Clone)]
pub struct Candidate {
    pub key: String,
    pub segments: Vec<String>,
    pub sources: Vec<Source>,
    pub permit: Permit,
    pub automatic: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub reserved_tokens: u64,
    pub reported_tokens: u64,
    pub requests: u64,
    pub searches: u64,
    pub reserved_cost: u64,
}
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub generation: u64,
    pub enabled: bool,
    pub mode: Mode,
    pub model: String,
    pub reason: String,
    pub candidate: Option<Candidate>,
    pub usage: Usage,
    pub sources: Vec<Source>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            generation: 0,
            enabled: false,
            mode: Mode::Suggest,
            model: "ChatGPT订阅 · 尚未登录并选择账号模型；API可独立配置".into(),
            reason: "等待新事件；/ai approve-mode /ai approve /ai discard /ai pause".into(),
            candidate: None,
            usage: Usage::default(),
            sources: vec![],
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum State {
    Observed,
    Ignored,
    Candidate,
    Claimed,
    Accepted,
    Sent,
    Uncertain,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    pub state: State,
    pub reason: String,
    pub usage: Usage,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub sources: Vec<Source>,
}

enum Command {
    Configure(u64, Box<Config>),
    Gate(Gate),
    Observe(DanmuEvent, Permit),
    Mode(Mode),
    Approve,
    Discard,
    Delivery(String, State),
}
pub struct Handle {
    tx: mpsc::Sender<Command>,
    pub view: watch::Receiver<Snapshot>,
    pub generation: u64,
    pub faulted: bool,
    pub ready: mpsc::Receiver<Candidate>,
    permit: Permit,
    gate: Gate,
}
impl Handle {
    pub fn start(config: Config, journal: SessionJournal, journal_session: String) -> Self {
        let (tx, rx) = mpsc::channel(64);
        let (view_tx, view) = watch::channel(Snapshot::default());
        let (ready_tx, ready) = mpsc::channel(1);
        tokio::spawn(Actor::new(config, journal, journal_session, view_tx, ready_tx).run(rx));
        Self {
            tx,
            view,
            ready,
            permit: Permit::new(Utc::now() + chrono::Duration::days(365)),
            gate: Gate::default(),
            generation: 0,
            faulted: false,
        }
    }
    fn send(&mut self, command: Command) {
        if self.faulted {
            return;
        }
        if self.tx.try_send(command).is_err() {
            self.permit.cancel();
            self.faulted = true;
        }
    }
    fn invalidate(&mut self) {
        self.permit.cancel();
        self.permit = Permit::new(Utc::now() + chrono::Duration::days(365));
    }
    pub fn set_gate(&mut self, gate: Gate) {
        if self.gate != gate {
            self.invalidate();
            self.gate = gate.clone();
            self.send(Command::Gate(gate));
        }
    }
    pub fn observe(&mut self, event: DanmuEvent) {
        if self.faulted {
            return;
        }
        if !self.permit.valid() {
            self.invalidate();
        }
        if event.origin == DanmuEventOrigin::Live
            && ((event.author_id.as_deref() == Some(&self.gate.broadcaster)
                && !event.content.starts_with('✦'))
                || event.kind == DanmuEventKind::RoomStatus)
        {
            self.invalidate();
            self.send(Command::Discard);
        }
        self.send(Command::Observe(event, self.permit.clone()));
    }
    pub fn configure(&mut self, config: Config) {
        self.invalidate();
        self.generation += 1;
        self.send(Command::Configure(self.generation, Box::new(config)));
    }
    pub fn mode(&mut self, mode: Mode) {
        self.invalidate();
        self.send(Command::Mode(mode));
    }
    pub fn discard(&mut self) {
        self.invalidate();
        self.send(Command::Discard);
    }
    pub fn approve(&mut self) {
        self.send(Command::Approve);
    }
    pub fn delivery(&mut self, key: String, state: State) {
        self.send(Command::Delivery(key, state));
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.permit.cancel();
    }
}

pub fn safe_text(text: &str) -> bool {
    !text.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{2028}' | '\u{2029}' | '\u{feff}'))
}
pub fn sensitive(text: &str) -> bool {
    let value = text.to_lowercase();
    [
        "http:",
        "https:",
        "file:",
        "localhost",
        "127.0.",
        "www.",
        "/users/",
        "密码",
        "密钥",
        "cookie",
        "api_key",
        "bearer ",
        "sk-",
        "账号",
        "system prompt",
        "ignore previous",
        "忽略",
        "执行命令",
        "系统提示",
        "规则变更",
        "扮演",
        "泄露",
        "身份证",
        "手机号",
        "住址",
        "诊断",
        "用药",
        "买哪只",
        "投资建议",
        "起诉",
        "傻逼",
        "诈骗",
        "黑料",
    ]
    .iter()
    .any(|needle| value.contains(needle))
        || value
            .split(|c: char| !c.is_ascii_digit())
            .any(|digits| digits.len() >= 7)
}
fn volatile(text: &str) -> bool {
    [
        "今天",
        "现在",
        "最新",
        "实时",
        "额度",
        "重置",
        "主播经历",
        "你以前",
        "承诺",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}
pub fn reply_segments(text: &str, nickname: &str) -> Result<Vec<String>> {
    ensure!(
        safe_text(text)
            && safe_text(nickname)
            && !sensitive(text)
            && !text.contains("助手")
            && !nickname.contains("助手")
            && !text.contains('✦'),
        "候选含不安全内容"
    );
    // Remove whitespace only between Han characters; preserve English words and emoji ZWJ clusters.
    let mut compact = String::new();
    let chars: Vec<char> = text.trim().chars().collect();
    for (index, c) in chars.iter().copied().enumerate() {
        let han = |v: char| ('\u{3400}'..='\u{9fff}').contains(&v);
        if c.is_whitespace()
            && index > 0
            && index + 1 < chars.len()
            && han(chars[index - 1])
            && han(chars[index + 1])
        {
            continue;
        }
        compact.push(c);
    }
    let nickname: String = nickname.graphemes(true).take(10).collect();
    ensure!(!nickname.contains(['@', '/', '\\', '✦']), "昵称不安全");
    let prefix = if nickname.is_empty() {
        "✦".into()
    } else {
        format!("✦@{nickname} ")
    };
    let budget = SEND_SEGMENT_LIMIT - prefix.graphemes(true).count();
    let parts = segment_message(&compact, budget);
    ensure!(
        !parts.is_empty() && parts.len() <= 3,
        "候选需缩短至三条以内"
    );
    Ok(parts
        .into_iter()
        .map(|part| format!("{prefix}{part}"))
        .collect())
}
fn key(session: &str, event: &DanmuEvent) -> String {
    let identity = event.platform_event_id.as_deref().unwrap_or(&event.id);
    format!(
        "{session}:{}",
        hex::encode(Sha256::digest(identity.as_bytes()))
    )
}
fn local_answer(config: &Config, question: &str) -> Option<String> {
    let normalized = question.trim().trim_end_matches(['?', '？', '!', '！']);
    if matches!(normalized, "你好" | "大家好" | "晚上好" | "早上好") {
        return Some("你好，欢迎一起聊聊。".into());
    }
    config
        .faq
        .iter()
        .find(|faq| faq.question == normalized)
        .map(|faq| faq.answer.clone())
}
#[derive(Clone)]
struct CachedReply {
    at: DateTime<Utc>,
    text: String,
    sources: Vec<Source>,
    automatic: bool,
}
struct Completed {
    key: String,
    candidate: Result<Candidate>,
    tokens: u64,
    cache: Option<(String, CachedReply)>,
}
struct Actor {
    config: Config,
    journal: SessionJournal,
    journal_session: String,
    view: Snapshot,
    views: watch::Sender<Snapshot>,
    ready: mpsc::Sender<Candidate>,
    gate: Gate,
    started: DateTime<Utc>,
    seen: HashSet<String>,
    people: HashMap<String, DateTime<Utc>>,
    last: Option<DateTime<Utc>>,
    cache: HashMap<String, CachedReply>,
    job: Option<tokio::task::JoinHandle<Completed>>,
    active_key: Option<String>,
    active_permit: Option<Permit>,
    claimed: bool,
    journal_failed: bool,
}
impl Actor {
    fn new(
        config: Config,
        journal: SessionJournal,
        journal_session: String,
        views: watch::Sender<Snapshot>,
        ready: mpsc::Sender<Candidate>,
    ) -> Self {
        let view = Snapshot {
            enabled: config.enabled,
            mode: config.mode,
            model: config.model_label(),
            ..Snapshot::default()
        };
        Self {
            config,
            journal,
            journal_session,
            view,
            views,
            ready,
            gate: Gate::default(),
            started: Utc::now(),
            seen: HashSet::new(),
            people: HashMap::new(),
            last: None,
            cache: HashMap::new(),
            job: None,
            active_key: None,
            active_permit: None,
            claimed: false,
            journal_failed: false,
        }
    }
    async fn persist(&mut self, key: String, state: State, reason: &str) -> bool {
        let record = Record {
            key,
            state,
            reason: reason.into(),
            usage: self.view.usage.clone(),
            at: Utc::now(),
            sources: self.view.sources.clone(),
        };
        let journal = self.journal.clone();
        let session = self.journal_session.clone();
        let ok = tokio::task::spawn_blocking(move || journal.reply_record(&session, &record))
            .await
            .is_ok_and(|result| result.is_ok());
        if !ok {
            self.journal_failed = true;
            if let Some(permit) = &self.active_permit {
                permit.cancel();
            }
            self.view.mode = Mode::Paused;
            self.view.reason = "持久日志失败；安全关闭".into();
        }
        ok
    }
    async fn cancel(&mut self, reason: &str) {
        if let Some(job) = self.job.take() {
            job.abort();
        }
        if let Some(permit) = self.active_permit.take() {
            permit.cancel();
        }
        self.view.candidate = None;
        if let Some(key) = self.active_key.take() {
            let state = if self.claimed {
                State::Uncertain
            } else {
                State::Cancelled
            };
            self.persist(key, state, reason).await;
        }
        self.claimed = false;
        if !self.journal_failed {
            self.view.reason = reason.into();
        }
    }
    async fn recover_stream(&mut self, stream: String) {
        self.seen.clear();
        self.people.clear();
        self.cache.clear();
        self.last = None;
        self.view.usage = Usage::default();
        let journal = self.journal.clone();
        match tokio::task::spawn_blocking(move || {
            journal.reply_records_for_stream::<Record>(&stream)
        })
        .await
        {
            Ok(Ok(records)) => {
                for record in records {
                    let cooldown = self
                        .config
                        .person_interval_seconds
                        .saturating_sub(self.config.global_interval_seconds);
                    let at = record.at + chrono::Duration::seconds(cooldown.min(3600) as i64);
                    self.last = Some(self.last.map_or(at, |last| last.max(at)));
                    self.seen.insert(record.key);
                    self.view.usage.reserved_tokens = self
                        .view
                        .usage
                        .reserved_tokens
                        .max(record.usage.reserved_tokens);
                    self.view.usage.reported_tokens = self
                        .view
                        .usage
                        .reported_tokens
                        .max(record.usage.reported_tokens);
                    self.view.usage.requests = self.view.usage.requests.max(record.usage.requests);
                    self.view.usage.searches = self.view.usage.searches.max(record.usage.searches);
                    self.view.usage.reserved_cost = self
                        .view
                        .usage
                        .reserved_cost
                        .max(record.usage.reserved_cost);
                }
            }
            _ => {
                self.journal_failed = true;
                self.view.mode = Mode::Paused;
                self.view.reason = "恢复去重日志失败；安全关闭".into();
            }
        }
    }
    async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        self.views.send_replace(self.view.clone());
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                command = rx.recv() => {
                    let Some(command) = command else { self.cancel("退出取消").await; break; };
                    match command {
                        Command::Configure(generation, config) => {
                            self.view.generation = generation;
                            self.cancel("配置已变更；旧候选与待发已撤销").await;
                            self.cache.clear();
                            self.config = *config;
                            self.view.enabled = self.config.enabled;
                            self.view.model = self.config.model_label();
                            if !self.journal_failed { self.view.mode = self.config.mode; }
                        }
                        Command::Gate(gate) => {
                            self.cancel("场次/连接/人工优先状态变更").await;
                            if self.gate.session != gate.session { self.recover_stream(gate.session.clone()).await; }
                            self.gate = gate;
                        }
                        Command::Mode(mode) => { self.cancel("模式切换；待发已清空").await; if !self.journal_failed { self.view.mode = mode; } }
                        Command::Discard => self.cancel("主播抢答或人工丢弃").await,
                        Command::Approve => if self.view.mode != Mode::Suggest && self.view.mode != Mode::Paused { self.dispatch().await; },
                        Command::Delivery(key, state) => {
                            let current = self.active_key.as_deref() == Some(&key);
                            self.persist(key, state.clone(), "发送服务结果").await;
                            self.view.reason = format!("发送：{state:?}");
                            if current && matches!(state, State::Sent | State::Uncertain | State::Cancelled) { self.active_key = None; self.active_permit = None; self.claimed = false; }
                            if state == State::Uncertain { self.cancel("发送不确定；暂停且不重试").await; self.view.mode = Mode::Paused; }
                        }
                        Command::Observe(event, permit) => self.observe(event, permit).await,
                    }
                    self.views.send_replace(self.view.clone());
                }
                _ = tick.tick() => {
                    if self.active_permit.as_ref().is_some_and(|permit| !permit.valid()) {
                        self.cancel("候选过期或被取消").await; self.views.send_replace(self.view.clone());
                    }
                    if self.job.as_ref().is_some_and(|job| job.is_finished()) {
                        let result = self.job.take().unwrap().await;
                        if let Ok(result) = result { self.complete(result).await; }
                        else { self.cancel("worker异常；安全暂停").await; self.view.mode = Mode::Paused; }
                        self.views.send_replace(self.view.clone());
                    }
                }
            }
        }
    }
    async fn observe(&mut self, event: DanmuEvent, permit: Permit) {
        let now = Utc::now();
        let key = key(&self.gate.session, &event);
        if !self.seen.insert(key.clone()) {
            self.view.reason = "跳过：场次消息已观察".into();
            return;
        }
        if !self
            .persist(key.clone(), State::Observed, "新事件游标")
            .await
        {
            return;
        }
        let question = event.content.trim().to_owned();
        let self_message =
            event.author_id.as_deref() == Some(&self.gate.broadcaster) || question.starts_with('✦');
        let reason = if !self.config.enabled {
            Some("AI总开关已关闭；不生成不检索")
        } else if event.origin != DanmuEventOrigin::Live || event.timestamp < self.started {
            Some("历史/启动前事件不补答")
        } else if event.kind != DanmuEventKind::Danmu || self_message {
            Some("自发/系统/活动事件")
        } else if question.is_empty() || question.eq_ignore_ascii_case("test") || question == "测试"
        {
            Some("空白或test")
        } else if !safe_text(&question)
            || !safe_text(event.username.as_deref().unwrap_or(""))
            || sensitive(&question)
        {
            Some("注入/链接/隐私/高风险留主播")
        } else if question.len() > 400 || event.reply_to.is_some() || question.starts_with('@') {
            Some("过长或观众互聊")
        } else if !self.gate.live
            || self.gate.busy
            || !permit.valid()
            || self.view.mode == Mode::Paused
        {
            Some("非直播/人工优先/暂停")
        } else if now - event.timestamp
            > chrono::Duration::seconds(self.config.candidate_seconds.min(90) as i64)
            || event.timestamp > now + chrono::Duration::seconds(5)
        {
            Some("事件过期或时间异常")
        } else if self.active_key.is_some() {
            Some("已有生成/候选/发送；并发上限1")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.view.reason = format!("跳过：{reason}");
            self.persist(key, State::Ignored, reason).await;
            return;
        }
        let person = event
            .author_id
            .clone()
            .or(event.username.clone())
            .unwrap_or_default();
        if person.is_empty()
            || self.last.is_some_and(|last| {
                (now - last).num_seconds() < self.config.global_interval_seconds.max(1) as i64
            })
            || self.people.get(&person).is_some_and(|last| {
                (now - *last).num_seconds() < self.config.person_interval_seconds.max(1) as i64
            })
        {
            self.view.reason = "跳过：身份缺失或频率限制".into();
            self.persist(key, State::Ignored, "频率限制").await;
            return;
        }
        self.last = Some(now);
        self.people.insert(person, now);
        let permit = permit.child(
            event.timestamp
                + chrono::Duration::seconds(self.config.candidate_seconds.min(90) as i64),
        );
        self.active_key = Some(key.clone());
        self.active_permit = Some(permit.clone());
        let nickname = event.username.unwrap_or_default();
        self.cache.retain(|_, cached| {
            now - cached.at < chrono::Duration::seconds(self.config.cache_seconds.min(300) as i64)
                && cached
                    .sources
                    .iter()
                    .all(|source| now - source.retrieved_at < chrono::Duration::minutes(5))
        });
        let answer = self.cache.get(&question).cloned().or_else(|| {
            local_answer(&self.config, &question).map(|text| CachedReply {
                at: now,
                automatic: !volatile(&question) && !volatile(&text),
                text,
                sources: vec![],
            })
        });
        if let Some(answer) = answer {
            let candidate = reply_segments(&answer.text, &nickname).map(|segments| Candidate {
                key: key.clone(),
                segments,
                sources: answer.sources.clone(),
                permit,
                automatic: answer.automatic,
            });
            let cache = Some((question, answer));
            self.complete(Completed {
                key,
                candidate,
                tokens: 0,
                cache,
            })
            .await;
            return;
        }
        let configured = match self.config.provider {
            Provider::Api => self.config.model.is_some(),
            Provider::Chatgpt => {
                self.config.codex.model.is_some() && self.config.codex.effort.is_some()
            }
        };
        if !configured {
            self.view.reason = format!("留主播：{}", self.config.model_label());
            self.persist(key, State::Ignored, "缺少模型配置").await;
            self.active_key = None;
            return;
        }
        if !question.contains(['?', '？'])
            && !["什么", "怎么", "如何", "为什么"]
                .iter()
                .any(|v| question.contains(v))
        {
            self.persist(key, State::Ignored, "非问题互聊").await;
            self.active_key = None;
            self.view.reason = "跳过：非问题互聊".into();
            return;
        }
        let requests = if self.config.search.is_some() { 2 } else { 1 };
        let searches = u64::from(self.config.search.is_some());
        let reserve = self.config.request_token_ceiling.saturating_mul(requests);
        let cost = self
            .config
            .model
            .as_ref()
            .and_then(|model| model.request_cost_ceiling)
            .and_then(|cost| cost.checked_mul(requests))
            .and_then(|cost| {
                if let Some(search) = &self.config.search {
                    search
                        .request_cost_ceiling
                        .and_then(|s| cost.checked_add(s))
                } else {
                    Some(cost)
                }
            });
        let usage = &self.view.usage;
        if usage.requests.saturating_add(requests) > self.config.request_budget
            || usage.reserved_tokens.saturating_add(reserve) > self.config.token_budget
            || usage.searches.saturating_add(searches) > self.config.search_budget
            || self.config.cost_budget.is_some_and(|budget| {
                cost.is_none_or(|cost| usage.reserved_cost.saturating_add(cost) > budget)
            })
        {
            self.cancel("预算上限或费用不可核算；暂停").await;
            self.view.mode = Mode::Paused;
            return;
        }
        self.view.usage.requests += requests;
        self.view.usage.searches += searches;
        self.view.usage.reserved_tokens += reserve;
        self.view.usage.reserved_cost += cost.unwrap_or(0);
        if !self
            .persist(
                key.clone(),
                State::Observed,
                "调用前保守预留；取消不退还未知消耗",
            )
            .await
        {
            return;
        }
        let config = self.config.clone();
        self.view.reason = "异步生成候选；保留人工优先".into();
        self.job = Some(tokio::spawn(async move {
            let generated = tokio::time::timeout(
                Duration::from_secs(20),
                provider::generate(&config, &question),
            )
            .await;
            let mut tokens = 0;
            let mut cache = None;
            let candidate = match generated {
                Ok(Ok(result)) => {
                    tokens = result.tokens;
                    if (volatile(&question) || volatile(&result.text)) && result.sources.is_empty()
                    {
                        Err(anyhow::anyhow!("实时/经历/额度缺少依据，留主播"))
                    } else {
                        if !result.sources.is_empty() {
                            cache = Some((
                                question.clone(),
                                CachedReply {
                                    at: Utc::now(),
                                    text: result.text.clone(),
                                    sources: result.sources.clone(),
                                    automatic: false,
                                },
                            ));
                        }
                        reply_segments(&result.text, &nickname).map(|segments| Candidate {
                            key: key.clone(),
                            segments,
                            sources: result.sources,
                            permit,
                            automatic: false,
                        })
                    }
                }
                Ok(Err(error)) => Err(error),
                Err(_) => Err(anyhow::anyhow!("生成/检索超时；不重试")),
            };
            Completed {
                key,
                candidate,
                tokens,
                cache,
            }
        }));
    }
    async fn complete(&mut self, result: Completed) {
        self.view.usage.reported_tokens += result.tokens;
        match result.candidate {
            Ok(candidate)
                if candidate.permit.valid()
                    && self.config.enabled
                    && self.gate.live
                    && !self.gate.busy
                    && self.view.mode != Mode::Paused =>
            {
                self.view.sources = candidate.sources.clone();
                if !self.persist(result.key, State::Candidate, "候选就绪").await {
                    return;
                }
                if let Some((question, cached)) = result.cache {
                    self.cache.insert(question, cached);
                }
                self.view.reason = if candidate.automatic {
                    "本地低风险候选"
                } else {
                    "模型候选：需人工核验批准"
                }
                .into();
                self.view.candidate = Some(candidate);
                if self.view.mode == Mode::Auto
                    && self.view.candidate.as_ref().is_some_and(|c| c.automatic)
                {
                    self.dispatch().await;
                }
            }
            Ok(_) => self.cancel("返回后过期/抢答/下播；取消").await,
            Err(error) => {
                self.persist(result.key, State::Ignored, "生成失败/需主播")
                    .await;
                self.cancel(&error.to_string()).await;
                self.view.mode = Mode::Paused;
            }
        }
    }
    async fn dispatch(&mut self) {
        let Some(candidate) = self.view.candidate.take() else {
            return;
        };
        if !self.config.enabled
            || self.view.mode == Mode::Suggest
            || self.view.mode == Mode::Paused
            || !candidate.permit.valid()
            || !self.gate.live
            || self.gate.busy
        {
            self.cancel("发送前复检取消").await;
            return;
        }
        // Durable claim precedes enqueue: a crash may lose a reply, but can never replay it.
        if !self
            .persist(
                candidate.key.clone(),
                State::Claimed,
                "持久认领；重启不补发",
            )
            .await
        {
            candidate.permit.cancel();
            return;
        }
        self.claimed = true;
        if self.ready.try_send(candidate).is_err() {
            self.cancel("发送出口繁忙；安全取消").await;
        } else {
            self.view.reason = "已认领，等待唯一发送服务".into();
        }
    }
}

#[cfg(test)]
mod tests;
