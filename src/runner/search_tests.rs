use super::*;
use crate::runner::test_support::{HttpServer, Response, Scenario};
const QUERY: &str = "如何理解增量游标？";
const SOURCE: &str = "来源 https://example.org/cursor 增量游标资料";

fn initialize() -> Response {
    Response::json(json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":VERSION}}))
}
fn initialized() -> Response {
    Response::bytes(Vec::new()).status(202)
}
fn result(text: &str) -> Value {
    json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":text}]}})
}
fn client(server: &HttpServer, timeout: Duration) -> Client {
    Client::with_endpoint(&server.url("/mcp?tools=web_search_exa"), timeout).unwrap()
}
async fn run(server: &HttpServer) -> Option<String> {
    client(server, Duration::from_secs(2))
        .search(QUERY, Instant::now() + Duration::from_secs(3))
        .await
}
async fn tool_case(response: Response) -> HttpServer {
    HttpServer::spawn(vec![
        Scenario::new(initialize()),
        Scenario::new(initialized()),
        Scenario::new(response),
    ])
    .await
}

#[tokio::test]
async fn json_handshake_frozen_query_session_and_delete() {
    let server = HttpServer::spawn(vec![
        Scenario::new(initialize().header("mcp-session-id", "Session_1-valid")),
        Scenario::new(initialized()),
        Scenario::new(Response::json(result(SOURCE))),
        Scenario::new(Response::bytes(Vec::new()).status(204)),
    ])
    .await;
    assert_eq!(run(&server).await.as_deref(), Some(SOURCE));
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[0].json(),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"shisui-danmu-search","version":"1"}}})
    );
    assert_eq!(
        requests[1].json(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    );
    assert_eq!(
        requests[2].json(),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"web_search_exa","arguments":{"query":QUERY,"numResults":3}}})
    );
    assert_eq!(requests[3].method, "DELETE");
    assert!(requests[3].body.is_empty());
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request.path, "/mcp?tools=web_search_exa");
        assert_eq!(request.header("mcp-protocol-version"), Some(VERSION));
        assert_eq!(
            request.header("mcp-session-id"),
            if index == 0 {
                None
            } else {
                Some("Session_1-valid")
            }
        );
        for secret_header in ["authorization", "cookie", "x-api-key"] {
            assert!(request.header(secret_header).is_none());
        }
    }
    assert!(server.errors().is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn split_json_and_multiple_text_blocks_keep_only_evidence() {
    let response = json!({"jsonrpc":"2.0","id":2,"result":{"content":[
        {"type":"text","text":"  来源 https://example.org/one  "},
        {"type":"resource","resource":{"uri":"https://example.org/not-fetched"}},
        {"type":"text","text":"第二段"}]}});
    let chunks = serde_json::to_vec(&response)
        .unwrap()
        .chunks(1)
        .map(<[u8]>::to_vec)
        .collect();
    let server =
        tool_case(Response::chunked(chunks).header("content-type", "application/json")).await;
    assert_eq!(
        run(&server).await.as_deref(),
        Some("来源 https://example.org/one\n第二段")
    );
    assert_eq!(server.requests().len(), 3);
    server.shutdown().await;
}

#[tokio::test]
async fn sse_multiline_crlf_split_unicode_and_notifications() {
    let init = "data: {\r\ndata: \"jsonrpc\":\"2.0\",\"id\":1,\r\ndata: \"result\":{\"protocolVersion\":\"2025-03-26\"}}\r\n\r\n";
    let tool = format!(
        "data: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\n\ndata: {{\ndata: \"jsonrpc\":\"2.0\",\"id\":2,\ndata: \"result\":{{\"content\":[{{\"type\":\"text\",\"text\":{}}}]}}}}\n\n",
        serde_json::to_string(SOURCE).unwrap()
    );
    let chunks = |bytes: &[u8]| bytes.chunks(1).map(<[u8]>::to_vec).collect();
    let server = HttpServer::spawn(vec![
        Scenario::new(
            Response::chunked(chunks(init.as_bytes()))
                .header("content-type", "text/event-stream; charset=utf-8"),
        ),
        Scenario::new(initialized()),
        Scenario::new(
            Response::chunked(chunks(tool.as_bytes())).header("content-type", "text/event-stream"),
        ),
    ])
    .await;
    assert_eq!(run(&server).await.as_deref(), Some(SOURCE));
    assert_eq!(server.requests().len(), 3);
    server.shutdown().await;
}

#[tokio::test]
async fn rejects_unverified_rpc_without_retry() {
    let wrong = json!({"jsonrpc":"2.0","id":3,"result":{"content":[]}});
    for response in [
        Response::json(wrong.clone()),
        Response::json(json!({"jsonrpc":"1.0","id":2,"result":{"content":[]}})),
        Response::json(json!({"jsonrpc":"2.0","id":"2","result":{"content":[]}})),
        Response::json(json!({"jsonrpc":"2.0","id":2,"error":{"code":-1},"result":{"content":[]}})),
        Response::json(json!({"jsonrpc":"2.0","id":2,"result":null})),
        Response::json(json!([result(SOURCE)])),
        Response::bytes(b"not JSON".to_vec()).header("content-type", "application/json"),
        Response::bytes(serde_json::to_vec(&result(SOURCE)).unwrap())
            .header("content-type", "text/plain"),
        Response::bytes(
            format!("data: {}\n\ndata: {}\n\n", result(SOURCE), result(SOURCE)).into_bytes(),
        )
        .header("content-type", "text/event-stream"),
        Response::bytes(format!("data: {wrong}\n\n").into_bytes())
            .header("content-type", "text/event-stream"),
        Response::bytes(format!("data: {}\n", result(SOURCE)).into_bytes())
            .header("content-type", "text/event-stream"),
        Response::bytes(b"data: {\"jsonrpc\":\"1.0\",\"id\":2}\n\n".to_vec())
            .header("content-type", "text/event-stream"),
    ] {
        let server = tool_case(response).await;
        assert!(run(&server).await.is_none());
        assert_eq!(server.requests().len(), 3);
        server.shutdown().await;
    }
}

#[tokio::test]
async fn errors_empty_and_nontext_content_unavailable() {
    for value in [
        json!({"isError":true,"content":[{"type":"text","text":SOURCE}]}),
        json!({"isError":"false","content":[]}),
        json!({"content":[]}),
        json!({"content":[{"type":"text","text":"  "}]}),
        json!({"content":[{"type":"text","text":5}]}),
        json!({"content":[{"type":"image","data":"private-image"}]}),
    ] {
        let server = tool_case(Response::json(
            json!({"jsonrpc":"2.0","id":2,"result":value}),
        ))
        .await;
        assert!(run(&server).await.is_none());
        server.shutdown().await;
    }
}

#[tokio::test]
async fn unicode_scalar_bound_preserves_text_urls_only() {
    let long = format!(
        "{SOURCE}
{}",
        "🦀".repeat(4100)
    );
    let server = tool_case(Response::json(json!({"jsonrpc":"2.0","id":2,"result":{"content":[
        {"type":"image","data":"not-evidence"},{"type":"text","text":long},{"type":"text","text":"beyond-bound"}]}}))).await;
    let output = run(&server).await.unwrap();
    assert_eq!(output.chars().count(), TEXT_LIMIT);
    assert!(output.starts_with(SOURCE));
    assert!(!output.contains("not-evidence") && !output.contains("beyond-bound"));
    server.shutdown().await;
}

#[tokio::test]
async fn handshake_version_and_session_validation() {
    for response in [
        initialize().header("mcp-session-id", "bad.token"),
        initialize().header("mcp-session-id", ""),
        initialize().header("mcp-session-id", &"s".repeat(129)),
        initialize()
            .header("mcp-session-id", "one")
            .header("mcp-session-id", "two"),
        Response::json(json!({"jsonrpc":"2.0","id":0,"result":{"protocolVersion":VERSION}})),
        Response::json(json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"old"}})),
    ] {
        let server = HttpServer::spawn(vec![Scenario::new(response)]).await;
        assert!(run(&server).await.is_none());
        assert_eq!(server.requests().len(), 1);
        server.shutdown().await;
    }
    let server = HttpServer::spawn(vec![
        Scenario::new(initialize().header("mcp-session-id", "original")),
        Scenario::new(initialized().header("mcp-session-id", "changed")),
        Scenario::new(Response::bytes(Vec::new()).status(204)),
    ])
    .await;
    assert!(run(&server).await.is_none());
    assert_eq!(server.requests().len(), 3);
    assert_eq!(server.requests()[2].method, "DELETE");
    assert_eq!(
        server.requests()[2].header("mcp-session-id"),
        Some("original")
    );
    server.shutdown().await;
}

