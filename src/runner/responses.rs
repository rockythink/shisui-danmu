//! Official ChatGPT-plan Responses transport. No provider session or partial output is retained.
use super::{
    model::{self, Answer, Prompt},
    settings::{ApiConfig, normalize_base_url},
};
use reqwest::{
    Client,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue},
};
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, SeqAccess, Visitor},
};
use serde_json::{Value, json, value::RawValue};
use std::time::Duration;

const BASE: &str = "https://api.openai.com/v1";
const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const MODELS_URL: &str = "https://api.openai.com/v1/models";
const RESPONSE_LIMIT: usize = 256 * 1024;
const SSE_EVENT_LIMIT: usize = 256 * 1024;
const SSE_STREAM_LIMIT: usize = 8 * 1024 * 1024;
const SSE_DATA_EVENT_LIMIT: usize = 4096;
const MODEL_CATALOG_LIMIT: usize = 8 * 1024 * 1024;
const REQUEST_LIMIT: usize = 48 * 1024;
const CONTEXT_LIMIT: usize = 24 * 1024;
const SEARCH_LIMIT: usize = 16 * 1024;
const SEARCH_DESCRIPTION: &str = "用指定本批完整公开问题搜索；不能提供query或扩写私有上下文。";
const BUDGET_USED: &str = "\n搜索预算已用完，只返回最终候选JSON。";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ResponseError {
    #[error("ChatGPT HTTP状态：{status}；代码：{code:?}；请求ID：{request_id:?}")]
    Http {
        status: u16,
        code: Option<&'static str>,
        request_id: Option<String>,
    },
    #[error("ChatGPT响应失败；代码：{code:?}；请求ID：{request_id:?}")]
    Failed {
        code: Option<&'static str>,
        request_id: Option<String>,
    },
    #[error("ChatGPT传输错误：{0}")]
    Transport(&'static str),
    #[error("ChatGPT协议错误：{0}")]
    Protocol(&'static str),
    #[error("ChatGPT配置错误：{0}")]
    Configuration(&'static str),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelChoice {
    pub slug: String,
    pub display_name: String,
}
#[derive(Debug)]
pub(crate) struct ResponseTurn {
    pub result: ResponseResult,
    pub returned_model: Option<String>,
}
#[derive(Debug)]
pub(crate) enum ResponseResult {
    Answer(Answer),
    ToolCall(ToolCall),
}
// Only the terminal admission parser creates calls. Continuation cannot carry arbitrary output items.
#[derive(Debug)]
pub(crate) struct ToolCall {
    call_id: String,
    message_id: String,
}
impl ToolCall {
    pub(crate) fn message_id(&self) -> &str {
        &self.message_id
    }
    pub(crate) fn into_continuation(self, result: String) -> Continuation {
        Continuation { call: self, result }
    }
}
pub(crate) struct Continuation {
    call: ToolCall,
    result: String,
}

/// Caller owns cancellation/deadline and must reuse this redirect/retry-free client policy.
pub(crate) fn client() -> Result<Client, ResponseError> {
    let builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10));
    #[cfg(any(test, feature = "test-support"))]
    let builder = if TEST_API_ENDPOINTS.get().is_some() {
        builder.no_proxy()
    } else {
        builder
    };
    builder
        .build()
        .map_err(|_| ResponseError::Configuration("HTTP客户端不可用"))
}

pub(crate) async fn request(
    client: &Client,
    authorization: HeaderValue,
    api: &ApiConfig,
    prompt: &Prompt,
    continuation: Option<Continuation>,
) -> Result<ResponseTurn, ResponseError> {
    let endpoint = RESPONSES_URL;
    #[cfg(any(test, feature = "test-support"))]
    let endpoint = TEST_API_ENDPOINTS
        .get()
        .map_or(endpoint, |endpoints| endpoints.responses.as_str());
    request_at(client, authorization, api, prompt, continuation, endpoint).await
}

fn body(
    api: &ApiConfig,
    prompt: &Prompt,
    continuation: Option<Continuation>,
) -> Result<(Vec<u8>, bool), ResponseError> {
    if normalize_base_url(&api.base_url).ok().as_deref() != Some(BASE) {
        return Err(ResponseError::Configuration("订阅连接仅允许官方API地址"));
    }
    if !bounded_text(&api.model, 256) || api.model.trim() != api.model {
        return Err(ResponseError::Configuration("须显式选择账号可用模型"));
    }
    let second = continuation.is_some();
    let instructions = if second {
        format!("{}{BUDGET_USED}", crate::workspace::CONTRACT)
    } else {
        crate::workspace::CONTRACT.to_owned()
    };
    if instructions.len() + prompt.text.len() > CONTEXT_LIMIT {
        return Err(ResponseError::Configuration("应用输入超过24KiB"));
    }
    let mut input = vec![json!({"role":"user","content":prompt.text})];
    if let Some(Continuation { call, result }) = continuation {
        if !search_enabled(prompt)
            || !prompt
                .search_targets
                .iter()
                .any(|target| target.message_id == call.message_id)
            || result.len() > SEARCH_LIMIT
        {
            return Err(ResponseError::Configuration("搜索补全输入无效或超限"));
        }
        input.push(
            json!({"type":"function_call","namespace":"shisui_danmu","name":"web_search",
            "call_id":call.call_id,"arguments":json!({"message_id":call.message_id}).to_string()}),
        );
        input.push(json!({"type":"function_call_output","call_id":call.call_id,"output":result}));
    }
    let mut request = json!({"model":api.model,"instructions":instructions,"input":input,"store":false,"stream":true});
    if !second && search_enabled(prompt) {
        request["tools"] = json!([{"type":"namespace","name":"shisui_danmu","description":SEARCH_DESCRIPTION,
            "tools":[{"type":"function","name":"web_search","strict":true,"parameters":{
                "type":"object","properties":{"message_id":{"type":"string","enum":prompt.search_targets.iter().map(|target| &target.message_id).collect::<Vec<_>>() }},
                "required":["message_id"],"additionalProperties":false}}]}]);
        request["parallel_tool_calls"] = json!(false);
    }
    let bytes =
        serde_json::to_vec(&request).map_err(|_| ResponseError::Configuration("请求编码失败"))?;
    if bytes.len() > REQUEST_LIMIT {
        return Err(ResponseError::Configuration("HTTP请求超过48KiB"));
    }
    Ok((bytes, second))
}
fn search_enabled(prompt: &Prompt) -> bool {
    prompt.web_search && !prompt.search_targets.is_empty()
}

async fn request_at(
    client: &Client,
    mut authorization: HeaderValue,
    api: &ApiConfig,
    prompt: &Prompt,
    continuation: Option<Continuation>,
    endpoint: &str,
) -> Result<ResponseTurn, ResponseError> {
    let (bytes, second) = body(api, prompt, continuation)?;
    authorization.set_sensitive(true);
    let response = client
        .post(endpoint)
        .header(AUTHORIZATION, authorization)
        .header(CONTENT_TYPE, "application/json")
        .header("accept", "text/event-stream")
        .body(bytes)
        .send()
        .await
        .map_err(|_| ResponseError::Transport("请求失败"))?;
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| safe_id(value))
        .map(str::to_owned);
    let event_stream = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        });
    if !status.is_success() {
        let bytes = read_bounded(response, RESPONSE_LIMIT, "HTTP错误响应超过256KiB").await?;
        let code = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| known_code(value.get("error").and_then(|error| error.get("code"))));
        return Err(ResponseError::Http {
            status: status.as_u16(),
            code,
            request_id,
        });
    }
    if !event_stream {
        return Err(ResponseError::Protocol("非SSE响应"));
    }
    parse_stream(response, prompt, second, request_id).await
}
async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
    overflow: &'static str,
) -> Result<Vec<u8>, ResponseError> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ResponseError::Protocol(overflow));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ResponseError::Transport("响应中断"))?
    {
        if chunk.len() > limit - bytes.len() {
            return Err(ResponseError::Protocol(overflow));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn bounded_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}
fn known_code(value: Option<&Value>) -> Option<&'static str> {
    match value.and_then(Value::as_str)? {
        "subscription_sharing_user_not_eligible" => Some("subscription_sharing_user_not_eligible"),
        "subscription_sharing_usage_limit_exceeded" => {
            Some("subscription_sharing_usage_limit_exceeded")
        }
        "subscription_sharing_usage_unavailable" => Some("subscription_sharing_usage_unavailable"),
        "subscription_sharing_unsupported_capability" => {
            Some("subscription_sharing_unsupported_capability")
        }
        "subscription_sharing_route_not_supported" => {
            Some("subscription_sharing_route_not_supported")
        }
        "subscription_sharing_invalid_user" => Some("subscription_sharing_invalid_user"),
        "chatpass_v2_scope_not_authorized" => Some("chatpass_v2_scope_not_authorized"),
        "chatpass_v2_invalid_authorization_context" => {
            Some("chatpass_v2_invalid_authorization_context")
        }
        "subscription_sharing_user_unavailable" => Some("subscription_sharing_user_unavailable"),
        _ => None,
    }
}

