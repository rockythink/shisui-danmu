//! One bounded Exa MCP exchange. No task or cleanup is spawned when its future is dropped.
use std::time::{Duration, Instant};

use reqwest::{
    Method,
    header::{CONTENT_TYPE, HeaderValue},
};
use serde_json::{Value, json};

const ENDPOINT: &str = "https://mcp.exa.ai/mcp?tools=web_search_exa";
const VERSION: &str = "2025-03-26";
const RESPONSE_LIMIT: usize = 96 * 1024;
const TEXT_LIMIT: usize = 4000;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) struct Client {
    http: reqwest::Client,
    endpoint: url::Url,
    timeout: Duration,
}

impl Client {
    /// Only builds local configuration; the production endpoint is not configurable.
    pub(crate) fn new() -> anyhow::Result<Self> {
        Self::build(url::Url::parse(ENDPOINT)?, SEARCH_TIMEOUT, false)
    }

    fn build(endpoint: url::Url, timeout: Duration, loopback: bool) -> anyhow::Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert("mcp-protocol-version", HeaderValue::from_static(VERSION));
        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10));
        if loopback {
            builder = builder.no_proxy();
        }
        let http = builder
            .build()
            .map_err(|_| anyhow::anyhow!("搜索客户端无法初始化"))?;
        Ok(Self {
            http,
            endpoint,
            timeout,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_endpoint(endpoint: &str, timeout: Duration) -> anyhow::Result<Self> {
        let endpoint = url::Url::parse(endpoint)?;
        anyhow::ensure!(
            endpoint.scheme() == "http"
                && match endpoint.host() {
                    Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                    Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                    _ => false,
                },
            "搜索测试地址必须是loopback"
        );
        Self::build(endpoint, timeout, true)
    }

    /// `query` is the application's frozen public question, never a model-supplied query.
    /// Any transport/protocol/search failure is Unavailable; no raw diagnostics escape.
    pub(crate) async fn search(&self, query: &str, deadline: Instant) -> Option<String> {
        let end = deadline.min(Instant::now() + self.timeout);
        if end <= Instant::now()
            || query.is_empty()
            || query.trim() != query
            || query.chars().any(char::is_control)
            || query.chars().count() > 240
        {
            return None;
        }
        let mut session = None;
        let result = tokio::time::timeout_at(end.into(), self.exchange(query, &mut session))
            .await
            .ok()?;
        // This is ordinary future-owned work: cancellation drops it without DELETE.
        if let Some(session) = session.as_ref()
            && Instant::now() < end
        {
            let cleanup_end = end.min(Instant::now() + CLEANUP_TIMEOUT);
            let _ = tokio::time::timeout_at(cleanup_end.into(), async {
                let _ = self.request(Method::DELETE, Some(session)).send().await;
            })
            .await;
        }
        result
    }

    fn request(&self, method: Method, session: Option<&HeaderValue>) -> reqwest::RequestBuilder {
        let request = self.http.request(method, self.endpoint.clone());
        match session {
            Some(session) => request.header("mcp-session-id", session),
            None => request,
        }
    }

    async fn exchange(&self, query: &str, session: &mut Option<HeaderValue>) -> Option<String> {
        let initialized = self
            .rpc(
                json!({
                    "jsonrpc":"2.0", "id":1, "method":"initialize",
                    "params": {"protocolVersion":VERSION, "capabilities":{},
                        "clientInfo":{"name":"shisui-danmu-search", "version":"1"}}
                }),
                Some(1),
                session,
                true,
            )
            .await?;
        if initialized.get("protocolVersion")?.as_str()? != VERSION {
            return None;
        }
        self.rpc(
            json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
            None,
            session,
            false,
        )
        .await?;
        let result = self
            .rpc(
                json!({
                    "jsonrpc":"2.0", "id":2, "method":"tools/call",
                    "params":{"name":"web_search_exa", "arguments":{"query":query, "numResults":3}}
                }),
                Some(2),
                session,
                false,
            )
            .await?;
        match result.get("isError") {
            None | Some(Value::Bool(false)) => {}
            _ => return None,
        }
        let content = result.get("content")?.as_array()?;
        let mut text = String::new();
        let mut remaining = TEXT_LIMIT;
        for part in content {
            // Images, resources, and arbitrary fields are not forwarded as evidence.
            if part.get("type").and_then(Value::as_str) != Some("text") {
                continue;
            }
            let part = part.get("text")?.as_str()?.trim();
            if part.is_empty() {
                continue;
            }
            if !text.is_empty() && remaining > 0 {
                text.push('\n');
                remaining -= 1;
            }
            for scalar in part.chars().take(remaining) {
                text.push(scalar);
                remaining -= 1;
            }
            if remaining == 0 {
                break;
            }
        }
        text.truncate(text.trim_end().len());
        (!text.is_empty()).then_some(text)
    }

    async fn rpc(
        &self,
        message: Value,
        id: Option<u64>,
        session: &mut Option<HeaderValue>,
        initialize: bool,
    ) -> Option<Value> {
        let mut response = self
            .request(Method::POST, session.as_ref())
            .json(&message)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut tokens = response.headers().get_all("mcp-session-id").iter();
        if let Some(token) = tokens.next() {
            if tokens.next().is_some() || !valid_session(token.as_bytes()) {
                return None;
            }
            if initialize {
                *session = Some(token.clone());
            } else if session.as_ref() != Some(token) {
                return None;
            }
        }
        let mime = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .unwrap_or("");
        let json = mime.eq_ignore_ascii_case("application/json");
        let sse = mime.eq_ignore_ascii_case("text/event-stream");
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if chunk.len() > RESPONSE_LIMIT - body.len() {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        let Some(id) = id else {
            // MCP notifications have no JSON-RPC reply (typically HTTP 202).
            return Some(Value::Null);
        };
        if json {
            rpc_result(serde_json::from_slice(&body).ok()?, id)
        } else if sse {
            sse_result(std::str::from_utf8(&body).ok()?, id)
        } else {
            None
        }
    }
}

fn valid_session(token: &[u8]) -> bool {
    (1..=128).contains(&token.len())
        && token
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn rpc_result(value: Value, id: u64) -> Option<Value> {
    let Value::Object(mut value) = value else {
        return None;
    };
    if value.get("jsonrpc")?.as_str()? != "2.0"
        || value.get("id")?.as_u64()? != id
        || value.contains_key("error")
        || value.contains_key("method")
    {
        return None;
    }
    let result = value.remove("result")?;
    result.is_object().then_some(result)
}

fn sse_result(text: &str, id: u64) -> Option<Value> {
    let mut result = None;
    let mut data = String::new();
    // Read the complete bounded body first; HTTP/TCP chunks are never SSE events.
    for line in text.split_terminator('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !data.is_empty() {
                let value: Value = serde_json::from_str(&data).ok()?;
                if value.get("jsonrpc")?.as_str()? != "2.0" {
                    return None;
                }
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if result.is_some() {
                        return None;
                    }
                    result = Some(rpc_result(value, id)?);
                }
                data.clear();
            }
        } else if let Some(line) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(line.strip_prefix(' ').unwrap_or(line));
        }
    }
    // An unterminated event is not a verified response.
    if !data.is_empty() {
        return None;
    }
    result
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
