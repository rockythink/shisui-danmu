//! In-memory HTTP/1.1 fixture shared by model, search, and Runner tests.
//! Each accepted connection consumes one scenario and closes after its response.
//! Requests (including fake authentication headers) never leave this fixture's memory.

use std::{sync::Arc, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};

const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(3);

/// One complete request. Header names are lowercase; duplicate headers are retained.
/// Deliberately not Debug: an assertion must not accidentally print fake credentials.
#[derive(Clone)]
pub(crate) struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("fixture request must contain JSON")
    }
}

/// Response framing is explicit so tests can distinguish complete, chunked,
/// prematurely disconnected, and absent HTTP responses.
pub(crate) struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: ResponseBody,
}

enum ResponseBody {
    Complete(Vec<u8>),
    Chunked(Vec<Vec<u8>>),
    Partial {
        bytes: Vec<u8>,
        declared_length: usize,
    },
    Disconnect,
}

impl Response {
    pub fn json(body: Value) -> Self {
        Self::bytes(serde_json::to_vec(&body).unwrap()).header("content-type", "application/json")
    }

    pub fn bytes(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: ResponseBody::Complete(body),
        }
    }

    /// Chunk boundaries are HTTP framing boundaries, not promises about TCP reads.
    /// Empty chunks are skipped; exactly one terminating zero chunk is emitted.
    pub fn chunked(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            body: ResponseBody::Chunked(chunks),
            ..Self::bytes(Vec::new())
        }
    }

    /// Send fewer bytes than advertised, then close the socket.
    pub fn partial(bytes: Vec<u8>, declared_length: usize) -> Self {
        assert!(bytes.len() < declared_length);
        Self {
            body: ResponseBody::Partial {
                bytes,
                declared_length,
            },
            ..Self::bytes(Vec::new())
        }
    }

    /// Close immediately after receiving the request, without HTTP headers.
    pub fn disconnect() -> Self {
        Self {
            body: ResponseBody::Disconnect,
            ..Self::bytes(Vec::new())
        }
    }

    pub fn status(mut self, status: u16) -> Self {
        assert!((100..=599).contains(&status));
        self.status = status;
        self
    }

    /// Extra response headers, e.g. content-type or mcp-session-id.
    /// Framing headers belong to the constructors, not this method.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        assert!(!name.is_empty() && name.bytes().all(header_token));
        assert!(
            value
                .bytes()
                .all(|byte| byte == b'\t' || (32..=126).contains(&byte))
        );
        assert!(
            !["content-length", "transfer-encoding", "connection"]
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
        );
        self.headers.push((name.into(), value.into()));
        self
    }
}

type Responder = Box<dyn FnOnce(&RecordedRequest) -> Response + Send>;

/// A FIFO response, optionally computed from the actual round/target in the request.
pub(crate) struct Scenario {
    respond: Responder,
    received: Option<oneshot::Sender<RecordedRequest>>,
    release: Option<oneshot::Receiver<()>>,
}

impl Scenario {
    pub fn new(response: Response) -> Self {
        Self::respond(move |_| response)
    }

    pub fn respond(respond: impl FnOnce(&RecordedRequest) -> Response + Send + 'static) -> Self {
        Self {
            respond: Box::new(respond),
            received: None,
            release: None,
        }
    }

