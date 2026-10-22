//! Stateless, single-flight model HTTP transport. Application ownership stays in Runner.
use super::{
    credentials::ApiKey,
    model::{self, Answer, Prompt, RoundOutput, SearchState},
    responses, search,
    settings::{ApiConfig, ApiProtocol, OpenaiTokenLimit, normalize_base_url},
    subscription,
};
use reqwest::{
    Client,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

const RESPONSE_LIMIT: usize = 256 * 1024;
const REQUEST_LIMIT: usize = 48 * 1024;
const CONTEXT_LIMIT: usize = 24 * 1024;
const ROUND_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ApiError {
    #[error("模型HTTP状态：{0}")]
    Http(u16),
    #[error("模型传输错误：{0}")]
    Transport(&'static str),
    #[error("模型协议错误：{0}")]
    Protocol(&'static str),
    #[error("模型配置错误：{0}")]
    Configuration(&'static str),
    #[error("ChatGPT订阅请求失败；HTTP：{status:?}；代码：{code:?}；请求ID：{request_id:?}。{}", subscription_hint(*code))]
    Subscription {
        status: Option<u16>,
        code: Option<&'static str>,
        request_id: Option<String>,
    },
}
fn subscription_hint(code: Option<&str>) -> &'static str {
    match code {
        Some(
            "subscription_sharing_usage_limit_exceeded"
            | "subscription_sharing_usage_unavailable"
            | "subscription_sharing_user_not_eligible",
        ) => {
            "请在 https://chatgpt.com/settings/usage 查看共享额度与应用限制，然后显式重新启动；不会转为API计费"
        }
        Some(
            "invalid_grant"
            | "invalid_refresh_token"
            | "token_expired"
            | "refresh_token_expired"
            | "refresh_token_invalidated"
            | "refresh_token_reused"
            | "invalid_client"
            | "chatpass_v2_scope_not_authorized"
            | "chatpass_v2_invalid_authorization_context",
        ) => "请在模型设置中重新登录并授权ChatGPT订阅；不会转为API计费",
        _ => "请检查ChatGPT账号授权与服务状态，然后显式重新启动；不会转为API计费",
    }
}
impl From<responses::ResponseError> for ApiError {
    fn from(error: responses::ResponseError) -> Self {
        match error {
            responses::ResponseError::Http {
                status,
                code,
                request_id,
            } => Self::Subscription {
                status: Some(status),
                code,
                request_id,
            },
            responses::ResponseError::Failed { code, request_id } => Self::Subscription {
                status: None,
                code,
                request_id,
            },
            responses::ResponseError::Transport(reason) => Self::Transport(reason),
            responses::ResponseError::Protocol(reason) => Self::Protocol(reason),
            responses::ResponseError::Configuration(reason) => Self::Configuration(reason),
        }
    }
}
impl From<subscription::SubscriptionError> for ApiError {
    fn from(error: subscription::SubscriptionError) -> Self {
        match error {
            subscription::SubscriptionError::Http(status) => Self::Subscription {
                status: Some(status),
                code: None,
                request_id: None,
            },
            subscription::SubscriptionError::Refresh(code) => Self::Subscription {
                status: None,
                code: Some(code),
                request_id: None,
            },
            subscription::SubscriptionError::ScopeDisabled => Self::Subscription {
                status: None,
                code: Some("chatpass_v2_scope_not_authorized"),
                request_id: None,
            },
            subscription::SubscriptionError::AccessDenied => {
                Self::Configuration("未授权ChatGPT登录")
            }
            subscription::SubscriptionError::Transport(reason) => Self::Transport(reason),
            subscription::SubscriptionError::Protocol(reason) => Self::Protocol(reason),
            subscription::SubscriptionError::Configuration(reason) => Self::Configuration(reason),
        }
    }
}
struct Backend {
    client: Client,
    subscription: Option<SubscriptionBackend>,
}
struct SubscriptionBackend {
    session: subscription::Session,
    #[cfg(test)]
    endpoint: Option<String>,
}
fn config_subscription(config: &ApiConfig) -> Result<(), ApiError> {
    if config.protocol != ApiProtocol::ChatgptSubscription {
        return Err(ApiError::Configuration("OAuth凭据仅可用于ChatGPT订阅连接"));
    }
    config
        .ready()
        .map_err(|_| ApiError::Configuration("订阅地址、账号或模型无效"))?;
    if normalize_base_url(&config.base_url).ok().as_deref() != Some("https://api.openai.com/v1") {
        return Err(ApiError::Configuration("订阅连接仅允许官方API地址"));
    }
    Ok(())
}
impl Backend {
    async fn request(
        &mut self,
        config: &ApiConfig,
        search: &search::Client,
        prompt: &Prompt,
        deadline: Instant,
        search_state: &mut SearchState,
    ) -> Result<RoundOutput, ApiError> {
        match self.subscription.as_mut() {
            None => request(config, &self.client, search, prompt, deadline, search_state).await,
            Some(backend) => {
                let first = backend.turn(&self.client, config, prompt, None).await?;
                let call = match first.result {
                    responses::ResponseResult::Answer(answer) => {
                        return Ok(RoundOutput {
                            answer,
                            returned_model: first.returned_model,
                            search: SearchState::NotRequested,
                        });
                    }
                    responses::ResponseResult::ToolCall(call) => call,
                };
                // Admission is strict in Responses; still refuse rather than guessing a query if ownership differs.
                let target = prompt
                    .search_targets
                    .iter()
                    .find(|target| target.message_id == call.message_id())
                    .filter(|_| tools_allowed(prompt))
                    .ok_or(ApiError::Protocol("搜索目标归属错误"))?;
                *search_state = SearchState::Unavailable;
                let text = search.search(&target.query, deadline).await;
                if text.is_some() {
                    *search_state = SearchState::Completed;
                }
                let result = text.map_or_else(
                    || SEARCH_UNAVAILABLE.to_owned(),
                    |text| search_result(&target.query, &text),
                );
                let second = backend
                    .turn(
                        &self.client,
                        config,
                        prompt,
                        Some(call.into_continuation(result)),
                    )
                    .await?;
                let answer = match second.result {
                    responses::ResponseResult::Answer(answer) => answer,
                    responses::ResponseResult::ToolCall(_) => {
                        Answer::Rejected("搜索预算已用完".into())
                    }
                };
                Ok(RoundOutput {
                    answer,
                    returned_model: second.returned_model,
                    search: *search_state,
                })
            }
        }
    }
}
impl SubscriptionBackend {
    async fn turn(
        &mut self,
        client: &Client,
        config: &ApiConfig,
        prompt: &Prompt,
        continuation: Option<responses::Continuation>,
    ) -> Result<responses::ResponseTurn, ApiError> {
        let authorization = self.session.header().await.map_err(ApiError::from)?;
        #[cfg(test)]
        if let Some(endpoint) = &self.endpoint {
            return responses::request_with_endpoint(
                client,
                authorization,
                config,
                prompt,
                continuation,
                endpoint,
            )
            .await
            .map_err(ApiError::from);
        }
        responses::request(client, authorization, config, prompt, continuation)
            .await
            .map_err(ApiError::from)
    }
}
#[derive(Debug)]
pub(crate) enum Event {
    Started {
        round_id: String,
    },
    Answer {
        round_id: String,
        result: Result<RoundOutput, ApiError>,
    },
    Closed(Result<(), ApiError>),
}
pub(crate) struct Handle {
    pub(crate) prompts: mpsc::Sender<Prompt>,
    pub(crate) events: mpsc::Receiver<Event>,
    pub(crate) task: JoinHandle<()>,
    cancel: watch::Sender<u64>,
    stop: watch::Sender<bool>,
}
impl Handle {
    /// Validates only local configuration; does not contact the service.
    pub(crate) fn spawn(config: ApiConfig, key: ApiKey) -> anyhow::Result<Self> {
        let search =
            search::Client::new().map_err(|_| ApiError::Configuration("搜索客户端不可用"))?;
        Self::spawn_with_search(config, key, search)
    }
    #[cfg(test)]
    pub(crate) fn spawn_for_test(
        config: ApiConfig,
        key: ApiKey,
        search: search::Client,
    ) -> anyhow::Result<Self> {
        Self::spawn_with_search(config, key, search)
    }
    fn spawn_with_search(
        mut config: ApiConfig,
        key: ApiKey,
        search: search::Client,
    ) -> anyhow::Result<Self> {
        if config.protocol == ApiProtocol::ChatgptSubscription {
            return Err(ApiError::Configuration("订阅连接不接受API Key").into());
        }
        config
            .ready()
            .map_err(|_| ApiError::Configuration("地址、模型或输出上限无效"))?;
        config.base_url = normalize_base_url(&config.base_url)
            .map_err(|_| ApiError::Configuration("API地址无效"))?;
        let url = url::Url::parse(&config.base_url)
            .map_err(|_| ApiError::Configuration("API地址无效"))?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        match config.protocol {
            ApiProtocol::OpenaiChat => {
                let key_header = key.header();
                let mut bytes = Vec::with_capacity(7 + key_header.as_bytes().len());
                bytes.extend_from_slice(b"Bearer ");
                bytes.extend_from_slice(key_header.as_bytes());
                let mut value = HeaderValue::from_bytes(&bytes)
                    .map_err(|_| ApiError::Configuration("Key格式无效"))?;
                value.set_sensitive(true);
                headers.insert(AUTHORIZATION, value);
            }
            ApiProtocol::AnthropicMessages => {
                headers.insert("x-api-key", key.header());
                headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            }
            ApiProtocol::ChatgptSubscription => {
                return Err(ApiError::Configuration("订阅连接不接受API Key").into());
            }
        }
        let mut builder = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10));
        if loopback {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|_| ApiError::Configuration("HTTP客户端不可用"))?;
        Self::spawn_backend(
            config,
            Backend {
                client,
                subscription: None,
            },
            search,
        )
    }
    /// Local setup only. Refresh belongs to the cancellable round, not startup.
    pub(crate) fn spawn_subscription(
        config: ApiConfig,
        session: subscription::Session,
    ) -> anyhow::Result<Self> {
        let search =
            search::Client::new().map_err(|_| ApiError::Configuration("搜索客户端不可用"))?;
        config_subscription(&config)?;
        if config.chatgpt_account.as_deref() != Some(session.account_id()) {
            return Err(ApiError::Configuration("订阅账号与会话绑定不匹配").into());
        }
        let client = responses::client().map_err(ApiError::from)?;
        Self::spawn_backend(
            config,
            Backend {
                client,
                subscription: Some(SubscriptionBackend {
                    session,
                    #[cfg(test)]
                    endpoint: None,
                }),
            },
            search,
        )
    }
    #[cfg(test)]
    pub(crate) fn spawn_subscription_for_test(
        config: ApiConfig,
        session: subscription::Session,
        search: search::Client,
        endpoint: &str,
    ) -> anyhow::Result<Self> {
        config_subscription(&config)?;
        if config.chatgpt_account.as_deref() != Some(session.account_id()) {
            return Err(ApiError::Configuration("订阅账号与会话绑定不匹配").into());
        }
        let url = url::Url::parse(endpoint).map_err(|_| ApiError::Configuration("测试地址无效"))?;
        let loopback = matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
        if url.scheme() != "http"
            || !loopback
            || url.path() != "/v1/responses"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ApiError::Configuration("测试地址必须为loopback Responses端点").into());
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| ApiError::Configuration("HTTP客户端不可用"))?;
        Self::spawn_backend(
            config,
            Backend {
                client,
                subscription: Some(SubscriptionBackend {
                    session,
                    endpoint: Some(endpoint.to_owned()),
                }),
            },
            search,
        )
    }
    fn spawn_backend(
        config: ApiConfig,
        backend: Backend,
        search: search::Client,
    ) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| ApiError::Configuration("异步运行环境不可用"))?;
        let (prompts, receiver) = mpsc::channel(1);
        let (sender, events) = mpsc::channel(8);
        let (cancel, epoch) = watch::channel(0);
        let (stop, stopping) = watch::channel(false);
        let task = runtime.spawn(worker(
            config, backend, search, receiver, sender, epoch, stopping,
        ));
        Ok(Self {
            prompts,
            events,
            task,
            cancel,
            stop,
        })
    }
    pub(crate) fn epoch(&self) -> u64 {
        *self.cancel.borrow()
    }
    pub(crate) fn cancel(&self) {
        self.cancel.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub(crate) fn stop(&self) {
        self.cancel();
        self.stop.send_replace(true);
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
        self.task.abort();
    }
}
fn output(answer: Answer) -> RoundOutput {
    RoundOutput {
        answer,
        returned_model: None,
        search: SearchState::NotRequested,
    }
}
fn interrupted(
    prompt: &Prompt,
    epoch: &watch::Receiver<u64>,
    stop: &watch::Receiver<bool>,
    deadline: Instant,
) -> Option<Answer> {
    if *stop.borrow() || *epoch.borrow() != prompt.cancel_epoch {
        Some(Answer::Cancelled)
    } else if Instant::now() >= deadline {
        Some(Answer::Expired)
    } else {
        None
    }
}
async fn worker(
    config: ApiConfig,
    mut backend: Backend,
    search: search::Client,
    mut prompts: mpsc::Receiver<Prompt>,
    events: mpsc::Sender<Event>,
    mut epoch: watch::Receiver<u64>,
    mut stop: watch::Receiver<bool>,
) {
    // Keep a terminal event slot even when the consumer is not draining during shutdown.
    let Ok(closed) = events.reserve().await else {
        return;
    };
    loop {
        let prompt = tokio::select! {
            biased;
            _ = stop.changed() => break,
            prompt = prompts.recv() => match prompt { Some(prompt) => prompt, None => break },
        };
        if *stop.borrow() {
            break;
        }
        // A previous cancellation must not cancel a new prompt carrying the current epoch.
        epoch.borrow_and_update();
        let deadline = prompt.deadline.min(Instant::now() + ROUND_TIMEOUT);
        let result = if let Some(answer) = interrupted(&prompt, &epoch, &stop, deadline) {
            Ok(output(answer))
        } else {
            // Reserve event capacity before starting: backpressure cannot launch an unobservable request.
            let permit = tokio::select! {
                biased;
                _ = stop.changed() => None,
                _ = epoch.changed() => None,
                _ = tokio::time::sleep_until(deadline.into()) => None,
                permit = events.reserve() => match permit { Ok(permit) => Some(permit), Err(_) => break },
            };
            if let Some(answer) = interrupted(&prompt, &epoch, &stop, deadline) {
                Ok(output(answer))
            } else if let Some(permit) = permit {
                permit.send(Event::Started {
                    round_id: prompt.round_id.clone(),
                });
                let mut search_state = SearchState::NotRequested;
                let result = tokio::select! {
                    biased;
                    _ = stop.changed() => Err(Answer::Cancelled),
                    _ = epoch.changed() => Err(Answer::Cancelled),
                    _ = tokio::time::sleep_until(deadline.into()) => Err(Answer::Expired),
                    result = backend.request(&config, &search, &prompt, deadline, &mut search_state) => Ok(result),
                };
                match result {
                    Ok(result) => result,
                    Err(answer) => Ok(RoundOutput {
                        answer,
                        returned_model: None,
                        search: search_state,
                    }),
                }
            } else {
                Ok(output(Answer::Cancelled))
            }
        };
        let event = Event::Answer {
            round_id: prompt.round_id,
            result,
        };
        if *stop.borrow() {
            // Shutdown cannot depend on the owner draining a full event queue.
            let _ = events.try_send(event);
            break;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            sent = events.send(event) => if sent.is_err() { break; },
        }
    }
    closed.send(Event::Closed(Ok(())));
}