async fn parse_stream(
    mut response: reqwest::Response,
    prompt: &Prompt,
    second: bool,
    request_id: Option<String>,
) -> Result<ResponseTurn, ResponseError> {
    if response
        .content_length()
        .is_some_and(|size| size > SSE_STREAM_LIMIT as u64)
    {
        return Err(ResponseError::Protocol("SSE流超过8MiB"));
    }
    let mut parser = SseParser::new(prompt, second, request_id);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ResponseError::Transport("响应中断"))?
    {
        parser.feed(&chunk)?;
    }
    parser.finish()
}

// Only the current line/event and an admitted terminal snapshot are retained.
// A terminal is not returned until clean HTTP EOF; later data still invalidates it.
struct SseParser<'a> {
    prompt: &'a Prompt,
    second: bool,
    request_id: Option<String>,
    line: Vec<u8>,
    data: String,
    event: Option<String>,
    response_id: Option<String>,
    terminal: Option<ResponseTurn>,
    first_line: bool,
    pending_cr: bool,
    event_bytes: usize,
    stream_bytes: usize,
    data_events: usize,
}
impl<'a> SseParser<'a> {
    fn new(prompt: &'a Prompt, second: bool, request_id: Option<String>) -> Self {
        Self {
            prompt,
            second,
            request_id,
            line: Vec::new(),
            data: String::new(),
            event: None,
            response_id: None,
            terminal: None,
            first_line: true,
            pending_cr: false,
            event_bytes: 0,
            stream_bytes: 0,
            data_events: 0,
        }
    }
    fn account_event(&mut self, size: usize) -> Result<(), ResponseError> {
        if size > SSE_EVENT_LIMIT - self.event_bytes {
            return Err(ResponseError::Protocol("SSE事件超过256KiB"));
        }
        self.event_bytes += size;
        Ok(())
    }
    fn feed(&mut self, mut bytes: &[u8]) -> Result<(), ResponseError> {
        if bytes.len() > SSE_STREAM_LIMIT - self.stream_bytes {
            return Err(ResponseError::Protocol("SSE流超过8MiB"));
        }
        self.stream_bytes += bytes.len();
        while !bytes.is_empty() {
            if self.pending_cr {
                self.pending_cr = false;
                if bytes[0] == b'\n' {
                    self.account_event(1)?;
                    bytes = &bytes[1..];
                }
                self.complete_line()?;
                continue;
            }
            let end = bytes
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .unwrap_or(bytes.len());
            self.account_event(end)?;
            self.line.extend_from_slice(&bytes[..end]);
            bytes = &bytes[end..];
            if let Some(&ending) = bytes.first() {
                self.account_event(1)?;
                bytes = &bytes[1..];
                if ending == b'\r' {
                    self.pending_cr = true;
                } else {
                    self.complete_line()?;
                }
            }
        }
        Ok(())
    }
    fn complete_line(&mut self) -> Result<(), ResponseError> {
        let line =
            std::str::from_utf8(&self.line).map_err(|_| ResponseError::Protocol("SSE编码无效"))?;
        let line = if self.first_line {
            line.trim_start_matches('\u{feff}')
        } else {
            line
        };
        self.first_line = false;
        if line.is_empty() {
            self.dispatch()?;
            self.event = None;
            self.event_bytes = 0;
        } else if let Some((field, value)) = line.split_once(':') {
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => {
                    self.data.push_str(value);
                    self.data.push('\n');
                }
                "event" => {
                    if self.event.is_some() {
                        return Err(ResponseError::Protocol("重复事件类型"));
                    }
                    self.event = Some(value.to_owned());
                }
                _ => {} // Comments, id and retry never affect ownership/retries.
            }
        } else if line == "data" {
            self.data.push('\n');
        }
        self.line.clear();
        Ok(())
    }
    fn dispatch(&mut self) -> Result<(), ResponseError> {
        if self.data.is_empty() {
            return Ok(());
        }
        self.data_events += 1;
        if self.data_events > SSE_DATA_EVENT_LIMIT {
            return Err(ResponseError::Protocol("SSE数据事件超过4096个"));
        }
        self.data.pop();
        if self.data == "[DONE]" {
            if self.terminal.is_none() {
                return Err(ResponseError::Protocol("缺少终态"));
            }
        } else {
            if self.terminal.is_some() {
                return Err(ResponseError::Protocol("终态后仍有事件"));
            }
            let value: Value = serde_json::from_str(&self.data)
                .map_err(|_| ResponseError::Protocol("SSE事件JSON无效"))?;
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .ok_or(ResponseError::Protocol("事件类型缺失"))?;
            if self.event.as_deref().is_some_and(|name| name != kind) {
                return Err(ResponseError::Protocol("SSE事件类型不匹配"));
            }
            if value
                .get("sequence_number")
                .is_some_and(|number| number.as_u64().is_none())
            {
                return Err(ResponseError::Protocol("事件序号无效"));
            }
            validate_event_metadata(&value, kind)?;
            check_identity(&value, &mut self.response_id)?;
            match kind {
                "response.completed" | "response.incomplete" | "response.failed" => {
                    self.terminal = Some(terminal_response(
                        &value,
                        kind,
                        self.prompt,
                        self.second,
                        self.request_id.take(),
                    )?);
                }
                "error" => {
                    return Err(ResponseError::Failed {
                        code: known_code(value.get("code")),
                        request_id: self.request_id.take(),
                    });
                }
                "response.created" | "response.in_progress" => {
                    let response = value
                        .get("response")
                        .ok_or(ResponseError::Protocol("缺少响应对象"))?;
                    if response.get("object").and_then(Value::as_str) != Some("response")
                        || response.get("status").and_then(Value::as_str) != Some("in_progress")
                    {
                        return Err(ResponseError::Protocol("非运行中响应"));
                    }
                }
                "response.output_text.delta"
                | "response.refusal.delta"
                | "response.function_call_arguments.delta"
                | "response.reasoning_text.delta"
                | "response.reasoning_summary_text.delta" => {
                    if value.get("delta").and_then(Value::as_str).is_none() {
                        return Err(ResponseError::Protocol("增量类型无效"));
                    }
                }
                "response.output_item.added"
                | "response.output_item.done"
                | "response.content_part.added"
                | "response.content_part.done"
                | "response.output_text.done"
                | "response.refusal.done"
                | "response.function_call_arguments.done"
                | "response.reasoning_text.done"
                | "response.reasoning_summary_text.done"
                | "response.reasoning_summary_part.added"
                | "response.reasoning_summary_part.done" => {}
                _ => return Err(ResponseError::Protocol("不支持的事件类型")),
            }
        }
        self.data.clear();
        Ok(())
    }
    fn finish(mut self) -> Result<ResponseTurn, ResponseError> {
        if self.pending_cr || !self.line.is_empty() {
            self.complete_line()?;
        }
        if !self.data.is_empty() {
            return Err(ResponseError::Protocol("SSE事件未结束"));
        }
        self.terminal.ok_or(ResponseError::Protocol("缺少终态"))
    }
}
fn check_identity(value: &Value, expected: &mut Option<String>) -> Result<(), ResponseError> {
    if value
        .get("response")
        .is_some_and(|response| !response.is_object() || response.get("id").is_none())
    {
        return Err(ResponseError::Protocol("响应对象或ID缺失"));
    }
    for id in [
        value.get("response_id"),
        value
            .get("response")
            .and_then(|response| response.get("id")),
    ]
    .into_iter()
    .flatten()
    {
        let id = id
            .as_str()
            .filter(|id| safe_id(id))
            .ok_or(ResponseError::Protocol("响应ID无效"))?;
        if expected.as_deref().is_some_and(|expected| expected != id) {
            return Err(ResponseError::Protocol("响应归属不匹配"));
        }
        if expected.is_none() {
            *expected = Some(id.to_owned());
        }
    }
    Ok(())
}
fn terminal_response(
    value: &Value,
    kind: &str,
    prompt: &Prompt,
    second: bool,
    request_id: Option<String>,
) -> Result<ResponseTurn, ResponseError> {
    let response = value
        .get("response")
        .filter(|response| response.is_object())
        .ok_or(ResponseError::Protocol("缺少响应对象"))?;
    if response.get("object").and_then(Value::as_str) != Some("response")
        || response
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| !safe_id(id))
    {
        return Err(ResponseError::Protocol("响应对象或ID无效"));
    }
    let status = match kind {
        "response.completed" => "completed",
        "response.incomplete" => "incomplete",
        _ => "failed",
    };
    if response.get("status").and_then(Value::as_str) != Some(status) {
        return Err(ResponseError::Protocol("终态状态不匹配"));
    }
    let returned_model = match response.get("model") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|model| bounded_text(model, 256))
                .ok_or(ResponseError::Protocol("返回模型无效"))?
                .to_owned(),
        ),
    };
    if status == "failed" {
        if response
            .get("error")
            .is_some_and(|error| !error.is_null() && !error.is_object())
        {
            return Err(ResponseError::Protocol("错误对象无效"));
        }
        return Err(ResponseError::Failed {
            code: known_code(response.get("error").and_then(|error| error.get("code"))),
            request_id,
        });
    }
    if status == "incomplete" {
        return Ok(turn(
            Answer::Rejected("模型输出未完成".into()),
            returned_model,
        ));
    }
    if response.get("error").is_some_and(|error| !error.is_null()) {
        return Err(ResponseError::Protocol("完成响应仍有错误"));
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or(ResponseError::Protocol("输出数组缺失"))?;
    let mut text = String::new();
    let mut call = None;
    let mut multiple_calls = false;
    let mut unauthorized_tool = false;
    let mut refused = false;
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                if item
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| !safe_id(id))
                    || item
                        .get("summary")
                        .and_then(Value::as_array)
                        .is_none_or(|summary| {
                            summary.iter().any(|part| {
                                part.get("type").and_then(Value::as_str) != Some("summary_text")
                                    || part.get("text").and_then(Value::as_str).is_none()
                            })
                        })
                    || item
                        .get("encrypted_content")
                        .is_some_and(|value| !value.is_null() && !value.is_string())
                {
                    return Err(ResponseError::Protocol("推理元数据无效"));
                }
            } // Metadata is never candidate text or continuation input.
            Some("message") => {
                if item.get("role").and_then(Value::as_str) != Some("assistant")
                    || item
                        .get("id")
                        .and_then(Value::as_str)
                        .is_none_or(|id| !safe_id(id))
                    || item.get("status").and_then(Value::as_str) != Some("completed")
                {
                    return Err(ResponseError::Protocol("非完整助手消息"));
                }
                for content in item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or(ResponseError::Protocol("消息内容无效"))?
                {
                    match content.get("type").and_then(Value::as_str) {
                        Some("output_text") => text.push_str(
                            content
                                .get("text")
                                .and_then(Value::as_str)
                                .ok_or(ResponseError::Protocol("正文类型无效"))?,
                        ),
                        Some("refusal") => {
                            if content.get("refusal").and_then(Value::as_str).is_none() {
                                return Err(ResponseError::Protocol("拒绝类型无效"));
                            }
                            refused = true;
                        }
                        _ => return Err(ResponseError::Protocol("不支持的内容类型")),
                    }
                }
            }
            Some("function_call") => {
                multiple_calls |= call.replace(item).is_some();
            }
            Some(
                "web_search_call"
                | "mcp_call"
                | "tool_search_call"
                | "custom_tool_call"
                | "computer_call"
                | "file_search_call"
                | "code_interpreter_call"
                | "image_generation_call",
            ) => {
                unauthorized_tool = true;
            }
            _ => return Err(ResponseError::Protocol("不支持的输出类型")),
        }
    }
    if unauthorized_tool {
        return Ok(turn(
            Answer::Rejected("模型请求了未授权工具".into()),
            returned_model,
        ));
    }
    if refused {
        return Ok(turn(
            Answer::Rejected("模型拒绝回答".into()),
            returned_model,
        ));
    }
    if let Some(call) = call {
        if second || !search_enabled(prompt) || multiple_calls {
            return Ok(turn(
                Answer::Rejected("搜索关闭、预算耗尽或调用数量非法".into()),
                returned_model,
            ));
        }
        let rejected = || {
            turn(
                Answer::Rejected("搜索调用越权或参数非法".into()),
                returned_model.clone(),
            )
        };
        if call.get("namespace").and_then(Value::as_str) != Some("shisui_danmu")
            || call.get("name").and_then(Value::as_str) != Some("web_search")
            || call
                .get("status")
                .is_some_and(|status| status.as_str() != Some("completed"))
            || call
                .get("async")
                .is_some_and(|value| value.as_bool() != Some(false))
            || call.get("caller").is_some_and(|value| {
                !value.is_null() && value.get("type").and_then(Value::as_str) != Some("direct")
            })
        {
            return Ok(rejected());
        }
        let Some(call_id) = call
            .get("call_id")
            .and_then(Value::as_str)
            .filter(|id| bounded_text(id, 128))
        else {
            return Ok(rejected());
        };
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            message_id: String,
        }
        let Some(arguments) = call
            .get("arguments")
            .and_then(Value::as_str)
            .and_then(|arguments| serde_json::from_str::<Arguments>(arguments).ok())
        else {
            return Ok(rejected());
        };
        if !prompt
            .search_targets
            .iter()
            .any(|target| target.message_id == arguments.message_id)
        {
            return Ok(rejected());
        }
        return Ok(ResponseTurn {
            result: ResponseResult::ToolCall(ToolCall {
                call_id: call_id.to_owned(),
                message_id: arguments.message_id,
            }),
            returned_model,
        });
    }
    let answer = if text.trim().is_empty() {
        Answer::NoMessage
    } else {
        match model::candidates(&text, &prompt.round_id, &prompt.message_ids) {
            Ok(candidates) => Answer::Candidates(candidates),
            Err(error) => Answer::Rejected(error.to_string()),
        }
    };
    Ok(turn(answer, returned_model))
}
fn turn(answer: Answer, returned_model: Option<String>) -> ResponseTurn {
    ResponseTurn {
        result: ResponseResult::Answer(answer),
        returned_model,
    }
}