#[tokio::test]
async fn valid_session_cleaned_on_protocol_failure() {
    let server = HttpServer::spawn(vec![
        Scenario::new(
            Response::json(json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"wrong"}}))
                .header("mcp-session-id", &"s".repeat(128)),
        ),
        Scenario::new(Response::bytes(Vec::new()).status(500)),
    ])
    .await;
    assert!(run(&server).await.is_none());
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.requests()[1].method, "DELETE");
    server.shutdown().await;
}

#[tokio::test]
async fn statuses_disconnect_redirect_no_retry_at_each_stage() {
    for stage in 0..3 {
        for failure in [
            Response::bytes(Vec::new()).status(429),
            Response::bytes(Vec::new()).status(500),
            Response::disconnect(),
            Response::partial(b"{".to_vec(), 100),
            Response::bytes(Vec::new())
                .status(307)
                .header("location", "http://127.0.0.1:1/forbidden"),
        ] {
            let mut scenarios = Vec::new();
            if stage > 0 {
                scenarios.push(Scenario::new(initialize()));
            }
            if stage > 1 {
                scenarios.push(Scenario::new(initialized()));
            }
            scenarios.push(Scenario::new(failure));
            let server = HttpServer::spawn(scenarios).await;
            assert!(run(&server).await.is_none());
            assert_eq!(server.requests().len(), stage + 1);
            server.shutdown().await;
        }
    }
}