const SEARCH_DESCRIPTION: &str = "用指定本批完整公开问题搜索；不能提供query或扩写私有上下文";
const SEARCH_BUDGET: &str = "\n搜索预算已用完，只返回最终候选JSON";
const SEARCH_UNAVAILABLE: &str = "本轮搜索不可用，未核实";

#[derive(Serialize)]
struct SearchInput<'a> {
    message_id: &'a str,
}
#[derive(Serialize)]
struct OpenaiFunction {
    name: &'static str,
    arguments: String,
}
#[derive(Serialize)]
struct OpenaiToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenaiFunction,
}
#[derive(Serialize)]
struct OpenaiMessage<'a> {
    role: &'static str,
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<[OpenaiToolCall<'a>; 1]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}
impl<'a> OpenaiMessage<'a> {
    fn text(role: &'static str, text: &'a str) -> Self {
        Self {
            role,
            content: Some(text),
            tool_calls: None,
            tool_call_id: None,
        }
    }
}
#[derive(Serialize)]
struct OpenaiRequest<'a> {
    model: &'a str,
    stream: bool,
    messages: Vec<OpenaiMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicBlock<'a> {
    ToolUse {
        id: &'a str,
        name: &'static str,
        input: SearchInput<'a>,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
        is_error: bool,
    },
}
#[derive(Serialize)]
#[serde(untagged)]
enum AnthropicContent<'a> {
    Text(&'a str),
    Blocks([AnthropicBlock<'a>; 1]),
}
#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'static str,
    content: AnthropicContent<'a>,
}
#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    stream: bool,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<AnthropicMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
}
struct SearchCall {
    id: String,
    target: usize,
}
struct Completion {
    call: SearchCall,
    text: String,
    failed: bool,
}
enum Parsed {
    Final(RoundOutput),
    Search(SearchCall),
}