pub(crate) async fn list_models(
    client: &Client,
    authorization: HeaderValue,
) -> Result<Vec<ModelChoice>, ResponseError> {
    let endpoint = MODELS_URL;
    #[cfg(any(test, feature = "test-support"))]
    let endpoint = TEST_API_ENDPOINTS
        .get()
        .map_or(endpoint, |endpoints| endpoints.models.as_str());
    list_models_at(client, authorization, endpoint).await
}
async fn list_models_at(
    client: &Client,
    mut authorization: HeaderValue,
    endpoint: &str,
) -> Result<Vec<ModelChoice>, ResponseError> {
    authorization.set_sensitive(true);
    let response = client
        .get(endpoint)
        .header(AUTHORIZATION, authorization)
        .send()
        .await
        .map_err(|_| ResponseError::Transport("模型目录请求失败"))?;
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|id| safe_id(id))
        .map(str::to_owned);
    let (limit, overflow) = if status.is_success() {
        (MODEL_CATALOG_LIMIT, "模型目录响应超过8MiB")
    } else {
        (RESPONSE_LIMIT, "响应超过256KiB")
    };
    let bytes = read_bounded(response, limit, overflow).await?;
    if !status.is_success() {
        let code = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| known_code(value.get("error").and_then(|error| error.get("code"))));
        return Err(ResponseError::Http {
            status: status.as_u16(),
            code,
            request_id,
        });
    }
    catalog_choices(&bytes)
}