#[tokio::test]
async fn body_limit_includes_notifications_without_content_length() {
    for stage in 0..3 {
        let mut scenarios = Vec::new();
        if stage > 0 {
            scenarios.push(Scenario::new(initialize()));
        }
        if stage > 1 {
            scenarios.push(Scenario::new(initialized()));
        }
        scenarios.push(Scenario::new(
            Response::chunked(vec![vec![b' '; RESPONSE_LIMIT], vec![b' ']])
                .header("content-type", "application/json"),
        ));
        let server = HttpServer::spawn(scenarios).await;
        assert!(run(&server).await.is_none());
        assert_eq!(server.requests().len(), stage + 1);
        server.shutdown().await;
    }
    let bytes = serde_json::to_vec(&result(SOURCE)).unwrap();
    let mut exact = vec![b' '; RESPONSE_LIMIT - bytes.len()];
    exact.extend(bytes);
    let server =
        tool_case(Response::chunked(vec![exact]).header("content-type", "application/json")).await;
    assert_eq!(run(&server).await.as_deref(), Some(SOURCE));
    server.shutdown().await;
}

#[tokio::test]
async fn deadline_total_timeout_and_cancel_never_delete() {
    for mode in 0..3 {
        let (held, received, release) = Scenario::new(Response::json(result(SOURCE))).barrier();
        let server = HttpServer::spawn(vec![
            Scenario::new(initialize().header("mcp-session-id", "valid")),
            Scenario::new(initialized()),
            held,
        ])
        .await;
        let short = Duration::from_millis(100);
        let client = client(
            &server,
            if mode == 0 {
                short
            } else {
                Duration::from_secs(2)
            },
        );
        let deadline = Instant::now()
            + if mode == 1 {
                short
            } else {
                Duration::from_secs(3)
            };
        let task = tokio::spawn(async move { client.search(QUERY, deadline).await });
        received.await.unwrap();
        if mode == 2 {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert!(task.await.unwrap().is_none());
        }
        drop(release);
        assert_eq!(server.requests().len(), 3);
        server.shutdown().await;
    }
}

#[tokio::test]
async fn cancel_during_initialize_stops_without_followup() {
    let (held, received, release) = Scenario::new(initialize()).barrier();
    let server = HttpServer::spawn(vec![held]).await;
    let client = client(&server, Duration::from_secs(2));
    let task = tokio::spawn(async move {
        client
            .search(QUERY, Instant::now() + Duration::from_secs(3))
            .await
    });
    received.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(release);
    assert_eq!(server.requests().len(), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn cleanup_best_effort_capped_by_round_deadline() {
    let (held, received, release) =
        Scenario::new(Response::bytes(Vec::new()).status(204)).barrier();
    let server = HttpServer::spawn(vec![
        Scenario::new(initialize().header("mcp-session-id", "valid")),
        Scenario::new(initialized()),
        Scenario::new(Response::json(result(SOURCE))),
        held,
    ])
    .await;
    let client = client(&server, Duration::from_secs(2));
    let task = tokio::spawn(async move {
        client
            .search(QUERY, Instant::now() + Duration::from_millis(100))
            .await
    });
    assert_eq!(received.await.unwrap().method, "DELETE");
    assert_eq!(task.await.unwrap().as_deref(), Some(SOURCE));
    assert_eq!(server.requests().len(), 4);
    drop(release);
    server.shutdown().await;
}

#[tokio::test]
async fn cleanup_has_one_second_cap_and_no_retry() {
    let (held, received, release) =
        Scenario::new(Response::bytes(Vec::new()).status(204)).barrier();
    let server = HttpServer::spawn(vec![
        Scenario::new(initialize().header("mcp-session-id", "valid")),
        Scenario::new(initialized()),
        Scenario::new(Response::json(result(SOURCE))),
        held,
    ])
    .await;
    let client = client(&server, Duration::from_secs(3));
    let task = tokio::spawn(async move {
        client
            .search(QUERY, Instant::now() + Duration::from_secs(4))
            .await
    });
    assert_eq!(received.await.unwrap().method, "DELETE");
    let output = tokio::time::timeout(Duration::from_millis(1500), task)
        .await
        .expect("cleanup exceeded one second cap")
        .unwrap();
    assert_eq!(output.as_deref(), Some(SOURCE));
    assert_eq!(server.requests().len(), 4);
    drop(release);
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_queries_and_expired_round_zero_http() {
    let server = HttpServer::spawn(Vec::new()).await;
    let client = client(&server, Duration::from_secs(2));
    for query in [
        "".to_owned(),
        " padded ".to_owned(),
        "line\ncontrol".to_owned(),
        "界".repeat(241),
    ] {
        assert!(
            client
                .search(&query, Instant::now() + Duration::from_secs(1))
                .await
                .is_none()
        );
    }
    assert!(client.search(QUERY, Instant::now()).await.is_none());
    assert!(server.requests().is_empty());
    server.shutdown().await;
}