fn tools_allowed(prompt: &Prompt) -> bool {
    prompt.web_search && !prompt.search_targets.is_empty()
}
fn request_body(
    config: &ApiConfig,
    prompt: &Prompt,
    completion: Option<&Completion>,
) -> Result<Vec<u8>, ApiError> {
    let contract = crate::workspace::CONTRACT;
    let system_len = contract.len()
        + if completion.is_some() {
            SEARCH_BUDGET.len()
        } else {
            0
        };
    if system_len.saturating_add(prompt.text.len()) > CONTEXT_LIMIT {
        return Err(ApiError::Configuration("应用上下文超限"));
    }
    let system = if completion.is_some() {
        std::borrow::Cow::Owned(format!("{contract}{SEARCH_BUDGET}"))
    } else {
        std::borrow::Cow::Borrowed(contract)
    };
    let allowed = completion.is_none() && tools_allowed(prompt);
    let schema = allowed.then(|| json!({"type":"object","properties":{"message_id":{"type":"string","enum":prompt.search_targets.iter().map(|t| &t.message_id).collect::<Vec<_>>()}},"required":["message_id"],"additionalProperties":false}));
    let body = match config.protocol {
        ApiProtocol::OpenaiChat => {
            let mut messages = Vec::with_capacity(if completion.is_some() { 4 } else { 2 });
            messages.push(OpenaiMessage::text("system", &system));
            messages.push(OpenaiMessage::text("user", &prompt.text));
            if let Some(c) = completion {
                let arguments = serde_json::to_string(&SearchInput { message_id: &prompt.search_targets[c.call.target].message_id }).map_err(|_| ApiError::Configuration("请求编码失败"))?;
                messages.push(OpenaiMessage { role: "assistant", content: None, tool_calls: Some([OpenaiToolCall { id: &c.call.id, kind:"function", function:OpenaiFunction { name:"web_search", arguments } }]), tool_call_id: None });
                messages.push(OpenaiMessage { role: "tool", content: Some(&c.text), tool_calls: None, tool_call_id: Some(&c.call.id) });
            }
            serde_json::to_vec(&OpenaiRequest {
                model: config.model.trim(), stream: false, messages,
                max_completion_tokens: (config.openai_token_limit == OpenaiTokenLimit::MaxCompletionTokens).then_some(config.max_output_tokens),
                max_tokens: (config.openai_token_limit == OpenaiTokenLimit::MaxTokens).then_some(config.max_output_tokens),
                tools: schema.map(|s| json!([{"type":"function","function":{"name":"web_search","description":SEARCH_DESCRIPTION,"parameters":s}}])),
                tool_choice: allowed.then_some("auto"),
            })
        }
        ApiProtocol::AnthropicMessages => {
            let mut messages = Vec::with_capacity(if completion.is_some() { 3 } else { 1 });
            messages.push(AnthropicMessage { role: "user", content: AnthropicContent::Text(&prompt.text) });
            if let Some(c) = completion {
                messages.push(AnthropicMessage { role:"assistant", content:AnthropicContent::Blocks([AnthropicBlock::ToolUse { id:&c.call.id, name:"web_search", input:SearchInput { message_id:&prompt.search_targets[c.call.target].message_id } }]) });
                messages.push(AnthropicMessage { role:"user", content:AnthropicContent::Blocks([AnthropicBlock::ToolResult { tool_use_id:&c.call.id, content:&c.text, is_error:c.failed }]) });
            }
            serde_json::to_vec(&AnthropicRequest {
                model: config.model.trim(), stream: false, max_tokens: config.max_output_tokens, system: &system, messages,
                tools: schema.map(|s| json!([{"name":"web_search","description":SEARCH_DESCRIPTION,"input_schema":s}])),
                tool_choice: allowed.then(|| json!({"type":"auto","disable_parallel_tool_use":true})),
            })
        }
        ApiProtocol::ChatgptSubscription => return Err(ApiError::Configuration("订阅请求不可使用Chat Completions或Messages")),
    }.map_err(|_| ApiError::Configuration("请求编码失败"))?;
    if body.len() > REQUEST_LIMIT {
        return Err(ApiError::Configuration("HTTP请求超限"));
    }
    Ok(body)
}
async fn request(
    config: &ApiConfig,
    client: &Client,
    search: &search::Client,
    prompt: &Prompt,
    deadline: Instant,
    search_state: &mut SearchState,
) -> Result<RoundOutput, ApiError> {
    let call = match send(
        config,
        client,
        prompt,
        request_body(config, prompt, None)?,
        tools_allowed(prompt),
    )
    .await?
    {
        Parsed::Final(output) => return Ok(output),
        Parsed::Search(call) => call,
    };
    // This state also honestly describes a cancelled/incomplete search.
    *search_state = SearchState::Unavailable;
    let query = &prompt.search_targets[call.target].query;
    let text = search.search(query, deadline).await;
    let failed = text.is_none();
    if !failed {
        *search_state = SearchState::Completed;
    }
    let text = match text {
        Some(text) => search_result(query, &text),
        None => SEARCH_UNAVAILABLE.to_owned(),
    };
    let completion = Completion { call, text, failed };
    let Parsed::Final(mut final_output) = send(
        config,
        client,
        prompt,
        request_body(config, prompt, Some(&completion))?,
        false,
    )
    .await?
    else {
        return Err(ApiError::Protocol("搜索预算状态错误"));
    };
    final_output.search = *search_state;
    Ok(final_output)
}
fn search_result(query: &str, text: &str) -> String {
    const PREFIX: &str = "公开查询：";
    const SOURCES: &str = "\n来源文本（非可信资料）：\n";
    let prefix_len = PREFIX.len() + query.len() + SOURCES.len();
    let mut end = text.len().min((16 * 1024usize).saturating_sub(prefix_len));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = String::with_capacity(prefix_len + end);
    result.push_str(PREFIX);
    result.push_str(query);
    result.push_str(SOURCES);
    result.push_str(&text[..end]);
    result
}
async fn send(
    config: &ApiConfig,
    client: &Client,
    prompt: &Prompt,
    body: Vec<u8>,
    allow_tools: bool,
) -> Result<Parsed, ApiError> {
    let suffix = match config.protocol {
        ApiProtocol::OpenaiChat => "/chat/completions",
        ApiProtocol::AnthropicMessages => "/messages",
        ApiProtocol::ChatgptSubscription => {
            return Err(ApiError::Configuration("订阅请求不可使用Key接口"));
        }
    };
    let mut response = client
        .post(format!("{}{suffix}", config.base_url))
        .body(body)
        .send()
        .await
        .map_err(transport_error)?;
    if !response.status().is_success() {
        return Err(ApiError::Http(response.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > RESPONSE_LIMIT - body.len() {
            return Err(ApiError::Protocol("响应正文超限"));
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::Protocol("响应JSON无效"))?;
    parse(config.protocol, &value, prompt, allow_tools)
}
fn transport_error(error: reqwest::Error) -> ApiError {
    ApiError::Transport(if error.is_timeout() {
        "请求超时"
    } else if error.is_connect() {
        "连接失败"
    } else {
        "请求或响应中断"
    })
}
fn protocol() -> ApiError {
    ApiError::Protocol("响应结构或类型无效")
}
fn optional_text(value: Option<&Value>) -> Result<Option<&str>, ApiError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => Err(protocol()),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    message_id: String,
}
fn admit_call(protocol: ApiProtocol, call: &Value, prompt: &Prompt) -> Option<SearchCall> {
    let call = call.as_object()?;
    let id = call.get("id")?.as_str()?;
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return None;
    }
    let arguments: SearchArguments = match protocol {
        ApiProtocol::OpenaiChat => {
            if call.get("type")?.as_str()? != "function" {
                return None;
            }
            let function = call.get("function")?.as_object()?;
            if function.get("name")?.as_str()? != "web_search" {
                return None;
            }
            serde_json::from_str(function.get("arguments")?.as_str()?).ok()?
        }
        ApiProtocol::AnthropicMessages => {
            if call.get("name")?.as_str()? != "web_search" {
                return None;
            }
            SearchArguments::deserialize(call.get("input")?).ok()?
        }
        ApiProtocol::ChatgptSubscription => return None,
    };
    let target = prompt
        .search_targets
        .iter()
        .position(|t| t.message_id == arguments.message_id)?;
    Some(SearchCall {
        id: id.to_owned(),
        target,
    })
}
fn parse(
    protocol_kind: ApiProtocol,
    value: &Value,
    prompt: &Prompt,
    allow_tools: bool,
) -> Result<Parsed, ApiError> {
    let object = value.as_object().ok_or_else(protocol)?;
    let returned_model = optional_text(object.get("model"))?;
    let rejected = |reason: &str| Answer::Rejected(reason.to_owned());
    let tool_answer = |single: Option<&Value>| {
        if let Some(call) = single
            .filter(|_| allow_tools)
            .and_then(|c| admit_call(protocol_kind, c, prompt))
        {
            return Parsed::Search(call);
        }
        Parsed::Final(RoundOutput {
            answer: rejected("本轮工具调用不合法或预算已用完"),
            returned_model: returned_model.map(str::to_owned),
            search: SearchState::NotRequested,
        })
    };
    let answer = match protocol_kind {
        ApiProtocol::OpenaiChat => {
            if object.get("object").and_then(Value::as_str) != Some("chat.completion") {
                return Err(protocol());
            }
            let choices = object
                .get("choices")
                .and_then(Value::as_array)
                .ok_or_else(protocol)?;
            if choices.len() != 1 {
                return Err(protocol());
            }
            let choice = choices[0].as_object().ok_or_else(protocol)?;
            if choice.get("index").and_then(Value::as_u64) != Some(0) {
                return Err(protocol());
            }
            let message = choice
                .get("message")
                .and_then(Value::as_object)
                .ok_or_else(protocol)?;
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                return Err(protocol());
            }
            let text = optional_text(message.get("content"))?;
            let refusal = optional_text(message.get("refusal"))?;
            let tools = match message.get("tool_calls") {
                None | Some(Value::Null) => &[][..],
                Some(Value::Array(calls)) => calls.as_slice(),
                _ => return Err(protocol()),
            };
            match choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .ok_or_else(protocol)?
            {
                "length" | "content_filter" => rejected("模型截断或拒绝本批"),
                "stop" | "tool_calls" if refusal.is_some_and(|s| !s.is_empty()) => {
                    rejected("模型拒绝本批")
                }
                "stop" if !tools.is_empty() => rejected("正常结束夹带工具调用"),
                "stop" => {
                    if !message.contains_key("content") {
                        return Err(protocol());
                    }
                    candidate_answer(text, prompt)
                }
                "tool_calls" => return Ok(tool_answer((tools.len() == 1).then(|| &tools[0]))),
                _ => return Err(ApiError::Protocol("未知结束原因")),
            }
        }
        ApiProtocol::AnthropicMessages => {
            if object.get("type").and_then(Value::as_str) != Some("message")
                || object.get("role").and_then(Value::as_str) != Some("assistant")
            {
                return Err(protocol());
            }
            let blocks = object
                .get("content")
                .and_then(Value::as_array)
                .ok_or_else(protocol)?;
            let mut tool_count = 0;
            let mut single_tool = None;
            for block in blocks {
                let obj = block.as_object().ok_or_else(protocol)?;
                match obj.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        obj.get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(protocol)?;
                    }
                    Some("tool_use") => {
                        tool_count += 1;
                        single_tool = Some(block);
                    }
                    _ => return Err(protocol()),
                }
            }
            match object
                .get("stop_reason")
                .and_then(Value::as_str)
                .ok_or_else(protocol)?
            {
                "max_tokens" | "refusal" => rejected("模型截断或拒绝本批"),
                "end_turn" if tool_count != 0 => rejected("正常结束夹带工具调用"),
                "end_turn" => {
                    let mut text = std::borrow::Cow::Borrowed("");
                    for block in blocks {
                        let part = block["text"].as_str().ok_or_else(protocol)?;
                        if text.is_empty() {
                            text = std::borrow::Cow::Borrowed(part);
                        } else if !part.is_empty() {
                            text.to_mut().push_str(part);
                        }
                    }
                    candidate_answer(Some(&text), prompt)
                }
                "tool_use" => {
                    return Ok(tool_answer(if tool_count == 1 {
                        single_tool
                    } else {
                        None
                    }));
                }
                _ => return Err(ApiError::Protocol("未知结束原因")),
            }
        }
        ApiProtocol::ChatgptSubscription => {
            return Err(ApiError::Protocol("订阅响应不可使用Key协议解析器"));
        }
    };
    Ok(Parsed::Final(RoundOutput {
        answer,
        returned_model: returned_model.map(str::to_owned),
        search: SearchState::NotRequested,
    }))
}
fn candidate_answer(text: Option<&str>, prompt: &Prompt) -> Answer {
    let Some(text) = text.filter(|s| !s.trim().is_empty()) else {
        return Answer::NoMessage;
    };
    match model::candidates(text, &prompt.round_id, &prompt.message_ids) {
        Ok(candidates) => Answer::Candidates(candidates),
        Err(error) => Answer::Rejected(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::{HttpServer, Response, Scenario, anthropic, openai};
    use serde_json::json;
    use std::{io::Write, sync::Arc};
    use tokio::time::timeout;

    const SECRET: &str = "private-response-sentinel-73951";
    const KEY: &str = "synthetic-api-key-only";
    const PROTOCOLS: [ApiProtocol; 2] = [ApiProtocol::OpenaiChat, ApiProtocol::AnthropicMessages];
    fn prompt(epoch: u64) -> Prompt {
        Prompt {
            round_id: "round-1".into(),
            text: "完整资料与当前问题".into(),
            message_ids: Arc::from(["message-1".into()]),
            cancel_epoch: epoch,
            deadline: Instant::now() + Duration::from_secs(3),
            web_search: false,
            search_targets: Vec::new(),
        }
    }
    fn candidate() -> String {
        json!({"round_id":"round-1","candidates":[{"message_id":"message-1","text":"简短答复"}]})
            .to_string()
    }
    fn envelope(protocol: ApiProtocol, text: Option<&str>, finish: &str) -> Value {
        match protocol {
            ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
            ApiProtocol::OpenaiChat => openai(text, finish),
            ApiProtocol::AnthropicMessages => anthropic(
                text.map_or(json!([]), |s| json!([{"type":"text","text":s}])),
                finish,
            ),
        }
    }
    fn normal(protocol: ApiProtocol, text: Option<&str>) -> Value {
        envelope(
            protocol,
            text,
            match protocol {
                ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                ApiProtocol::OpenaiChat => "stop",
                ApiProtocol::AnthropicMessages => "end_turn",
            },
        )
    }
    fn handle(server: &HttpServer, protocol: ApiProtocol) -> Handle {
        Handle::spawn(
            ApiConfig {
                protocol,
                base_url: server.base_url(),
                model: "configured-model".into(),
                ..ApiConfig::default()
            },
            ApiKey::new(KEY).unwrap(),
        )
        .unwrap()
    }
    async fn event(handle: &mut Handle) -> Event {
        timeout(Duration::from_secs(3), handle.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn result(
        handle: &mut Handle,
        prompt: Prompt,
        started: bool,
    ) -> Result<RoundOutput, ApiError> {
        let round = prompt.round_id.clone();
        handle.prompts.send(prompt).await.unwrap();
        if started {
            assert!(
                matches!(event(handle).await, Event::Started { round_id } if round_id == round)
            );
        }
        match event(handle).await {
            Event::Answer { round_id, result } => {
                assert_eq!(round_id, round);
                result
            }
            e => panic!("unexpected event {e:?}"),
        }
    }
    async fn close(handle: &mut Handle) {
        handle.stop();
        timeout(Duration::from_secs(3), &mut handle.task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event(handle).await, Event::Closed(Ok(()))));
    }
    fn assert_clean(error: &ApiError) {
        assert!(!format!("{error:#} {error:?}").contains(SECRET));
        assert!(!format!("{error:#} {error:?}").contains(KEY));
        assert!(!format!("{error:#} {error:?}").contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn both_protocols_use_exact_stateless_requests_and_local_spawn() {
        for protocol in PROTOCOLS {
            let server = HttpServer::spawn(vec![
                Scenario::new(Response::json(normal(protocol, Some(&candidate())))),
                Scenario::new(Response::json(normal(protocol, None))),
            ])
            .await;
            let mut h = handle(&server, protocol);
            tokio::task::yield_now().await;
            assert!(server.requests().is_empty());
            assert_eq!(h.prompts.max_capacity(), 1);
            assert!(
                matches!(result(&mut h, prompt(0), true).await.unwrap().answer, Answer::Candidates(cs) if cs.len() == 1)
            );
            assert!(matches!(
                result(&mut h, prompt(0), true).await.unwrap().answer,
                Answer::NoMessage
            ));
            let requests = server.requests();
            assert_eq!(requests.len(), 2);
            for request in requests {
                assert_eq!(request.method, "POST");
                assert!(request.header("cookie").is_none());
                assert!(!String::from_utf8_lossy(&request.body).contains(KEY));
                let body = request.json();
                assert_eq!(body["model"], "configured-model");
                assert_eq!(body["stream"], false);
                assert!(body.get("tools").is_none());
                match protocol {
                    ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                    ApiProtocol::OpenaiChat => {
                        assert_eq!(request.path, "/v1/chat/completions");
                        assert!(
                            request
                                .header("authorization")
                                .is_some_and(|s| s == format!("Bearer {KEY}"))
                        );
                        assert!(request.header("x-api-key").is_none());
                        assert_eq!(body.as_object().unwrap().len(), 4);
                        assert_eq!(body["max_completion_tokens"], 4096);
                        assert!(body.get("max_tokens").is_none());
                        assert_eq!(
                            body["messages"],
                            json!([{"role":"system","content":crate::workspace::CONTRACT},{"role":"user","content":prompt(0).text}])
                        );
                    }
                    ApiProtocol::AnthropicMessages => {
                        assert_eq!(request.path, "/v1/messages");
                        assert!(request.header("x-api-key").is_some_and(|s| s == KEY));
                        assert!(request.header("authorization").is_none());
                        assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
                        assert_eq!(body.as_object().unwrap().len(), 5);
                        assert_eq!(body["max_tokens"], 4096);
                        assert_eq!(body["system"], crate::workspace::CONTRACT);
                        assert_eq!(
                            body["messages"],
                            json!([{"role":"user","content":prompt(0).text}])
                        );
                    }
                }
            }
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn selected_legacy_token_field_is_sent_without_fallback() {
        let server = HttpServer::spawn(vec![Scenario::new(Response::json(normal(
            ApiProtocol::OpenaiChat,
            None,
        )))])
        .await;
        let mut h = Handle::spawn(
            ApiConfig {
                base_url: server.base_url(),
                model: "model".into(),
                openai_token_limit: OpenaiTokenLimit::MaxTokens,
                ..ApiConfig::default()
            },
            ApiKey::new(KEY).unwrap(),
        )
        .unwrap();
        result(&mut h, prompt(0), true).await.unwrap();
        let body = server.requests()[0].json();
        assert_eq!(body["max_tokens"], 4096);
        assert!(body.get("max_completion_tokens").is_none());
        close(&mut h).await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn candidate_content_rejection_is_atomic_and_worker_continues() {
        let good = candidate();
        let bad = [
            SECRET.to_owned(),
            json!({"round_id":"old","candidates":[]}).to_string(),
            json!({"round_id":"round-1","candidates":[{"message_id":"message-1","text":"正常"},{"message_id":"unknown","text":"非法目标"}]}).to_string(),
            json!({"round_id":"round-1","candidates":[{"message_id":"message-1","text":"正常"},{"message_id":"message-1","text":"重复"}]}).to_string(),
            json!({"round_id":"round-1","candidates":[{"message_id":"message-1","text":""}]}).to_string(),
            json!({"round_id":"round-1","candidates":vec![json!({"message_id":"message-1","text":"重复"});9]}).to_string(),
            json!({"round_id":"round-1","candidates":[],SECRET:SECRET}).to_string(),
        ];
        for protocol in PROTOCOLS {
            let server = HttpServer::spawn(
                bad.iter()
                    .chain(std::iter::once(&good))
                    .map(|text| Scenario::new(Response::json(normal(protocol, Some(text)))))
                    .collect(),
            )
            .await;
            let mut h = handle(&server, protocol);
            for _ in &bad {
                let answer = result(&mut h, prompt(0), true).await.unwrap();
                assert_eq!(answer.search, SearchState::NotRequested);
                assert!(
                    matches!(answer.answer, Answer::Rejected(reason) if !reason.contains(SECRET))
                );
            }
            assert!(
                matches!(result(&mut h, prompt(0), true).await.unwrap().answer, Answer::Candidates(cs) if cs.len() == 1)
            );
            assert_eq!(server.requests().len(), bad.len() + 1);
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn truncation_refusal_and_mixed_tools_are_never_no_message_or_candidates() {
        for protocol in PROTOCOLS {
            let mut cases = match protocol {
                ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                ApiProtocol::OpenaiChat => vec![
                    envelope(protocol, None, "length"),
                    envelope(protocol, None, "content_filter"),
                    envelope(protocol, None, "tool_calls"),
                ],
                ApiProtocol::AnthropicMessages => vec![
                    envelope(protocol, None, "max_tokens"),
                    envelope(protocol, None, "refusal"),
                    envelope(protocol, None, "tool_use"),
                ],
            };
            let mut mixed = normal(protocol, Some(&candidate()));
            match protocol {
                ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                ApiProtocol::OpenaiChat => {
                    mixed["choices"][0]["message"]["tool_calls"] = json!([{"id":"call-1","type":"function","function":{"name":"web_search","arguments":"{}"}}]);
                    let mut refusal = normal(protocol, None);
                    refusal["choices"][0]["message"]["refusal"] = json!(SECRET);
                    cases.push(refusal);
                }
                ApiProtocol::AnthropicMessages => mixed["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"tool_use","id":"call-1","name":"web_search","input":{}})),
            }
            cases.push(mixed);
            let count = cases.len();
            let server = HttpServer::spawn(
                cases
                    .into_iter()
                    .map(|v| Scenario::new(Response::json(v)))
                    .collect(),
            )
            .await;
            let mut h = handle(&server, protocol);
            for _ in 0..count {
                let mut p = prompt(0);
                p.web_search = true;
                p.search_targets.push(model::SearchTarget {
                    message_id: "message-1".into(),
                    query: "公开问题".into(),
                });
                assert!(
                    matches!(result(&mut h, p, true).await.unwrap().answer, Answer::Rejected(reason) if !reason.contains(SECRET))
                );
            }
            assert_eq!(server.requests().len(), count);
            assert!(
                server
                    .requests()
                    .iter()
                    .all(|r| r.json().get("tools").is_some())
            );
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn outer_envelopes_and_types_are_protocol_errors_before_content() {
        for protocol in PROTOCOLS {
            let mut cases = vec![
                json!([SECRET]),
                json!({"error":SECRET}),
                envelope(protocol, Some(SECRET), "unexpected"),
                envelope(protocol, Some(SECRET), ""),
            ];
            let mut base = normal(protocol, None);
            if protocol == ApiProtocol::OpenaiChat {
                base["choices"][0]["message"]["tool_calls"] = Value::Null;
            }
            match protocol {
                ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                ApiProtocol::OpenaiChat => {
                    for (path, value) in [
                        ("/object", json!("wrong")),
                        ("/choices", json!([])),
                        ("/choices/0/index", json!(1)),
                        ("/choices/0/message/role", json!("user")),
                        ("/choices/0/message/content", json!(123)),
                        ("/choices/0/message/tool_calls", json!({})),
                        ("/choices/0/finish_reason", Value::Null),
                    ] {
                        let mut v = base.clone();
                        *v.pointer_mut(path)
                            .unwrap_or_else(|| panic!("fixture missing {path}")) = value;
                        cases.push(v);
                    }
                }
                ApiProtocol::AnthropicMessages => {
                    for (path, value) in [
                        ("/type", json!("wrong")),
                        ("/role", json!("user")),
                        ("/content", json!(null)),
                        ("/content", json!([{"type":"thinking","thinking":SECRET}])),
                        ("/content", json!([{"type":"text","text":123}])),
                        ("/stop_reason", Value::Null),
                    ] {
                        let mut v = base.clone();
                        *v.pointer_mut(path).unwrap() = value;
                        cases.push(v);
                    }
                }
            }
            let count = cases.len();
            let server = HttpServer::spawn(
                cases
                    .into_iter()
                    .map(|v| Scenario::new(Response::json(v)))
                    .collect(),
            )
            .await;
            let mut h = handle(&server, protocol);
            for _ in 0..count {
                let error = result(&mut h, prompt(0), true).await.unwrap_err();
                assert!(matches!(error, ApiError::Protocol(_)));
                assert_clean(&error);
            }
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn http_failures_and_disconnects_are_not_retried_and_hide_raw_bodies() {
        for protocol in PROTOCOLS {
            let mut scenarios: Vec<_> = [401, 403, 429, 500, 503]
                .into_iter()
                .map(|status| {
                    Scenario::new(Response::bytes(SECRET.as_bytes().to_vec()).status(status))
                })
                .collect();
            scenarios.extend([
                Scenario::new(Response::disconnect()),
                Scenario::new(Response::partial(SECRET.as_bytes().to_vec(), 1024)),
                Scenario::new(Response::bytes(SECRET.as_bytes().to_vec())),
            ]);
            let server = HttpServer::spawn(scenarios).await;
            let mut h = handle(&server, protocol);
            for status in [401, 403, 429, 500, 503] {
                let error = result(&mut h, prompt(0), true).await.unwrap_err();
                assert_eq!(error, ApiError::Http(status));
                assert_clean(&error);
            }
            for _ in 0..2 {
                let error = result(&mut h, prompt(0), true).await.unwrap_err();
                assert!(matches!(error, ApiError::Transport(_)));
                assert_clean(&error);
            }
            let error = result(&mut h, prompt(0), true).await.unwrap_err();
            assert!(matches!(error, ApiError::Protocol(_)));
            assert_clean(&error);
            assert_eq!(server.requests().len(), 8);
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn redirects_never_disclose_authentication_to_another_target() {
        let target = HttpServer::spawn(Vec::new()).await;
        let server = HttpServer::spawn(vec![Scenario::new(
            Response::bytes(Vec::new())
                .status(307)
                .header("location", &target.url("/v1/chat/completions")),
        )])
        .await;
        let mut h = handle(&server, ApiProtocol::OpenaiChat);
        assert_eq!(
            result(&mut h, prompt(0), true).await.unwrap_err(),
            ApiError::Http(307)
        );
        assert!(target.requests().is_empty());
        close(&mut h).await;
        server.shutdown().await;
        target.shutdown().await;
    }

    #[tokio::test]
    async fn response_limit_applies_to_complete_chunked_and_decompressed_bodies() {
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&vec![b' '; RESPONSE_LIMIT + 1]).unwrap();
        let compressed = gzip.finish().unwrap();
        assert!(compressed.len() < RESPONSE_LIMIT);
        let server = HttpServer::spawn(vec![
            Scenario::new(Response::bytes(vec![b' '; RESPONSE_LIMIT + 1])),
            Scenario::new(Response::chunked(vec![
                vec![b' '; RESPONSE_LIMIT],
                vec![b' '; 1],
            ])),
            Scenario::new(Response::bytes(compressed).header("content-encoding", "gzip")),
            Scenario::new(Response::bytes({
                let mut bytes = serde_json::to_vec(&normal(ApiProtocol::OpenaiChat, None)).unwrap();
                bytes.resize(RESPONSE_LIMIT, b' ');
                bytes
            })),
        ])
        .await;
        let mut h = handle(&server, ApiProtocol::OpenaiChat);
        for _ in 0..3 {
            assert_eq!(
                result(&mut h, prompt(0), true).await.unwrap_err(),
                ApiError::Protocol("响应正文超限")
            );
        }
        assert!(matches!(
            result(&mut h, prompt(0), true).await.unwrap().answer,
            Answer::NoMessage
        ));
        close(&mut h).await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn valid_json_can_cross_http_chunks_and_anthropic_text_blocks() {
        for protocol in PROTOCOLS {
            let mut v = normal(protocol, Some(&candidate()));
            if protocol == ApiProtocol::AnthropicMessages {
                let text = candidate();
                let split = text.find("candidates").unwrap();
                v["content"] = json!([{"type":"text","text":&text[..split]}, {"type":"text","text":&text[split..]}]);
            }
            let bytes = serde_json::to_vec(&v).unwrap();
            let server = HttpServer::spawn(vec![Scenario::new(Response::chunked(
                bytes.chunks(7).map(<[u8]>::to_vec).collect(),
            ))])
            .await;
            let mut h = handle(&server, protocol);
            assert!(matches!(
                result(&mut h, prompt(0), true).await.unwrap().answer,
                Answer::Candidates(_)
            ));
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn request_context_and_escaped_wire_limits_prevent_http() {
        for protocol in PROTOCOLS {
            let server = HttpServer::spawn(Vec::new()).await;
            let mut h = handle(&server, protocol);
            let mut p = prompt(0);
            p.text = "x".repeat(CONTEXT_LIMIT);
            assert_eq!(
                result(&mut h, p, true).await.unwrap_err(),
                ApiError::Configuration("应用上下文超限")
            );
            let mut p = prompt(0);
            p.text = "\u{0001}".repeat(CONTEXT_LIMIT - crate::workspace::CONTRACT.len());
            assert_eq!(
                result(&mut h, p, true).await.unwrap_err(),
                ApiError::Configuration("HTTP请求超限")
            );
            assert!(server.requests().is_empty());
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn cancelled_queued_and_inflight_requests_release_single_flight() {
        let (held, received, release) = Scenario::new(Response::json(normal(
            ApiProtocol::OpenaiChat,
            Some(&candidate()),
        )))
        .barrier();
        let server = HttpServer::spawn(vec![
            held,
            Scenario::new(Response::json(normal(
                ApiProtocol::OpenaiChat,
                Some(&candidate()),
            ))),
        ])
        .await;
        let mut h = handle(&server, ApiProtocol::OpenaiChat);
        h.prompts.send(prompt(0)).await.unwrap();
        assert!(matches!(event(&mut h).await, Event::Started { .. }));
        timeout(Duration::from_secs(3), received)
            .await
            .unwrap()
            .unwrap();
        let mut queued = prompt(0);
        queued.round_id = "queued".into();
        h.prompts.send(queued).await.unwrap();
        h.cancel();
        for round in ["round-1", "queued"] {
            assert!(
                matches!(event(&mut h).await, Event::Answer { round_id, result: Ok(RoundOutput { answer: Answer::Cancelled, .. }) } if round_id == round)
            );
        }
        let _ = release.send(());
        assert_eq!(server.requests().len(), 1);
        let epoch = h.epoch();
        assert!(matches!(
            result(&mut h, prompt(epoch), true).await.unwrap().answer,
            Answer::Candidates(_)
        ));
        assert_eq!(server.requests().len(), 2);
        close(&mut h).await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn expired_before_admission_does_not_start_or_send() {
        let server = HttpServer::spawn(Vec::new()).await;
        let mut h = handle(&server, ApiProtocol::OpenaiChat);
        let mut p = prompt(0);
        p.deadline = Instant::now();
        assert!(matches!(
            result(&mut h, p, false).await.unwrap().answer,
            Answer::Expired
        ));
        let stale = prompt(0);
        h.cancel();
        assert!(matches!(
            result(&mut h, stale, false).await.unwrap().answer,
            Answer::Cancelled
        ));
        assert!(server.requests().is_empty());
        close(&mut h).await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn shared_deadline_cancels_waiting_response_not_just_connect() {
        for protocol in PROTOCOLS {
            let (held, received, release) =
                Scenario::new(Response::json(normal(protocol, Some(&candidate())))).barrier();
            let server = HttpServer::spawn(vec![held]).await;
            let mut h = handle(&server, protocol);
            let mut p = prompt(0);
            p.deadline = Instant::now() + Duration::from_millis(200);
            h.prompts.send(p).await.unwrap();
            assert!(matches!(event(&mut h).await, Event::Started { .. }));
            timeout(Duration::from_secs(3), received)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                event(&mut h).await,
                Event::Answer {
                    result: Ok(RoundOutput {
                        answer: Answer::Expired,
                        returned_model: None,
                        ..
                    }),
                    ..
                }
            ));
            let _ = release.send(());
            assert_eq!(server.requests().len(), 1);
            close(&mut h).await;
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn stop_and_drop_never_leave_owned_workers_waiting_on_remote_response() {
        for drop_handle in [false, true] {
            let (held, received, release) =
                Scenario::new(Response::json(normal(ApiProtocol::OpenaiChat, None))).barrier();
            let server = HttpServer::spawn(vec![held]).await;
            let mut h = handle(&server, ApiProtocol::OpenaiChat);
            h.prompts.send(prompt(0)).await.unwrap();
            assert!(matches!(event(&mut h).await, Event::Started { .. }));
            timeout(Duration::from_secs(3), received)
                .await
                .unwrap()
                .unwrap();
            if drop_handle {
                let task = h.task.abort_handle();
                drop(h);
                timeout(Duration::from_secs(3), async {
                    while !task.is_finished() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            } else {
                h.stop();
                timeout(Duration::from_secs(3), &mut h.task)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(matches!(
                    event(&mut h).await,
                    Event::Answer {
                        result: Ok(RoundOutput {
                            answer: Answer::Cancelled,
                            ..
                        }),
                        ..
                    }
                ));
                assert!(matches!(event(&mut h).await, Event::Closed(Ok(()))));
            }
            drop(release);
            server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn stopping_with_full_event_backpressure_completes_without_http_leak() {
        let server = HttpServer::spawn(
            (0..5)
                .map(|_| Scenario::new(Response::json(normal(ApiProtocol::OpenaiChat, None))))
                .collect(),
        )
        .await;
        let mut h = handle(&server, ApiProtocol::OpenaiChat);
        for _ in 0..5 {
            h.prompts.send(prompt(0)).await.unwrap();
        }
        server.wait_requests(4).await;
        h.stop();
        timeout(Duration::from_secs(3), &mut h.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(server.requests().len(), 4);
        let mut closed = false;
        while let Some(event) = h.events.recv().await {
            if matches!(event, Event::Closed(Ok(()))) {
                closed = true;
            }
        }
        assert!(closed);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn invalid_local_configuration_errors_do_not_echo_supplied_secrets() {
        for base_url in [
            format!("https://{SECRET}@example.test/v1"),
            format!("http://{SECRET}.test/v1"),
            String::new(),
        ] {
            let result = Handle::spawn(
                ApiConfig {
                    base_url,
                    model: "configured-model".into(),
                    ..ApiConfig::default()
                },
                ApiKey::new(KEY).unwrap(),
            );
            let error = match result {
                Ok(_) => panic!("unsafe URL accepted"),
                Err(error) => error,
            };
            assert!(!format!("{error:#}").contains(SECRET));
        }
    }

    #[tokio::test]
    async fn normal_empty_content_metadata_and_returned_models_are_not_confused() {
        for protocol in PROTOCOLS {
            let mut empty_candidates = normal(
                protocol,
                Some("{\"round_id\":\"round-1\",\"candidates\":[]}"),
            );
            empty_candidates["model"] = json!("actual-model");
            let mut whitespace = normal(protocol, Some(" \n\t "));
            if protocol == ApiProtocol::OpenaiChat {
                whitespace["choices"][0]["message"]["reasoning_content"] = json!(SECRET);
            }
            let mut missing_model = normal(protocol, None);
            missing_model.as_object_mut().unwrap().remove("model");
            let server = HttpServer::spawn(vec![
                Scenario::new(Response::json(empty_candidates)),
                Scenario::new(Response::json(whitespace)),
                Scenario::new(Response::json(missing_model)),
            ])
            .await;
            let mut h = handle(&server, protocol);
            let output = result(&mut h, prompt(0), true).await.unwrap();
            assert!(matches!(output.answer, Answer::Candidates(cs) if cs.is_empty()));
            assert_eq!(output.returned_model.as_deref(), Some("actual-model"));
            assert!(matches!(
                result(&mut h, prompt(0), true).await.unwrap().answer,
                Answer::NoMessage
            ));
            let output = result(&mut h, prompt(0), true).await.unwrap();
            assert!(matches!(output.answer, Answer::NoMessage));
            assert!(output.returned_model.is_none());
            close(&mut h).await;
            server.shutdown().await;
        }
    }
    fn search_prompt(epoch: u64) -> Prompt {
        let mut p = prompt(epoch);
        p.text = format!("资料秘密={SECRET}; 历史UID=private-uid-998; 当前公开问题");
        p.web_search = true;
        p.search_targets.push(model::SearchTarget {
            message_id: "message-1".into(),
            query: "如何理解增量游标？".into(),
        });
        p
    }
    fn call(protocol: ApiProtocol, id: &str, name: &str, args: Value) -> Value {
        match protocol {
            ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
            ApiProtocol::OpenaiChat => {
                json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
            }
            ApiProtocol::AnthropicMessages => {
                json!({"type":"tool_use","id":id,"name":name,"input":args})
            }
        }
    }
    fn tools(protocol: ApiProtocol, calls: Vec<Value>) -> Value {
        match protocol {
            ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
            ApiProtocol::OpenaiChat => {
                let mut value = envelope(protocol, Some(SECRET), "tool_calls");
                value["choices"][0]["message"]["tool_calls"] = json!(calls);
                value
            }
            ApiProtocol::AnthropicMessages => {
                let mut blocks = vec![json!({"type":"text","text":SECRET})];
                blocks.extend(calls);
                anthropic(json!(blocks), "tool_use")
            }
        }
    }
    fn one_tool(protocol: ApiProtocol) -> Value {
        tools(
            protocol,
            vec![call(
                protocol,
                "call-1",
                "web_search",
                json!({"message_id":"message-1"}),
            )],
        )
    }
    fn mcp_init() -> Response {
        Response::json(crate::runner::test_support::mcp(
            json!(1),
            json!({"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"test","version":"1"}}),
        ))
    }
    fn mcp_result(text: &str) -> Response {
        Response::json(crate::runner::test_support::mcp(
            json!(2),
            json!({"content":[{"type":"text","text":text}]}),
        ))
    }
    fn search_handle(
        model: &HttpServer,
        protocol: ApiProtocol,
        mcp: &HttpServer,
        duration: Duration,
    ) -> Handle {
        let search =
            search::Client::with_endpoint(&mcp.url("/mcp?tools=web_search_exa"), duration).unwrap();
        Handle::spawn_for_test(
            ApiConfig {
                protocol,
                base_url: model.base_url(),
                model: "configured-model".into(),
                ..ApiConfig::default()
            },
            ApiKey::new(KEY).unwrap(),
            search,
        )
        .unwrap()
    }
    fn assert_exa_private(server: &HttpServer, expected_query: &str) {
        let requests = server.requests();
        for r in &requests {
            assert!(r.header("authorization").is_none());
            assert!(r.header("x-api-key").is_none());
            let bytes = String::from_utf8_lossy(&r.body);
            for secret in [SECRET, KEY, "private-uid-998", "message-1"] {
                assert!(!bytes.contains(secret));
            }
        }
        let calls: Vec<_> = requests
            .iter()
            .filter(|r| r.method == "POST" && r.json()["method"] == "tools/call")
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].json()["params"],
            json!({"name":"web_search_exa","arguments":{"query":expected_query,"numResults":3}})
        );
    }
    fn assert_completion(protocol: ApiProtocol, first: &Value, second: &Value, failed: bool) {
        assert!(first.get("tools").is_some());
        let schema = match protocol {
            ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
            ApiProtocol::OpenaiChat => &first["tools"][0]["function"]["parameters"],
            ApiProtocol::AnthropicMessages => &first["tools"][0]["input_schema"],
        };
        assert_eq!(
            schema,
            &json!({"type":"object","properties":{"message_id":{"type":"string","enum":["message-1"]}},"required":["message_id"],"additionalProperties":false})
        );
        assert!(second.get("tools").is_none());
        assert!(second.get("tool_choice").is_none());
        let (system, result_text) = match protocol {
            ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
            ApiProtocol::OpenaiChat => {
                assert_eq!(first["tool_choice"], "auto");
                assert_eq!(second["messages"].as_array().unwrap().len(), 4);
                assert_eq!(second["messages"][2]["content"], Value::Null);
                assert_eq!(
                    second["messages"][2]["tool_calls"],
                    json!([call(
                        protocol,
                        "call-1",
                        "web_search",
                        json!({"message_id":"message-1"})
                    )])
                );
                assert_eq!(second["messages"][3]["role"], "tool");
                assert_eq!(second["messages"][3]["tool_call_id"], "call-1");
                (
                    second["messages"][0]["content"].as_str().unwrap(),
                    second["messages"][3]["content"].as_str().unwrap(),
                )
            }
            ApiProtocol::AnthropicMessages => {
                assert_eq!(
                    first["tool_choice"],
                    json!({"type":"auto","disable_parallel_tool_use":true})
                );
                assert_eq!(second["messages"].as_array().unwrap().len(), 3);
                assert_eq!(
                    second["messages"][1]["content"],
                    json!([call(
                        protocol,
                        "call-1",
                        "web_search",
                        json!({"message_id":"message-1"})
                    )])
                );
                let result = &second["messages"][2]["content"][0];
                assert_eq!(result["type"], "tool_result");
                assert_eq!(result["tool_use_id"], "call-1");
                assert_eq!(result["is_error"], failed);
                (
                    second["system"].as_str().unwrap(),
                    result["content"].as_str().unwrap(),
                )
            }
        };
        assert_eq!(
            system,
            format!("{}{SEARCH_BUDGET}", crate::workspace::CONTRACT)
        );
        assert!(!result_text.contains(SECRET));
        assert!(result_text.len() <= 16 * 1024);
        if failed {
            assert_eq!(result_text, SEARCH_UNAVAILABLE);
        } else {
            assert!(result_text.contains("如何理解增量游标？"));
            assert!(result_text.contains("https://public.example/source"));
        }
    }

    #[tokio::test]
    async fn search_success_uses_one_real_mcp_and_two_minimal_model_requests() {
        for protocol in PROTOCOLS {
            let source = format!("https://public.example/source\n{}", "🧪".repeat(3990));
            let mcp = HttpServer::spawn(vec![
                Scenario::new(mcp_init()),
                Scenario::new(Response::bytes(vec![]).status(202)),
                Scenario::new(mcp_result(&source)),
            ])
            .await;
            let model = HttpServer::spawn(vec![
                Scenario::new(Response::json(one_tool(protocol))),
                Scenario::new(Response::json(normal(protocol, Some(&candidate())))),
            ])
            .await;
            let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
            let output = result(&mut h, search_prompt(0), true).await.unwrap();
            assert!(matches!(output.answer,Answer::Candidates(cs) if cs.len() == 1));
            assert_eq!(output.search, SearchState::Completed);
            assert_eq!(
                output.returned_model.as_deref(),
                Some(match protocol {
                    ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                    ApiProtocol::OpenaiChat => "fixture-openai",
                    ApiProtocol::AnthropicMessages => "fixture-anthropic",
                })
            );
            assert_eq!(model.requests().len(), 2);
            assert_eq!(mcp.requests().len(), 3);
            assert_exa_private(&mcp, "如何理解增量游标？");
            assert_completion(
                protocol,
                &model.requests()[0].json(),
                &model.requests()[1].json(),
                false,
            );
            close(&mut h).await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn unavailable_search_has_one_completion_and_truthful_failure_result() {
        for protocol in PROTOCOLS {
            for response in [mcp_result(""), mcp_result(SECRET).status(429)] {
                let mcp = HttpServer::spawn(vec![
                    Scenario::new(mcp_init()),
                    Scenario::new(Response::bytes(vec![]).status(202)),
                    Scenario::new(response),
                ])
                .await;
                let model = HttpServer::spawn(vec![
                    Scenario::new(Response::json(one_tool(protocol))),
                    Scenario::new(Response::json(normal(protocol, Some(&candidate())))),
                ])
                .await;
                let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
                let output = result(&mut h, search_prompt(0), true).await.unwrap();
                assert_eq!(output.search, SearchState::Unavailable);
                assert!(matches!(output.answer,Answer::Candidates(cs) if cs.len() == 1));
                assert_eq!(model.requests().len(), 2);
                assert_completion(
                    protocol,
                    &model.requests()[0].json(),
                    &model.requests()[1].json(),
                    true,
                );
                assert_exa_private(&mcp, "如何理解增量游标？");
                close(&mut h).await;
                model.shutdown().await;
                mcp.shutdown().await;
            }
        }
    }
    #[tokio::test]
    async fn invalid_mixed_disabled_and_missing_target_tools_execute_nothing() {
        for protocol in PROTOCOLS {
            let good = call(
                protocol,
                "call-1",
                "web_search",
                json!({"message_id":"message-1"}),
            );
            let unknown = call(
                protocol,
                "call-2",
                "unknown",
                json!({"message_id":"message-1"}),
            );
            let mut cases = vec![
                vec![],
                vec![good.clone(), good.clone()],
                vec![good.clone(), unknown.clone()],
                vec![unknown],
            ];
            for args in [
                json!({}),
                json!({"message_id":"old-message"}),
                json!({"message_id":"message-1","query":SECRET}),
                json!({"message_id":1}),
                json!({"message_id":"message-1","private":SECRET}),
                json!([]),
            ] {
                cases.push(vec![call(protocol, "call-1", "web_search", args)]);
            }
            for id in [
                String::new(),
                "x".repeat(129),
                "bad\nID".to_owned(),
                "🧪".repeat(33),
            ] {
                cases.push(vec![call(
                    protocol,
                    &id,
                    "web_search",
                    json!({"message_id":"message-1"}),
                )]);
            }
            if protocol == ApiProtocol::OpenaiChat {
                let mut bad = good.clone();
                bad["function"]["arguments"] = json!("{broken");
                cases.push(vec![bad]);
                let mut bad = good.clone();
                bad["type"] = json!("other");
                cases.push(vec![bad]);
                let mut bad = good.clone();
                bad["function"]["arguments"] =
                    json!("{\"message_id\":\"message-1\",\"message_id\":\"message-1\"}");
                cases.push(vec![bad]);
            }
            let model = HttpServer::spawn(
                cases
                    .iter()
                    .map(|cs| Scenario::new(Response::json(tools(protocol, cs.clone()))))
                    .chain([
                        Scenario::new(Response::json(one_tool(protocol))),
                        Scenario::new(Response::json(one_tool(protocol))),
                        Scenario::new(Response::json(normal(protocol, Some(&candidate())))),
                    ])
                    .collect(),
            )
            .await;
            let mcp = HttpServer::spawn(vec![]).await;
            let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
            for _ in &cases {
                let out = result(&mut h, search_prompt(0), true).await.unwrap();
                assert!(matches!(out.answer,Answer::Rejected(reason) if !reason.contains(SECRET)));
                assert_eq!(out.search, SearchState::NotRequested);
            }
            let mut disabled = search_prompt(0);
            disabled.web_search = false;
            let mut empty = search_prompt(0);
            empty.search_targets.clear();
            for p in [disabled, empty] {
                assert!(matches!(
                    result(&mut h, p, true).await.unwrap().answer,
                    Answer::Rejected(_)
                ));
            }
            for r in &model.requests()[cases.len()..] {
                assert!(r.json().get("tools").is_none());
            }
            assert!(mcp.requests().is_empty());
            assert!(
                matches!(result(&mut h,search_prompt(0),true).await.unwrap().answer,Answer::Candidates(cs) if cs.len()==1)
            );
            assert_eq!(model.requests().len(), cases.len() + 3);
            close(&mut h).await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn repeated_tools_are_rejected_without_third_model_or_second_search() {
        for protocol in PROTOCOLS {
            let mcp = HttpServer::spawn(vec![
                Scenario::new(mcp_init()),
                Scenario::new(Response::bytes(vec![]).status(202)),
                Scenario::new(mcp_result("https://public.example/source")),
            ])
            .await;
            let model = HttpServer::spawn(vec![
                Scenario::new(Response::json(one_tool(protocol))),
                Scenario::new(Response::json(one_tool(protocol))),
            ])
            .await;
            let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
            let output = result(&mut h, search_prompt(0), true).await.unwrap();
            assert!(matches!(output.answer, Answer::Rejected(_)));
            assert_eq!(output.search, SearchState::Completed);
            assert_eq!(model.requests().len(), 2);
            assert_eq!(mcp.requests().len(), 3);
            close(&mut h).await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn cancellation_stop_and_shared_deadline_cover_search_and_second_model() {
        for protocol in PROTOCOLS {
            for phase in 0..3 {
                for action in 0..3 {
                    let (held, received, release) = Scenario::new(if phase == 0 {
                        mcp_init()
                    } else if phase == 1 {
                        mcp_result("https://public.example/source")
                    } else {
                        Response::json(normal(protocol, Some(&candidate())))
                    })
                    .barrier();
                    let (model_cases, mcp_cases) = match phase {
                        0 => (
                            vec![Scenario::new(Response::json(one_tool(protocol)))],
                            vec![held],
                        ),
                        1 => (
                            vec![Scenario::new(Response::json(one_tool(protocol)))],
                            vec![
                                Scenario::new(
                                    mcp_init().header("mcp-session-id", "cancel-session"),
                                ),
                                Scenario::new(Response::bytes(vec![]).status(202)),
                                held,
                            ],
                        ),
                        _ => (
                            vec![Scenario::new(Response::json(one_tool(protocol))), held],
                            vec![
                                Scenario::new(mcp_init()),
                                Scenario::new(Response::bytes(vec![]).status(202)),
                                Scenario::new(mcp_result("https://public.example/source")),
                            ],
                        ),
                    };
                    let model = HttpServer::spawn(model_cases).await;
                    let mcp = HttpServer::spawn(mcp_cases).await;
                    let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(2));
                    let mut p = search_prompt(0);
                    if action == 2 {
                        p.deadline = Instant::now() + Duration::from_millis(200);
                    }
                    h.prompts.send(p).await.unwrap();
                    assert!(matches!(event(&mut h).await, Event::Started { .. }));
                    timeout(Duration::from_secs(1), received)
                        .await
                        .unwrap()
                        .unwrap();
                    if action == 0 {
                        h.cancel();
                    } else if action == 1 {
                        h.stop();
                    }
                    let output = match event(&mut h).await {
                        Event::Answer {
                            result: Ok(out), ..
                        } => out,
                        e => panic!("unexpected {e:?}"),
                    };
                    assert!(if action == 2 {
                        matches!(output.answer, Answer::Expired)
                    } else {
                        matches!(output.answer, Answer::Cancelled)
                    });
                    assert_eq!(
                        output.search,
                        if phase == 2 {
                            SearchState::Completed
                        } else {
                            SearchState::Unavailable
                        }
                    );
                    let _ = release.send(());
                    if action == 1 {
                        timeout(Duration::from_secs(1), &mut h.task)
                            .await
                            .unwrap()
                            .unwrap();
                        assert!(matches!(event(&mut h).await, Event::Closed(Ok(()))));
                    } else {
                        close(&mut h).await;
                    }
                    assert_eq!(model.requests().len(), if phase == 2 { 2 } else { 1 });
                    assert_eq!(mcp.requests().len(), if phase == 0 { 1 } else { 3 });
                    assert!(!mcp.requests().iter().any(|r| r.method == "DELETE"));
                    model.shutdown().await;
                    mcp.shutdown().await;
                }
            }
        }
    }
    #[tokio::test]
    async fn normal_stop_with_valid_tool_and_refusal_never_executes_search() {
        for protocol in PROTOCOLS {
            let mut stopped = one_tool(protocol);
            match protocol {
                ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                ApiProtocol::OpenaiChat => {
                    stopped["choices"][0]["finish_reason"] = json!("stop");
                    stopped["choices"][0]["message"]["content"] = json!(candidate());
                }
                ApiProtocol::AnthropicMessages => {
                    stopped["stop_reason"] = json!("end_turn");
                    stopped["content"][0]["text"] = json!(candidate());
                }
            }
            let mut refused = one_tool(protocol);
            match protocol {
                ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                ApiProtocol::OpenaiChat => {
                    refused["choices"][0]["message"]["refusal"] = json!(SECRET)
                }
                ApiProtocol::AnthropicMessages => refused["stop_reason"] = json!("refusal"),
            }
            let model = HttpServer::spawn(vec![
                Scenario::new(Response::json(stopped)),
                Scenario::new(Response::json(refused)),
                Scenario::new(Response::json(normal(protocol, Some(&candidate())))),
            ])
            .await;
            let mcp = HttpServer::spawn(vec![]).await;
            let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
            for _ in 0..2 {
                let output = result(&mut h, search_prompt(0), true).await.unwrap();
                assert!(matches!(output.answer, Answer::Rejected(_)));
                assert_eq!(output.search, SearchState::NotRequested);
            }
            let output = result(&mut h, search_prompt(0), true).await.unwrap();
            assert!(matches!(output.answer, Answer::Candidates(_)));
            assert_eq!(output.search, SearchState::NotRequested);
            assert!(mcp.requests().is_empty());
            assert_eq!(model.requests().len(), 3);
            close(&mut h).await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn search_time_budget_failure_still_gets_one_final_completion() {
        for protocol in PROTOCOLS {
            let (held, received, release) = Scenario::new(mcp_init()).barrier();
            let mcp = HttpServer::spawn(vec![held]).await;
            let model = HttpServer::spawn(vec![
                Scenario::new(Response::json(one_tool(protocol))),
                Scenario::new(Response::json(normal(protocol, None))),
            ])
            .await;
            let mut h = search_handle(&model, protocol, &mcp, Duration::from_millis(100));
            h.prompts.send(search_prompt(0)).await.unwrap();
            assert!(matches!(event(&mut h).await, Event::Started { .. }));
            timeout(Duration::from_secs(1), received)
                .await
                .unwrap()
                .unwrap();
            let out = match event(&mut h).await {
                Event::Answer {
                    result: Ok(out), ..
                } => out,
                e => panic!("unexpected {e:?}"),
            };
            assert!(matches!(out.answer, Answer::NoMessage));
            assert_eq!(out.search, SearchState::Unavailable);
            assert_completion(
                protocol,
                &model.requests()[0].json(),
                &model.requests()[1].json(),
                true,
            );
            let _ = release.send(());
            close(&mut h).await;
            assert_eq!(mcp.requests().len(), 1);
            assert_eq!(model.requests().len(), 2);
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn maximum_unicode_query_and_source_keep_completion_result_within_16kib() {
        for protocol in PROTOCOLS {
            let source = format!("https://public.example/source\n{}", "🧪".repeat(4000));
            let query = "🧪".repeat(240);
            let mut p = search_prompt(0);
            p.search_targets[0].query = query.clone();
            let mcp = HttpServer::spawn(vec![
                Scenario::new(mcp_init()),
                Scenario::new(Response::bytes(vec![]).status(202)),
                Scenario::new(mcp_result(&source)),
            ])
            .await;
            let model = HttpServer::spawn(vec![
                Scenario::new(Response::json(one_tool(protocol))),
                Scenario::new(Response::json(normal(protocol, None))),
            ])
            .await;
            let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
            assert_eq!(
                result(&mut h, p, true).await.unwrap().search,
                SearchState::Completed
            );
            let body = model.requests()[1].json();
            let text = match protocol {
                ApiProtocol::ChatgptSubscription => unreachable!("two-Key protocol fixture"),
                ApiProtocol::OpenaiChat => body["messages"][3]["content"].as_str().unwrap(),
                ApiProtocol::AnthropicMessages => body["messages"][2]["content"][0]["content"]
                    .as_str()
                    .unwrap(),
            };
            assert!((16 * 1024 - 3..=16 * 1024).contains(&text.len()));
            assert!(text.contains(&query));
            assert!(text.contains("https://public.example/source"));
            assert_exa_private(&mcp, &query);
            close(&mut h).await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn completion_context_budget_includes_fixed_search_budget_instruction() {
        for protocol in PROTOCOLS {
            let mcp = HttpServer::spawn(vec![
                Scenario::new(mcp_init()),
                Scenario::new(Response::bytes(vec![]).status(202)),
                Scenario::new(mcp_result("https://public.example/source")),
            ])
            .await;
            let model =
                HttpServer::spawn(vec![Scenario::new(Response::json(one_tool(protocol)))]).await;
            let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
            let mut p = search_prompt(0);
            p.text = "A".repeat(CONTEXT_LIMIT - crate::workspace::CONTRACT.len());
            let error = result(&mut h, p, true).await.unwrap_err();
            assert_eq!(error, ApiError::Configuration("应用上下文超限"));
            assert_clean(&error);
            assert_eq!(model.requests().len(), 1);
            assert_eq!(mcp.requests().len(), 3);
            close(&mut h).await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn second_model_http_failure_is_not_search_unavailable_and_never_retries() {
        for protocol in PROTOCOLS {
            for status in [401, 429, 500] {
                let mcp = HttpServer::spawn(vec![
                    Scenario::new(mcp_init()),
                    Scenario::new(Response::bytes(vec![]).status(202)),
                    Scenario::new(mcp_result("https://public.example/source")),
                ])
                .await;
                let model = HttpServer::spawn(vec![
                    Scenario::new(Response::json(one_tool(protocol))),
                    Scenario::new(Response::json(json!({"error":SECRET})).status(status)),
                ])
                .await;
                let mut h = search_handle(&model, protocol, &mcp, Duration::from_secs(1));
                let error = result(&mut h, search_prompt(0), true).await.unwrap_err();
                assert_eq!(error, ApiError::Http(status));
                assert_clean(&error);
                close(&mut h).await;
                assert_eq!(model.requests().len(), 2);
                assert_eq!(mcp.requests().len(), 3);
                model.shutdown().await;
                mcp.shutdown().await;
            }
        }
    }

    fn subscription_config() -> ApiConfig {
        ApiConfig {
            protocol: ApiProtocol::ChatgptSubscription,
            model: "account-model".into(),
            chatgpt_account: Some(subscription::FIXTURE_ACCOUNT.into()),
            ..ApiConfig::default()
        }
    }
    fn subscription_call() -> Value {
        json!({"id":"fc_1","type":"function_call","namespace":"shisui_danmu","name":"web_search",
            "status":"completed","call_id":"call_1","arguments":json!({"message_id":"message-1"}).to_string()})
    }
    fn subscription_message(text: &str) -> Value {
        json!({"id":"msg_1","type":"message","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":text,"annotations":[]}]})
    }
    fn subscription_completed(output: Vec<Value>) -> Response {
        let event = json!({"type":"response.completed","sequence_number":1,
            "response":{"id":"resp_1","object":"response","status":"completed","model":"actual-subscription-model","output":output,"error":null}});
        // Split both SSE field names and UTF-8; multiline data must remain one JSON event.
        let data = serde_json::to_string_pretty(&event)
            .unwrap()
            .lines()
            .map(|line| format!("data: {line}\r\n"))
            .collect::<String>();
        Response::chunked(
            format!("event: response.completed\r\n{data}\r\n")
                .as_bytes()
                .chunks(3)
                .map(<[u8]>::to_vec)
                .collect(),
        )
        .header("content-type", "text/event-stream")
    }
    fn subscription_handle(
        root: &tempfile::TempDir,
        auth: &HttpServer,
        model: &HttpServer,
        mcp: &HttpServer,
        expired: bool,
    ) -> Handle {
        let session = subscription::fixture_session(
            &root.path().join("assistant.json"),
            &auth.url("/"),
            expired,
            true,
        )
        .unwrap();
        let search = search::Client::with_endpoint(
            &mcp.url("/mcp?tools=web_search_exa"),
            Duration::from_secs(1),
        )
        .unwrap();
        Handle::spawn_subscription_for_test(
            subscription_config(),
            session,
            search,
            &model.url("/v1/responses"),
        )
        .unwrap()
    }
    fn refresh_scenarios(auth: &HttpServer) {
        let origin = auth.url("/").trim_end_matches('/').to_owned();
        auth.push(Scenario::new(Response::json(
            subscription::fixture_discovery(&origin),
        )));
        auth.push(Scenario::new(Response::json(subscription::fixture_jwks())));
        auth.push(Scenario::respond(|request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/token");
            let form: std::collections::BTreeMap<_, _> =
                url::form_urlencoded::parse(&request.body).collect();
            assert_eq!(form["grant_type"], "refresh_token");
            assert_eq!(form["client_id"], "oaiapp_fixture");
            assert_eq!(form["refresh_token"], "refresh-1");
            assert_eq!(form["resource"], "https://api.openai.com/v1");
            assert!(!form.contains_key("client_secret"));
            Response::json(subscription::fixture_token_response("refresh-2"))
        }));
    }
    fn assert_subscription_requests(model: &HttpServer, prompts: &[&Prompt]) {
        let requests = model.requests();
        assert_eq!(requests.len(), prompts.len());
        let access = requests[0]
            .header("authorization")
            .unwrap()
            .strip_prefix("Bearer ")
            .unwrap()
            .to_owned();
        let parts: Vec<_> = access.split('.').collect();
        assert_eq!(parts.len(), 3);
        use base64::Engine;
        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["sub"], "fixture-subject");
        assert_eq!(claims["client_id"], "oaiapp_fixture");
        for (request, prompt) in requests.iter().zip(prompts) {
            assert_eq!(request.path, "/v1/responses");
            assert_eq!(request.method, "POST");
            assert!(
                request
                    .header("authorization")
                    .is_some_and(|header| header == format!("Bearer {access}"))
            );
            assert!(request.header("cookie").is_none());
            assert!(request.header("x-api-key").is_none());
            let body = request.json();
            assert_eq!(body["store"], false);
            assert_eq!(body["stream"], true);
            assert_eq!(body["model"], "account-model");
            assert_eq!(
                body["input"][0],
                json!({"role":"user","content":prompt.text})
            );
            for forbidden in [
                "previous_response_id",
                "conversation",
                "max_output_tokens",
                "max_completion_tokens",
                "max_tokens",
                "max_tool_calls",
                "temperature",
            ] {
                assert!(body.get(forbidden).is_none());
            }
            assert!(!String::from_utf8_lossy(&request.body).contains(&access));
        }
    }
    #[tokio::test]
    async fn subscription_refresh_is_inside_stateless_worker_not_spawn() {
        let root = tempfile::tempdir().unwrap();
        let auth = HttpServer::spawn(vec![]).await;
        refresh_scenarios(&auth);
        let model = HttpServer::spawn(vec![
            Scenario::new(subscription_completed(vec![subscription_message(
                &candidate(),
            )])),
            Scenario::new(subscription_completed(vec![])),
        ])
        .await;
        let mcp = HttpServer::spawn(vec![]).await;
        let mut h = subscription_handle(&root, &auth, &model, &mcp, true);
        tokio::task::yield_now().await;
        assert!(auth.requests().is_empty());
        assert!(model.requests().is_empty());
        let first = prompt(0);
        let out = result(&mut h, prompt(0), true).await.unwrap();
        assert!(matches!(out.answer,Answer::Candidates(cs) if cs.len()==1));
        assert_eq!(
            out.returned_model.as_deref(),
            Some("actual-subscription-model")
        );
        assert_eq!(out.search, SearchState::NotRequested);
        let mut followup = prompt(0);
        followup.text = json!({"operator_context":"完整资料","previous_question":"前问","actual_delivery_results":{"status":"confirmed","text":"真实送达"},"current":"追问"}).to_string();
        assert!(matches!(
            result(
                &mut h,
                Prompt {
                    text: followup.text.clone(),
                    ..prompt(0)
                },
                true
            )
            .await
            .unwrap()
            .answer,
            Answer::NoMessage
        ));
        assert_eq!(auth.requests().len(), 3);
        assert_subscription_requests(&model, &[&first, &followup]);
        assert!(mcp.requests().is_empty());
        close(&mut h).await;
        auth.shutdown().await;
        model.shutdown().await;
        mcp.shutdown().await;
    }
    #[tokio::test]
    async fn subscription_search_is_one_frozen_query_and_at_most_two_responses() {
        for (available, second_tool) in [(true, false), (false, false), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let auth = HttpServer::spawn(vec![]).await;
            let model = HttpServer::spawn(vec![
                Scenario::new(subscription_completed(vec![
                    subscription_message(SECRET),
                    subscription_call(),
                ])),
                Scenario::new(subscription_completed(if second_tool {
                    vec![subscription_call()]
                } else {
                    vec![subscription_message(&candidate())]
                })),
            ])
            .await;
            let mcp = HttpServer::spawn(if available {
                vec![
                    Scenario::new(mcp_init()),
                    Scenario::new(Response::bytes(vec![]).status(202)),
                    Scenario::new(mcp_result("https://public.example/source 公共资料")),
                ]
            } else {
                vec![Scenario::new(
                    Response::json(json!({"error":SECRET})).status(429),
                )]
            })
            .await;
            let mut h = subscription_handle(&root, &auth, &model, &mcp, false);
            let p = search_prompt(0);
            let out = result(&mut h, search_prompt(0), true).await.unwrap();
            assert_eq!(
                out.search,
                if available {
                    SearchState::Completed
                } else {
                    SearchState::Unavailable
                }
            );
            if second_tool {
                assert!(matches!(out.answer, Answer::Rejected(_)));
            } else {
                assert!(matches!(out.answer,Answer::Candidates(cs) if cs.len()==1));
            }
            assert_subscription_requests(&model, &[&p, &p]);
            let requests = model.requests();
            let first = requests[0].json();
            let second = requests[1].json();
            assert_eq!(first["tools"][0]["type"], "namespace");
            assert_eq!(
                first["tools"][0]["tools"][0]["parameters"]["properties"]["message_id"]["enum"],
                json!(["message-1"])
            );
            assert!(second.get("tools").is_none());
            assert!(second.get("parallel_tool_calls").is_none());
            assert_eq!(second["input"].as_array().unwrap().len(), 3);
            assert_eq!(second["input"][1], subscription_call_without_item_fields());
            assert_eq!(second["input"][2]["call_id"], "call_1");
            let tool_result = second["input"][2]["output"].as_str().unwrap();
            assert!(!tool_result.contains(SECRET));
            assert!(tool_result.len() <= 16 * 1024);
            if available {
                assert_exa_private(&mcp, "如何理解增量游标？");
                assert!(tool_result.contains("https://public.example/source"));
            } else {
                assert_eq!(tool_result, SEARCH_UNAVAILABLE);
                assert_eq!(mcp.requests().len(), 1);
            }
            close(&mut h).await;
            assert_eq!(model.requests().len(), 2);
            assert!(auth.requests().is_empty());
            auth.shutdown().await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    fn subscription_call_without_item_fields() -> Value {
        json!({"type":"function_call","namespace":"shisui_danmu","name":"web_search","call_id":"call_1","arguments":json!({"message_id":"message-1"}).to_string()})
    }
    #[tokio::test]
    async fn subscription_invalid_tools_execute_zero_search_and_worker_keeps_next_question() {
        let mut unknown = subscription_call();
        unknown["name"] = json!("unknown");
        let mut old = subscription_call();
        old["arguments"] = json!(json!({"message_id":"old-message"}).to_string());
        let mut extra = subscription_call();
        extra["arguments"] = json!(json!({"message_id":"message-1","query":SECRET}).to_string());
        for (calls, enabled, targets) in [
            (vec![subscription_call()], false, true),
            (vec![subscription_call()], true, false),
            (vec![subscription_call(), subscription_call()], true, true),
            (vec![unknown], true, true),
            (vec![old], true, true),
            (vec![extra], true, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let auth = HttpServer::spawn(vec![]).await;
            let mcp = HttpServer::spawn(vec![]).await;
            let model = HttpServer::spawn(vec![
                Scenario::new(subscription_completed(calls)),
                Scenario::new(subscription_completed(vec![subscription_message(
                    &candidate(),
                )])),
            ])
            .await;
            let mut h = subscription_handle(&root, &auth, &model, &mcp, false);
            let mut p = search_prompt(0);
            p.web_search = enabled;
            if !targets {
                p.search_targets.clear();
            }
            let out = result(&mut h, p, true).await.unwrap();
            assert!(matches!(out.answer, Answer::Rejected(_)));
            assert_eq!(out.search, SearchState::NotRequested);
            assert!(matches!(
                result(&mut h, prompt(0), true).await.unwrap().answer,
                Answer::Candidates(_)
            ));
            assert!(mcp.requests().is_empty());
            assert!(auth.requests().is_empty());
            assert_eq!(model.requests().len(), 2);
            close(&mut h).await;
            auth.shutdown().await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn subscription_http_and_sse_failures_are_safe_and_never_fall_back() {
        for response in [Response::json(json!({"error":{"code":"subscription_sharing_usage_limit_exceeded","message":SECRET}})).status(429).header("x-request-id","req_safe-17"),
            Response::json(json!({"error":{"code":SECRET,"message":SECRET}})).status(503),
            Response::json(json!({"error":SECRET})).status(401),Response::json(json!({"error":SECRET})).status(403),
            Response::bytes(format!("data: {}\n\n",json!({"type":"response.failed","sequence_number":1,"response":{"id":"resp_1","object":"response","status":"failed","error":{"code":"subscription_sharing_usage_unavailable","message":SECRET}}})).into_bytes()).header("content-type","text/event-stream"),
            Response::bytes(b"data: broken\n\n".to_vec()).header("content-type","text/event-stream"),Response::disconnect()] {
            let root=tempfile::tempdir().unwrap(); let auth=HttpServer::spawn(vec![]).await; let mcp=HttpServer::spawn(vec![]).await;
            let model=HttpServer::spawn(vec![Scenario::new(response)]).await;
            let mut h=subscription_handle(&root,&auth,&model,&mcp,false);
            let error=result(&mut h,search_prompt(0),true).await.unwrap_err(); assert_clean(&error);
            for request in model.requests() {
                let token = request.header("authorization").unwrap().strip_prefix("Bearer ").unwrap();
                assert!(!format!("{error:#} {error:?}").contains(token));
            }
            if let ApiError::Subscription { status: Some(status), code, request_id } = &error {
                match status {
                    429 => { assert_eq!(*code,Some("subscription_sharing_usage_limit_exceeded")); assert_eq!(request_id.as_deref(),Some("req_safe-17")); }
                    503 => { assert!(code.is_none()); assert!(request_id.is_none()); }
                    401|403 => { assert!(code.is_none()); }
                    _=>panic!("unexpected mapped HTTP status"),
                }
            }
            if matches!(error,ApiError::Subscription {code:Some("subscription_sharing_usage_limit_exceeded" | "subscription_sharing_usage_unavailable"),..}) {
                assert!(error.to_string().contains("https://chatgpt.com/settings/usage"));
            }
            close(&mut h).await; assert_eq!(model.requests().len(),1); assert!(mcp.requests().is_empty()); assert!(auth.requests().is_empty());
            auth.shutdown().await; model.shutdown().await; mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn subscription_missing_scope_or_credentials_and_wrong_backend_never_send_http() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.json");
        let auth = HttpServer::spawn(vec![]).await;
        let model = HttpServer::spawn(vec![]).await;
        let mcp = HttpServer::spawn(vec![]).await;
        assert!(subscription::session(&path, subscription::FIXTURE_ACCOUNT).is_err());
        assert!(subscription::fixture_session(&path, &auth.url("/"), false, false).is_err());
        assert!(Handle::spawn(subscription_config(), ApiKey::new(KEY).unwrap()).is_err());
        let session = subscription::fixture_session(&path, &auth.url("/"), false, true).unwrap();
        assert!(
            Handle::spawn_subscription(
                ApiConfig {
                    model: "wrong".into(),
                    ..ApiConfig::default()
                },
                session
            )
            .is_err()
        );
        let session = subscription::fixture_session(&path, &auth.url("/"), false, true).unwrap();
        let search = search::Client::with_endpoint(
            &mcp.url("/mcp?tools=web_search_exa"),
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(
            Handle::spawn_subscription_for_test(
                subscription_config(),
                session,
                search,
                "https://example.test/v1/responses"
            )
            .is_err()
        );
        let session = subscription::fixture_session(&path, &auth.url("/"), false, true).unwrap();
        let mut wrong_account = subscription_config();
        wrong_account.chatgpt_account = Some("another-local-account".into());
        assert!(Handle::spawn_subscription(wrong_account, session).is_err());
        let session = subscription::fixture_session(&path, &auth.url("/"), false, true).unwrap();
        let mut wrong_url = subscription_config();
        wrong_url.base_url = model.base_url();
        assert!(Handle::spawn_subscription(wrong_url, session).is_err());
        let mut h = subscription_handle(&root, &auth, &model, &mcp, false);
        // The real session re-reads protected storage before any request; deletion is not a fallback to Key auth.
        std::fs::remove_file(root.path().join("assistant-secrets/chatgpt.json")).unwrap();
        let error = result(&mut h, prompt(0), true).await.unwrap_err();
        assert!(matches!(error, ApiError::Configuration(_)));
        assert_clean(&error);
        close(&mut h).await;
        assert!(model.requests().is_empty());
        assert!(mcp.requests().is_empty());
        assert!(auth.requests().is_empty());
        auth.shutdown().await;
        model.shutdown().await;
        mcp.shutdown().await;
    }

    #[tokio::test]
    async fn subscription_cancellation_and_deadline_cover_refresh_sse_search_and_completion() {
        for stage in ["refresh", "response", "search", "completion"] {
            for cancel in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let auth = HttpServer::spawn(vec![]).await;
                let model = HttpServer::spawn(vec![]).await;
                let mcp = HttpServer::spawn(vec![]).await;
                let (held, received, release) = Scenario::new(if stage == "refresh" {
                    Response::json(subscription::fixture_token_response("refresh-2"))
                } else if stage == "search" {
                    mcp_init()
                } else {
                    subscription_completed(vec![subscription_message(&candidate())])
                })
                .barrier();
                match stage {
                    "refresh" => {
                        let origin = auth.url("/").trim_end_matches('/').to_owned();
                        auth.push(Scenario::new(Response::json(
                            subscription::fixture_discovery(&origin),
                        )));
                        auth.push(Scenario::new(Response::json(subscription::fixture_jwks())));
                        auth.push(held);
                    }
                    "response" => model.push(held),
                    "search" => {
                        model.push(Scenario::new(subscription_completed(vec![
                            subscription_call(),
                        ])));
                        mcp.push(held);
                    }
                    "completion" => {
                        model.push(Scenario::new(subscription_completed(vec![
                            subscription_call(),
                        ])));
                        model.push(held);
                        mcp.push(Scenario::new(mcp_init()));
                        mcp.push(Scenario::new(Response::bytes(vec![]).status(202)));
                        mcp.push(Scenario::new(mcp_result("https://public.example/source")));
                    }
                    _ => unreachable!(),
                }
                let mut h = subscription_handle(&root, &auth, &model, &mcp, stage == "refresh");
                let mut p = search_prompt(0);
                if !cancel {
                    p.deadline = Instant::now() + Duration::from_millis(250);
                }
                h.prompts.send(p).await.unwrap();
                assert!(matches!(event(&mut h).await, Event::Started { .. }));
                timeout(Duration::from_secs(2), received)
                    .await
                    .unwrap()
                    .unwrap();
                if cancel {
                    h.cancel();
                }
                let Event::Answer {
                    result: Ok(out), ..
                } = event(&mut h).await
                else {
                    panic!("missing interrupted answer");
                };
                if cancel {
                    assert!(matches!(out.answer, Answer::Cancelled));
                } else {
                    assert!(matches!(out.answer, Answer::Expired));
                }
                assert_eq!(
                    out.search,
                    match stage {
                        "search" => SearchState::Unavailable,
                        "completion" => SearchState::Completed,
                        _ => SearchState::NotRequested,
                    }
                );
                let _ = release.send(());
                close(&mut h).await;
                assert_eq!(
                    model.requests().len(),
                    match stage {
                        "refresh" => 0,
                        "completion" => 2,
                        _ => 1,
                    }
                );
                assert_eq!(
                    mcp.requests().len(),
                    match stage {
                        "search" => 1,
                        "completion" => 3,
                        _ => 0,
                    }
                );
                assert_eq!(
                    auth.requests().len(),
                    if stage == "refresh" { 3 } else { 0 }
                );
                auth.shutdown().await;
                model.shutdown().await;
                mcp.shutdown().await;
            }
        }
    }
    #[tokio::test]
    async fn subscription_queued_cancel_or_expiry_never_refreshes_or_requests_second_question() {
        for cancel in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let auth = HttpServer::spawn(vec![]).await;
            let mcp = HttpServer::spawn(vec![]).await;
            let (held, received, release) = Scenario::new(subscription_completed(vec![])).barrier();
            let model = HttpServer::spawn(vec![held]).await;
            let mut h = subscription_handle(&root, &auth, &model, &mcp, false);
            h.prompts.send(prompt(0)).await.unwrap();
            assert!(matches!(event(&mut h).await, Event::Started { .. }));
            timeout(Duration::from_secs(2), received)
                .await
                .unwrap()
                .unwrap();
            let mut queued = prompt(0);
            queued.round_id = "queued".into();
            queued.deadline = Instant::now() + Duration::from_millis(30);
            let deadline = queued.deadline;
            h.prompts.send(queued).await.unwrap();
            if cancel {
                h.cancel();
            } else {
                tokio::time::sleep_until(deadline.into()).await;
            }
            release.send(()).unwrap();
            let Event::Answer {
                result: Ok(first), ..
            } = event(&mut h).await
            else {
                panic!("missing first answer");
            };
            if cancel {
                assert!(matches!(first.answer, Answer::Cancelled));
            } else {
                assert!(matches!(first.answer, Answer::NoMessage));
            }
            let Event::Answer {
                round_id,
                result: Ok(queued),
                ..
            } = event(&mut h).await
            else {
                panic!("queued request unexpectedly started");
            };
            assert_eq!(round_id, "queued");
            if cancel {
                assert!(matches!(queued.answer, Answer::Cancelled));
            } else {
                assert!(matches!(queued.answer, Answer::Expired));
            }
            close(&mut h).await;
            assert_eq!(model.requests().len(), 1);
            assert!(auth.requests().is_empty());
            assert!(mcp.requests().is_empty());
            auth.shutdown().await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn subscription_refresh_failures_report_known_codes_without_model_request() {
        for response in [
            Response::json(json!({"error":"invalid_grant","error_description":SECRET})).status(400),
            Response::json(json!({"error":SECRET,"error_description":SECRET})).status(503),
        ] {
            let root = tempfile::tempdir().unwrap();
            let auth = HttpServer::spawn(vec![]).await;
            let origin = auth.url("/").trim_end_matches('/').to_owned();
            auth.push(Scenario::new(Response::json(
                subscription::fixture_discovery(&origin),
            )));
            auth.push(Scenario::new(Response::json(subscription::fixture_jwks())));
            auth.push(Scenario::new(response));
            let model = HttpServer::spawn(vec![]).await;
            let mcp = HttpServer::spawn(vec![]).await;
            let mut h = subscription_handle(&root, &auth, &model, &mcp, true);
            let error = result(&mut h, prompt(0), true).await.unwrap_err();
            assert_clean(&error);
            assert!(matches!(
                error,
                ApiError::Subscription {
                    code: Some("invalid_grant"),
                    ..
                } | ApiError::Subscription {
                    status: Some(503),
                    code: None,
                    ..
                }
            ));
            close(&mut h).await;
            assert_eq!(auth.requests().len(), 3);
            assert!(model.requests().is_empty());
            assert!(mcp.requests().is_empty());
            auth.shutdown().await;
            model.shutdown().await;
            mcp.shutdown().await;
        }
    }
    #[tokio::test]
    async fn subscription_scope_revoked_after_spawn_does_not_use_snapshot_token() {
        let root = tempfile::tempdir().unwrap();
        let auth = HttpServer::spawn(vec![]).await;
        let model = HttpServer::spawn(vec![]).await;
        let mcp = HttpServer::spawn(vec![]).await;
        let mut h = subscription_handle(&root, &auth, &model, &mcp, false);
        assert!(
            subscription::fixture_session(
                &root.path().join("assistant.json"),
                &auth.url("/"),
                false,
                false
            )
            .is_err()
        );
        let error = result(&mut h, prompt(0), true).await.unwrap_err();
        assert_eq!(
            error,
            ApiError::Subscription {
                status: None,
                code: Some("chatpass_v2_scope_not_authorized"),
                request_id: None
            }
        );
        assert_clean(&error);
        close(&mut h).await;
        assert!(auth.requests().is_empty());
        assert!(model.requests().is_empty());
        assert!(mcp.requests().is_empty());
        auth.shutdown().await;
        model.shutdown().await;
        mcp.shutdown().await;
    }

    #[tokio::test]
    async fn subscription_partial_sse_never_admits_deltas_before_cancel_deadline_or_stop() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            sync::oneshot,
        };
        for action in ["cancel", "expire", "stop"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
            let (sent, received) = oneshot::channel();
            let (release, resume) = oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 1024];
                let (header_end, length) = loop {
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    assert!(bytes.len() <= REQUEST_LIMIT + 16 * 1024);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        break (end + 4, length);
                    }
                };
                while bytes.len() < header_end + length {
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    assert!(bytes.len() <= REQUEST_LIMIT + 16 * 1024);
                }
                let created = json!({"type":"response.created","sequence_number":1,"response":{"id":"resp_1","object":"response","status":"in_progress"}});
                let delta = json!({"type":"response.output_text.delta","sequence_number":2,"response_id":"resp_1","delta":candidate()});
                let prefix = format!("data: {created}\n\ndata: {delta}\n\n");
                socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
                socket
                    .write_all(format!("{:x}\r\n{prefix}\r\n", prefix.len()).as_bytes())
                    .await
                    .unwrap();
                socket.flush().await.unwrap();
                sent.send(()).unwrap();
                let _ = resume.await;
                let _ = socket.write_all(b"0\r\n\r\n").await;
            });
            let root = tempfile::tempdir().unwrap();
            let auth = HttpServer::spawn(vec![]).await;
            let mcp = HttpServer::spawn(vec![]).await;
            let session = subscription::fixture_session(
                &root.path().join("assistant.json"),
                &auth.url("/"),
                false,
                true,
            )
            .unwrap();
            let search = search::Client::with_endpoint(
                &mcp.url("/mcp?tools=web_search_exa"),
                Duration::from_secs(1),
            )
            .unwrap();
            let mut h = Handle::spawn_subscription_for_test(
                subscription_config(),
                session,
                search,
                &endpoint,
            )
            .unwrap();
            let mut p = prompt(0);
            if action == "expire" {
                p.deadline = Instant::now() + Duration::from_millis(100);
            }
            h.prompts.send(p).await.unwrap();
            assert!(matches!(event(&mut h).await, Event::Started { .. }));
            timeout(Duration::from_secs(2), received)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                h.events.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            match action {
                "cancel" => h.cancel(),
                "stop" => h.stop(),
                "expire" => {}
                _ => unreachable!(),
            };
            let Event::Answer {
                result: Ok(out), ..
            } = event(&mut h).await
            else {
                panic!("partial stream became an answer");
            };
            if action == "expire" {
                assert!(matches!(out.answer, Answer::Expired));
            } else {
                assert!(matches!(out.answer, Answer::Cancelled));
            }
            assert_eq!(out.search, SearchState::NotRequested);
            close(&mut h).await;
            let _ = release.send(());
            timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap();
            assert!(auth.requests().is_empty());
            assert!(mcp.requests().is_empty());
            auth.shutdown().await;
            mcp.shutdown().await;
        }
    }

    #[tokio::test]
    async fn subscription_content_rejection_is_atomic_and_next_round_needs_no_restart() {
        for bad in [SECRET.to_owned(),json!({"round_id":"old","candidates":[]}).to_string(),
            json!({"round_id":"round-1","candidates":[{"message_id":"message-1","text":"合法"},{"message_id":"old-message","text":"越权"}]}).to_string(),
            json!({"round_id":"round-1","candidates":[{"message_id":"message-1","text":"合法"},{"message_id":"message-1","text":"重复"}]}).to_string(),
            json!({"round_id":"round-1","candidates":[{"message_id":"message-1","text":""}]}).to_string()] {
            let root=tempfile::tempdir().unwrap(); let auth=HttpServer::spawn(vec![]).await; let mcp=HttpServer::spawn(vec![]).await;
            let next=json!({"round_id":"round-next","candidates":[{"message_id":"message-next","text":"新题合法答复"}]}).to_string();
            let model=HttpServer::spawn(vec![Scenario::new(subscription_completed(vec![subscription_message(&bad)])),Scenario::new(subscription_completed(vec![subscription_message(&next)]))]).await;
            let mut h=subscription_handle(&root,&auth,&model,&mcp,false);
            let rejected=result(&mut h,prompt(0),true).await.unwrap();
            let Answer::Rejected(reason)=rejected.answer else {panic!("invalid batch admitted");};
            assert!(!reason.contains(SECRET)); assert_eq!(rejected.search,SearchState::NotRequested);
            let mut p=prompt(0); p.round_id="round-next".into(); p.message_ids=Arc::from(["message-next".into()]); p.text="完整资料与下一条新题".into();
            let out=result(&mut h,p,true).await.unwrap();
            assert!(matches!(out.answer,Answer::Candidates(cs) if cs.len()==1 && cs[0].message_id=="message-next"));
            close(&mut h).await; assert_eq!(model.requests().len(),2); assert!(auth.requests().is_empty()); assert!(mcp.requests().is_empty());
            auth.shutdown().await; model.shutdown().await; mcp.shutdown().await;
        }
    }
}