// Parse directory/array/items once. Only the three UI fields borrow raw JSON;
// unused metadata is strictly consumed without allocating a Value tree.
#[derive(Deserialize)]
#[serde(field_identifier)]
enum CatalogKey {
    #[serde(rename = "models")]
    Models,
    #[serde(rename = "visibility")]
    Visibility,
    #[serde(rename = "slug")]
    Slug,
    #[serde(rename = "display_name")]
    DisplayName,
    #[serde(other)]
    Other,
}
fn catalog_error<E: de::Error>(error: &mut Option<ResponseError>, message: &'static str) -> E {
    *error = Some(ResponseError::Protocol(message));
    E::custom("invalid model directory")
}
struct CatalogParser<'a>(&'a mut Option<ResponseError>);
impl<'de> DeserializeSeed<'de> for CatalogParser<'_> {
    type Value = Vec<ModelChoice>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for CatalogParser<'_> {
    type Value = Vec<ModelChoice>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a model directory object")
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut models = None;
        while let Some(key) = map.next_key::<CatalogKey>()? {
            if matches!(key, CatalogKey::Models) {
                models = Some(map.next_value_seed(CatalogItems(self.0))?);
            } else {
                map.next_value::<CatalogJson>()?;
            }
        }
        models.ok_or_else(|| catalog_error(self.0, "模型目录数组缺失"))
    }
}
struct CatalogItems<'a>(&'a mut Option<ResponseError>);
impl<'de> DeserializeSeed<'de> for CatalogItems<'_> {
    type Value = Vec<ModelChoice>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for CatalogItems<'_> {
    type Value = Vec<ModelChoice>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a bounded model array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut choices = Vec::new();
        let mut count = 0;
        while let Some(model) = sequence.next_element_seed(CatalogModel {
            error: self.0,
            count: &mut count,
        })? {
            if let Some(choice) = model {
                if choices.len() == 256
                    || choices
                        .iter()
                        .any(|other: &ModelChoice| other.slug == choice.slug)
                {
                    return Err(catalog_error(self.0, "模型目录重复或超限"));
                }
                choices.push(choice);
            }
        }
        Ok(choices)
    }
}
struct CatalogModel<'a> {
    error: &'a mut Option<ResponseError>,
    count: &'a mut usize,
}
impl<'de> DeserializeSeed<'de> for CatalogModel<'_> {
    type Value = Option<ModelChoice>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        // next_element_seed invokes this only when another array item exists.
        // Reject the 1025th item before reading its fields or metadata.
        if *self.count == 1024 {
            return Err(catalog_error(self.error, "模型目录数量超限"));
        }
        *self.count += 1;
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for CatalogModel<'_> {
    type Value = Option<ModelChoice>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a model object")
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut visibility = None;
        let mut slug = None;
        let mut display_name = None;
        while let Some(key) = map.next_key::<CatalogKey>()? {
            let field = match key {
                CatalogKey::Visibility => &mut visibility,
                CatalogKey::Slug => &mut slug,
                CatalogKey::DisplayName => &mut display_name,
                _ => {
                    map.next_value::<CatalogJson>()?;
                    continue;
                }
            };
            let raw: &'de RawValue = map.next_value()?;
            // RawValue skips decoding. Check these fields too, even when hidden
            // and their types are intentionally not admitted as UI text.
            serde_json::from_str::<CatalogJson>(raw.get())
                .map_err(|_| de::Error::custom("invalid model field JSON"))?;
            *field = Some(raw);
        }
        let listed = catalog_field(visibility, CatalogVisibility)
            .ok_or_else(|| catalog_error(self.error, "模型可见性无效"))?;
        if !listed {
            return Ok(None);
        }
        let slug = catalog_field(slug, CatalogText)
            .ok_or_else(|| catalog_error(self.error, "模型标识无效"))?;
        let display_name = catalog_field(display_name, CatalogText)
            .ok_or_else(|| catalog_error(self.error, "模型名称无效"))?;
        Ok(Some(ModelChoice { slug, display_name }))
    }
}
// Visibility needs only a comparison; visible text is bounded before owning it.
struct CatalogVisibility;
impl<'de> Visitor<'de> for CatalogVisibility {
    type Value = bool;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("model visibility")
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<bool, E> {
        Ok(value == "list")
    }
}
struct CatalogText;
impl<'de> Visitor<'de> for CatalogText {
    type Value = String;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("bounded model text")
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        if bounded_text(value, 256) {
            Ok(value.to_owned())
        } else {
            Err(E::custom("invalid model text"))
        }
    }
}
fn catalog_field<'de, V: Visitor<'de>>(
    value: Option<&'de RawValue>,
    visitor: V,
) -> Option<V::Value> {
    let mut deserializer = serde_json::Deserializer::from_str(value?.get());
    de::Deserializer::deserialize_str(&mut deserializer, visitor).ok()
}
// Strict discarded JSON: unlike IgnoredAny/RawValue, decode strings and numbers
// so malformed UTF-8, lone surrogates and excessive nesting stay errors.
struct CatalogJson;
impl<'de> Deserialize<'de> for CatalogJson {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Self)
    }
}
impl<'de> Visitor<'de> for CatalogJson {
    type Value = Self;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("valid JSON")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self, E> {
        Ok(self)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self, E> {
        Ok(self)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self, E> {
        Ok(self)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self, E> {
        Ok(self)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self, E> {
        Ok(self)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self, E> {
        Ok(self)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self, A::Error> {
        while sequence.next_element::<Self>()?.is_some() {}
        Ok(self)
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self, A::Error> {
        while map.next_entry::<Self, Self>()?.is_some() {}
        Ok(self)
    }
}

fn catalog_choices(bytes: &[u8]) -> Result<Vec<ModelChoice>, ResponseError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let mut error = None;
    let models = CatalogParser(&mut error)
        .deserialize(&mut deserializer)
        .map_err(|_| error.unwrap_or(ResponseError::Protocol("模型目录JSON无效")))?;
    deserializer
        .end()
        .map_err(|_| ResponseError::Protocol("模型目录JSON无效"))?;
    Ok(models)
}

fn validate_event_metadata(value: &Value, kind: &str) -> Result<(), ResponseError> {
    for field in ["output_index", "content_index", "summary_index"] {
        if value
            .get(field)
            .is_some_and(|index| index.as_u64().is_none())
        {
            return Err(ResponseError::Protocol("事件索引无效"));
        }
    }
    if value
        .get("item_id")
        .is_some_and(|id| id.as_str().is_none_or(|id| !safe_id(id)))
    {
        return Err(ResponseError::Protocol("事件项目ID无效"));
    }
    let field = match kind {
        "response.output_item.added" | "response.output_item.done" => Some("item"),
        "response.content_part.added"
        | "response.content_part.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done" => Some("part"),
        _ => None,
    };
    if field.is_some_and(|field| {
        value
            .get(field)
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .is_none()
    }) {
        return Err(ResponseError::Protocol("事件项目或内容类型无效"));
    }
    let field = match kind {
        "response.output_text.done"
        | "response.reasoning_text.done"
        | "response.reasoning_summary_text.done" => Some("text"),
        "response.refusal.done" => Some("refusal"),
        "response.function_call_arguments.done" => Some("arguments"),
        _ => None,
    };
    if field.is_some_and(|field| value.get(field).and_then(Value::as_str).is_none()) {
        return Err(ResponseError::Protocol("完成事件内容类型无效"));
    }
    Ok(())
}

// A process-local test seam for the isolated real-TUI child, never an environment or config override.
#[cfg(any(test, feature = "test-support"))]
struct TestApiEndpoints {
    responses: String,
    models: String,
}
#[cfg(any(test, feature = "test-support"))]
static TEST_API_ENDPOINTS: std::sync::OnceLock<TestApiEndpoints> = std::sync::OnceLock::new();
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn install_test_api_origin(origin: &str) -> Result<(), ResponseError> {
    let parsed =
        url::Url::parse(origin).map_err(|_| ResponseError::Configuration("测试API origin无效"))?;
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if !loopback
        || parsed.scheme() != "http"
        || parsed.path() != "/"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ResponseError::Configuration(
            "测试API须为HTTP IP-loopback origin",
        ));
    }
    let origin = parsed.origin().ascii_serialization();
    TEST_API_ENDPOINTS
        .set(TestApiEndpoints {
            responses: format!("{origin}/v1/responses"),
            models: format!("{origin}/v1/models"),
        })
        .map_err(|_| ResponseError::Configuration("测试API origin已安装"))
}

#[cfg(test)]
pub(crate) async fn request_with_endpoint(
    client: &Client,
    authorization: HeaderValue,
    api: &ApiConfig,
    prompt: &Prompt,
    continuation: Option<Continuation>,
    endpoint: &str,
) -> Result<ResponseTurn, ResponseError> {
    check_test_endpoint(endpoint, "/v1/responses")?;
    request_at(client, authorization, api, prompt, continuation, endpoint).await
}
#[cfg(test)]
pub(crate) async fn list_models_with_endpoint(
    client: &Client,
    authorization: HeaderValue,
    endpoint: &str,
) -> Result<Vec<ModelChoice>, ResponseError> {
    check_test_endpoint(endpoint, "/v1/models")?;
    list_models_at(client, authorization, endpoint).await
}
#[cfg(test)]
fn check_test_endpoint(endpoint: &str, path: &str) -> Result<(), ResponseError> {
    let url =
        url::Url::parse(endpoint).map_err(|_| ResponseError::Configuration("测试端点无效"))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if !loopback
        || url.scheme() != "http"
        || url.path() != path
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ResponseError::Configuration("测试端点必须是本地对应路由"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "responses_tests.rs"]
mod tests;