    /// Await `received` before cancelling a worker or changing scene/context.
    /// No response bytes are written until `release.send(())`. Dropping release
    /// closes the connection without a response. Shutdown cancels either wait.
    pub fn barrier(
        mut self,
    ) -> (
        Self,
        oneshot::Receiver<RecordedRequest>,
        oneshot::Sender<()>,
    ) {
        assert!(self.received.is_none() && self.release.is_none());
        let (received_tx, received_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        self.received = Some(received_tx);
        self.release = Some(release_rx);
        (self, received_rx, release_tx)
    }
}

struct State {
    requests: watch::Sender<Vec<RecordedRequest>>,
    errors: watch::Sender<Vec<&'static str>>,
}

/// Owns the listener and all connection tasks. Drop aborts the owner; its JoinSet
/// aborts every child. `shutdown` additionally waits for child cancellation.
pub(crate) struct HttpServer {
    origin: String,
    state: Arc<State>,
    scenarios: mpsc::UnboundedSender<Scenario>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl HttpServer {
    pub async fn spawn(scenarios: Vec<Scenario>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (requests, _) = watch::channel(Vec::new());
        let (errors, _) = watch::channel(Vec::new());
        let (scenario_tx, mut scenario_rx) = mpsc::unbounded_channel();
        for scenario in scenarios {
            assert!(
                scenario_tx.send(scenario).is_ok(),
                "fixture scenario queue closed"
            );
        }
        let state = Arc::new(State { requests, errors });
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let shared = state.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop_rx => break,
                    completed = connections.join_next(), if !connections.is_empty() => {
                        if completed.is_some_and(|result| result.is_err()) {
                            shared.errors.send_modify(|errors| errors.push("fixture connection task panicked"));
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else {
                            shared.errors.send_modify(|errors| errors.push("fixture accept failed"));
                            break;
                        };
                        let scenario = scenario_rx.try_recv().ok();
                        let shared = shared.clone();
                        connections.spawn(serve(socket, scenario, shared));
                    }
                }
            }
            connections.abort_all();
            while let Some(result) = connections.join_next().await {
                if result.is_err_and(|error| !error.is_cancelled()) {
                    shared
                        .errors
                        .send_modify(|errors| errors.push("fixture connection task panicked"));
                }
            }
        });
        Self {
            origin,
            state,
            scenarios: scenario_tx,
            stop: Some(stop_tx),
            task: Some(task),
        }
    }

    /// Model service root; preserves the /v1 prefix when production appends its route.
    pub fn base_url(&self) -> String {
        self.url("/v1")
    }

    /// Arbitrary fixture route, including MCP query strings.
    pub fn url(&self, path: &str) -> String {
        assert!(path.starts_with('/'));
        format!("{}{path}", self.origin)
    }

    /// Arrange the next response before injecting a new application event.
    pub fn push(&self, scenario: Scenario) {
        assert!(
            self.scenarios.send(scenario).is_ok(),
            "fixture scenario queue closed"
        );
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.borrow().clone()
    }

    /// Fixture faults contain only fixed categories, never request data.
    pub fn errors(&self) -> Vec<&'static str> {
        self.state.errors.borrow().clone()
    }

    /// Bounded observation barrier; unlike sleeping, this proves receipt of the body.
    pub async fn wait_requests(&self, count: usize) {
        let mut observed = self.state.requests.subscribe();
        tokio::time::timeout(RECEIVE_TIMEOUT, async {
            loop {
                if observed.borrow_and_update().len() >= count {
                    break;
                }
                observed.changed().await.unwrap();
            }
        })
        .await
        .expect("fixture did not receive the expected request count");
    }

    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.expect("fixture owner task panicked");
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn serve(mut socket: TcpStream, scenario: Option<Scenario>, state: Arc<State>) {
    let request = match tokio::time::timeout(RECEIVE_TIMEOUT, read_request(&mut socket)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            state.errors.send_modify(|errors| errors.push(error));
            return;
        }
        Err(_) => {
            state
                .errors
                .send_modify(|errors| errors.push("fixture request timed out"));
            return;
        }
    };
    state
        .requests
        .send_modify(|requests| requests.push(request.clone()));
    let Some(scenario) = scenario else {
        state
            .errors
            .send_modify(|errors| errors.push("fixture received an unplanned request"));
        return;
    };
    if let Some(received) = scenario.received {
        let _ = received.send(request.clone());
    }
    if let Some(release) = scenario.release
        && release.await.is_err()
    {
        return;
    }
    let response = (scenario.respond)(&request);
    // A cancelled client may close at any point during writes. That is an expected
    // fixture use case, not a server fault; reads and unexpected requests remain visible.
    let _ = write_response(&mut socket, response).await;
}

async fn read_request(socket: &mut TcpStream) -> Result<RecordedRequest, &'static str> {
    let mut request = Vec::new();
    let end = loop {
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            if end + 4 > MAX_HEADERS {
                return Err("fixture request headers too large");
            }
            break end;
        }
        if request.len() >= MAX_HEADERS {
            return Err("fixture request headers too large");
        }
        let mut bytes = [0; 1024];
        let available = bytes.len().min(MAX_HEADERS - request.len());
        let count = socket
            .read(&mut bytes[..available])
            .await
            .map_err(|_| "fixture request read failed")?;
        if count == 0 {
            return Err("fixture request disconnected before headers");
        }
        request.extend_from_slice(&bytes[..count]);
    };
    let header = std::str::from_utf8(&request[..end])
        .map_err(|_| "fixture request headers are not UTF-8")?;
    let mut lines = header.split("\r\n");
    let mut first = lines
        .next()
        .ok_or("fixture request line missing")?
        .split(' ');
    let method = first
        .next()
        .filter(|value| !value.is_empty() && value.bytes().all(header_token))
        .ok_or("fixture request method invalid")?
        .to_owned();
    let path = first
        .next()
        .filter(|value| value.starts_with('/') && !value.chars().any(char::is_control))
        .ok_or("fixture request path invalid")?
        .to_owned();
    if !matches!(first.next(), Some("HTTP/1.1" | "HTTP/1.0")) || first.next().is_some() {
        return Err("fixture request version invalid");
    }
    let mut headers = Vec::new();
    let mut length = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or("fixture request header invalid")?;
        if name.is_empty() || !name.bytes().all(header_token) {
            return Err("fixture request header name invalid");
        }
        let value = value.trim();
        if !value
            .bytes()
            .all(|byte| byte == b'\t' || (32..=126).contains(&byte))
        {
            return Err("fixture request header value invalid");
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err("fixture request content length invalid");
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "fixture request content length invalid")?,
            );
        }
        // reqwest's JSON requests have a known length. Refuse ambiguous framing
        // rather than teaching this small fixture a second HTTP request protocol.
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("fixture request transfer encoding unsupported");
        }
        headers.push((name.to_ascii_lowercase(), value.to_owned()));
    }
    let length = length.unwrap_or(0);
    if length > MAX_BODY {
        return Err("fixture request body too large");
    }
    let body_start = end + 4;
    let complete = body_start + length;
    if request.len() > complete {
        return Err("fixture request has trailing bytes");
    }
    while request.len() < complete {
        let mut bytes = [0; 1024];
        let available = bytes.len().min(complete - request.len());
        let count = socket
            .read(&mut bytes[..available])
            .await
            .map_err(|_| "fixture request read failed")?;
        if count == 0 {
            return Err("fixture request disconnected before body");
        }
        request.extend_from_slice(&bytes[..count]);
    }
    Ok(RecordedRequest {
        method,
        path,
        headers,
        body: request.split_off(body_start),
    })
}

fn header_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

async fn write_response(socket: &mut TcpStream, response: Response) -> std::io::Result<()> {
    if matches!(response.body, ResponseBody::Disconnect) {
        return Ok(());
    }
    let mut header = format!(
        "HTTP/1.1 {} Fixture\r\nConnection: close\r\n",
        response.status
    );
    for (name, value) in response.headers {
        header.push_str(&format!("{name}: {value}\r\n"));
    }
    match &response.body {
        ResponseBody::Complete(bytes) => {
            header.push_str(&format!("Content-Length: {}\r\n", bytes.len()))
        }
        ResponseBody::Partial {
            declared_length, ..
        } => header.push_str(&format!("Content-Length: {declared_length}\r\n")),
        ResponseBody::Chunked(_) => header.push_str("Transfer-Encoding: chunked\r\n"),
        ResponseBody::Disconnect => unreachable!(),
    }
    header.push_str("\r\n");
    socket.write_all(header.as_bytes()).await?;
    match response.body {
        ResponseBody::Complete(bytes) | ResponseBody::Partial { bytes, .. } => {
            socket.write_all(&bytes).await?
        }
        ResponseBody::Chunked(chunks) => {
            for bytes in chunks.into_iter().filter(|bytes| !bytes.is_empty()) {
                socket
                    .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                    .await?;
                socket.write_all(&bytes).await?;
                socket.write_all(b"\r\n").await?;
                socket.flush().await?;
            }
            socket.write_all(b"0\r\n\r\n").await?;
        }
        ResponseBody::Disconnect => unreachable!(),
    }
    socket.shutdown().await
}

/// Formal Chat Completions envelope; tests can mutate its fields for rejection cases.
pub(crate) fn openai(content: Option<&str>, finish_reason: &str) -> Value {
    json!({
        "id": "fixture-completion", "object": "chat.completion", "created": 0,
        "model": "fixture-openai",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content},
            "finish_reason": finish_reason}]
    })
}

/// `content` is the Anthropic block array (text or tool_use), not a serialized string.
pub(crate) fn anthropic(content: Value, stop_reason: &str) -> Value {
    json!({
        "id": "fixture-message", "type": "message", "role": "assistant",
        "model": "fixture-anthropic", "content": content, "stop_reason": stop_reason,
        "stop_sequence": null, "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

/// Formal JSON-RPC response usable for MCP initialize and tools/call.
pub(crate) fn mcp(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
