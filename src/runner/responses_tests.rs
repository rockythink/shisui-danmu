use super::*;
use crate::runner::{
    model::SearchTarget,
    test_support::{HttpServer, Response, Scenario},
};
use std::{sync::Arc, time::Instant};

const SECRET: &str = "synthetic-private-oauth-sentinel";
fn auth() -> HeaderValue {
    HeaderValue::from_str(&format!("Bearer {SECRET}")).unwrap()
}
fn config() -> ApiConfig {
    ApiConfig {
        model: "account-selected-model".into(),
        ..ApiConfig::default()
    }
}
fn prompt() -> Prompt {
    Prompt {
        round_id: "round-current".into(),
        text: json!({"operator_context":"full profile",
        "current_messages":[{"message_id":"message-current","text":"如何理解增量游标？"}],
        "history":"private history"})
        .to_string(),
        message_ids: Arc::from(["message-current".into()]),
        cancel_epoch: 0,
        deadline: Instant::now() + Duration::from_secs(3),
        web_search: true,
        search_targets: vec![SearchTarget {
            message_id: "message-current".into(),
            query: "如何理解增量游标？".into(),
        }],
    }
}
fn candidate() -> String {
    json!({"round_id":"round-current","candidates":[{"message_id":"message-current","text":"增量游标标记已经处理的位置。"}]}).to_string()
}
fn message(text: &str) -> Value {
    json!({"id":"msg_1","type":"message","role":"assistant","status":"completed",
    "content":[{"type":"output_text","text":text,"annotations":[]}]})
}
fn call() -> Value {
    json!({"id":"fc_1","type":"function_call","namespace":"shisui_danmu","name":"web_search",
    "status":"completed","call_id":"call_1","arguments":json!({"message_id":"message-current"}).to_string()})
}
fn response(status: &str, output: Vec<Value>) -> Value {
    json!({"id":"resp_1","object":"response","status":status,"model":"actual-account-model","output":output,"error":null})
}
fn event(kind: &str, response: Value) -> Value {
    json!({"type":kind,"response":response,"sequence_number":1})
}
fn sse(value: &Value) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        value["type"].as_str().unwrap(),
        value
    )
}
fn completed(output: Vec<Value>) -> Response {
    Response::bytes(sse(&event("response.completed", response("completed", output))).into_bytes())
        .header("content-type", "text/event-stream")
}

#[tokio::test]
async fn repeated_delta_and_reasoning_events_over_256kib_admit_small_terminal() {
    let mut stream = sse(&event("response.created", response("in_progress", vec![])));
    for kind in [
        "response.output_text.delta",
        "response.reasoning_text.delta",
        "response.reasoning_summary_text.delta",
        "response.output_text.done",
    ] {
        for _ in 0..16 {
            let mut value = json!({"type":kind,"response_id":"resp_1"});
            value[if kind.ends_with(".delta") {
                "delta"
            } else {
                "text"
            }] = json!(format!("{SECRET}{}", "思考".repeat(1024)));
            stream.push_str(&sse(&value));
        }
    }
    stream.push_str(&sse(&event(
        "response.completed",
        response("completed", vec![message(&candidate())]),
    )));
    assert!(stream.len() > RESPONSE_LIMIT);
    let server = HttpServer::spawn(vec![Scenario::new(
        Response::chunked(stream.as_bytes().chunks(8191).map(<[u8]>::to_vec).collect())
            .header("content-type", "text/event-stream"),
    )])
    .await;
    let turn = round(&server, &prompt(), None).await.unwrap();
    let ResponseResult::Answer(Answer::Candidates(candidates)) = turn.result else {
        panic!("small terminal was not admitted");
    };
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].text, "增量游标标记已经处理的位置。");
    assert_eq!(server.requests().len(), 1);
    server.shutdown().await;
}

fn test_client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
async fn round(
    server: &HttpServer,
    p: &Prompt,
    continuation: Option<Continuation>,
) -> Result<ResponseTurn, ResponseError> {
    request_with_endpoint(
        &test_client(),
        auth(),
        &config(),
        p,
        continuation,
        &server.url("/v1/responses"),
    )
    .await
}
fn is_rejected(turn: ResponseTurn) {
    assert!(matches!(
        turn.result,
        ResponseResult::Answer(Answer::Rejected(_))
    ));
}

#[tokio::test]
async fn split_multiline_sse_admits_only_completed_snapshot_and_full_input() {
    let created = event("response.created", response("in_progress", vec![]));
    let delta = json!({"type":"response.output_text.delta","response_id":"resp_1","delta":SECRET,"sequence_number":2});
    let done = event(
        "response.completed",
        response(
            "completed",
            vec![
                json!({"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":SECRET}]}),
                message(&candidate()),
            ],
        ),
    );
    // Pretty JSON is legal across multiple SSE data lines; UTF-8, CRLF and field names span HTTP chunks.
    let multiline = serde_json::to_string_pretty(&done)
        .unwrap()
        .lines()
        .map(|line| format!("data: {line}\r\n"))
        .collect::<String>();
    let stream = format!(
        ": comment\r\n{}{}event: response.completed\r\n{multiline}\r\n",
        sse(&created),
        sse(&delta)
    );
    let chunks = stream.as_bytes().chunks(3).map(<[u8]>::to_vec).collect();
    let server = HttpServer::spawn(vec![
        Scenario::new(
            Response::chunked(chunks).header("content-type", "text/event-stream; charset=utf-8"),
        ),
        Scenario::new(completed(vec![message(&candidate())])),
    ])
    .await;
    let p = prompt();
    let turn = round(&server, &p, None).await.unwrap();
    assert_eq!(turn.returned_model.as_deref(), Some("actual-account-model"));
    let ResponseResult::Answer(Answer::Candidates(candidates)) = turn.result else {
        panic!("not admitted");
    };
    assert_eq!(candidates.len(), 1);
    assert!(!candidates[0].text.contains(SECRET));
    let mut followup = prompt();
    followup.text = json!({"operator_context":"full profile","history":"private history","current_question":"追问","actual_delivery_results":"confirmed response"}).to_string();
    round(&server, &followup, None).await.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for (request, prompt) in requests.iter().zip([&p, &followup]) {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/responses");
        assert!(request.header("authorization") == auth().to_str().ok());
        assert!(request.header("cookie").is_none());
        assert!(request.header("x-api-key").is_none());
        let body = request.json();
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["instructions"], crate::workspace::CONTRACT);
        assert_eq!(
            body["input"],
            json!([{"role":"user","content":prompt.text}])
        );
        assert_eq!(body["model"], "account-selected-model");
        for forbidden in [
            "previous_response_id",
            "conversation",
            "max_output_tokens",
            "max_tool_calls",
            "temperature",
            "metadata",
            "prompt",
        ] {
            assert!(body.get(forbidden).is_none());
        }
        assert!(!String::from_utf8_lossy(&request.body).contains(SECRET));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn incomplete_and_refusal_reject_even_when_candidate_text_is_present() {
    let incomplete = event(
        "response.incomplete",
        response("incomplete", vec![message(&candidate())]),
    );
    let mut refused = message(&candidate());
    refused["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"refusal","refusal":SECRET}));
    let server = HttpServer::spawn(vec![
        Scenario::new(
            Response::bytes(sse(&incomplete).into_bytes())
                .header("content-type", "text/event-stream"),
        ),
        Scenario::new(completed(vec![refused])),
    ])
    .await;
    is_rejected(round(&server, &prompt(), None).await.unwrap());
    let turn = round(&server, &prompt(), None).await.unwrap();
    assert!(!format!("{turn:?}").contains(SECRET));
    is_rejected(turn);
    server.shutdown().await;
}

#[tokio::test]
async fn content_rejections_are_whole_batch_and_do_not_poison_next_request() {
    let bodies = ["not json".into(),json!({"round_id":"old","candidates":[]}).to_string(),
        json!({"round_id":"round-current","candidates":[{"message_id":"message-current","text":"有效"},{"message_id":"old","text":"非法"}]}).to_string(),
        json!({"round_id":"round-current","candidates":[{"message_id":"message-current","text":"有效"},{"message_id":"message-current","text":"重复"}]}).to_string()];
    let mut scenarios = bodies
        .iter()
        .map(|text| Scenario::new(completed(vec![message(text)])))
        .collect::<Vec<_>>();
    scenarios.push(Scenario::new(completed(vec![message(&candidate())])));
    scenarios.push(Scenario::new(completed(vec![])));
    let server = HttpServer::spawn(scenarios).await;
    for _ in bodies {
        is_rejected(round(&server, &prompt(), None).await.unwrap());
    }
    assert!(matches!(
        round(&server, &prompt(), None).await.unwrap().result,
        ResponseResult::Answer(Answer::Candidates(_))
    ));
    assert!(matches!(
        round(&server, &prompt(), None).await.unwrap().result,
        ResponseResult::Answer(Answer::NoMessage)
    ));
    assert_eq!(server.requests().len(), 6);
    server.shutdown().await;
}

#[tokio::test]
async fn failed_and_http_quota_errors_keep_only_whitelisted_code_and_safe_request_id() {
    let codes = [
        "subscription_sharing_usage_limit_exceeded",
        "subscription_sharing_usage_unavailable",
        "subscription_sharing_user_not_eligible",
    ];
    let mut scenarios = Vec::new();
    for code in codes {
        let mut failed = response("failed", vec![message(&candidate())]);
        failed["error"] = json!({"code":code,"message":SECRET,"param":SECRET});
        scenarios.push(Scenario::new(
            Response::bytes(sse(&event("response.failed", failed)).into_bytes())
                .header("content-type", "text/event-stream")
                .header("x-request-id", "req_safe-1"),
        ));
        scenarios.push(Scenario::new(
            Response::json(
                json!({"error":{"code":code,"message":SECRET,"param":SECRET},"detail":SECRET}),
            )
            .status(429)
            .header("x-request-id", "req_safe-2"),
        ));
    }
    scenarios.push(Scenario::new(
        Response::json(json!({"detail":SECRET}))
            .status(503)
            .header("x-request-id", "unsafe private value"),
    ));
    scenarios.push(Scenario::new(
        Response::json(json!({"error":{"code":SECRET,"message":SECRET}})).status(401),
    ));
    scenarios.push(Scenario::new(Response::bytes(sse(&json!({"type":"error","code":"subscription_sharing_usage_limit_exceeded","message":SECRET})).into_bytes()).header("content-type","text/event-stream")));
    let server = HttpServer::spawn(scenarios).await;
    for code in codes {
        let failed = round(&server, &prompt(), None).await.unwrap_err();
        assert!(
            matches!(&failed,ResponseError::Failed{code:Some(observed),request_id:Some(id)} if *observed==code && id=="req_safe-1")
        );
        assert!(!format!("{failed:?} {failed}").contains(SECRET));
        let http = round(&server, &prompt(), None).await.unwrap_err();
        assert!(
            matches!(&http,ResponseError::Http{status:429,code:Some(observed),request_id:Some(id)} if *observed==code && id=="req_safe-2")
        );
        assert!(!format!("{http:?} {http}").contains(SECRET));
    }
    assert_eq!(
        round(&server, &prompt(), None).await.unwrap_err(),
        ResponseError::Http {
            status: 503,
            code: None,
            request_id: None
        }
    );
    assert_eq!(
        round(&server, &prompt(), None).await.unwrap_err(),
        ResponseError::Http {
            status: 401,
            code: None,
            request_id: None
        }
    );
    assert!(matches!(
        round(&server, &prompt(), None).await.unwrap_err(),
        ResponseError::Failed {
            code: Some("subscription_sharing_usage_limit_exceeded"),
            ..
        }
    ));
    assert_eq!(server.requests().len(), 9); // No retry or paid fallback.
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_streams_and_wrong_envelopes_never_admit_delta_candidates() {
    let valid = event(
        "response.completed",
        response("completed", vec![message(&candidate())]),
    );
    let mut wrong_id = valid.clone();
    wrong_id["response"]["id"] = json!("resp_other");
    let created = sse(&event("response.created", response("in_progress", vec![])));
    let mut wrong_object = valid.clone();
    wrong_object["response"]["object"] = json!("chat.completion");
    let mut wrong_status = valid.clone();
    wrong_status["response"]["status"] = json!("in_progress");
    let mut wrong_role = valid.clone();
    wrong_role["response"]["output"][0]["role"] = json!("user");
    let mut wrong_text = valid.clone();
    wrong_text["response"]["output"][0]["content"][0]["text"] = json!({"private":SECRET});
    let mut wrong_output = valid.clone();
    wrong_output["response"]["output"] = json!("not array");
    let mut wrong_id_type = valid.clone();
    wrong_id_type["response"]["id"] = json!(27);
    let streams = vec![
        sse(&json!({"type":"response.output_text.delta","delta":candidate()})),
        format!("{created}{}", sse(&wrong_id)),
        sse(&wrong_object),
        sse(&wrong_status),
        sse(&wrong_role),
        sse(&wrong_text),
        sse(&wrong_output),
        sse(&wrong_id_type),
        "data: {bad json}\n\n".into(),
        sse(&valid).trim_end().to_owned(),
        format!("event: response.failed\ndata: {valid}\n\n"),
        format!("{}{}", sse(&valid), sse(&valid)),
        sse(&json!({"type":"response.output_text.delta","delta":7})),
        sse(&json!({"type":"response.unknown","detail":SECRET})),
        "data: [DONE]\n\n".into(),
    ];
    let server = HttpServer::spawn(
        streams
            .into_iter()
            .map(|stream| {
                Scenario::new(
                    Response::bytes(stream.into_bytes())
                        .header("content-type", "text/event-stream"),
                )
            })
            .collect(),
    )
    .await;
    for _ in 0..15 {
        let error = round(&server, &prompt(), None).await.unwrap_err();
        assert!(matches!(error, ResponseError::Protocol(_)));
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn overflow_disconnect_wrong_content_type_and_redirect_are_fatal_without_retry() {
    let server = HttpServer::spawn(vec![
        Scenario::new(
            Response::chunked(vec![vec![b'x'; RESPONSE_LIMIT], vec![b'x']])
                .header("content-type", "text/event-stream"),
        ),
        Scenario::new(
            Response::bytes(vec![b'x'; RESPONSE_LIMIT + 1])
                .header("content-type", "text/event-stream"),
        ),
        Scenario::new(
            Response::partial(b"data: ".to_vec(), 100).header("content-type", "text/event-stream"),
        ),
        Scenario::new(Response::json(event(
            "response.completed",
            response("completed", vec![message(&candidate())]),
        ))),
        Scenario::new(Response::bytes(vec![0xff]).header("content-type", "text/event-stream")),
        Scenario::new(
            Response::bytes(vec![])
                .status(302)
                .header("location", "http://127.0.0.1:1/leak"),
        ),
    ])
    .await;
    for index in 0..6 {
        let error = round(&server, &prompt(), None).await.unwrap_err();
        match index {
            2 => assert!(matches!(error, ResponseError::Transport(_))),
            5 => assert!(matches!(error, ResponseError::Http { status: 302, .. })),
            _ => assert!(matches!(error, ResponseError::Protocol(_))),
        }
    }
    assert_eq!(server.requests().len(), 6);
    server.shutdown().await;
}

#[tokio::test]
async fn namespace_call_and_continuation_include_only_accepted_call_and_untrusted_result() {
    let server = HttpServer::spawn(vec![
        Scenario::new(completed(vec![message(SECRET), call()])),
        Scenario::new(completed(vec![message(&candidate())])),
        Scenario::new(completed(vec![call()])),
    ])
    .await;
    let p = prompt();
    let ResponseResult::ToolCall(tool) = round(&server, &p, None).await.unwrap().result else {
        panic!("no call");
    };
    assert_eq!(tool.message_id(), "message-current");
    let output = "本轮搜索不可用，未核实";
    round(&server, &p, Some(tool.into_continuation(output.into())))
        .await
        .unwrap();
    let requests = server.requests();
    let first = requests[0].json();
    assert_eq!(first["tools"].as_array().unwrap().len(), 1);
    assert_eq!(first["tools"][0]["type"], "namespace");
    assert_eq!(first["tools"][0]["name"], "shisui_danmu");
    assert_eq!(first["parallel_tool_calls"], false);
    assert_eq!(
        first["tools"][0]["tools"],
        json!([{"type":"function","name":"web_search","strict":true,"parameters":{"type":"object","properties":{"message_id":{"type":"string","enum":["message-current"]}},"required":["message_id"],"additionalProperties":false}}])
    );
    let second = requests[1].json();
    assert!(second.get("tools").is_none());
    assert!(second.get("previous_response_id").is_none());
    assert_eq!(second["input"].as_array().unwrap().len(), 3);
    assert_eq!(second["input"][0]["content"], p.text);
    assert_eq!(
        second["input"][1],
        json!({"type":"function_call","namespace":"shisui_danmu","name":"web_search","call_id":"call_1","arguments":"{\"message_id\":\"message-current\"}"})
    );
    assert_eq!(
        second["input"][2],
        json!({"type":"function_call_output","call_id":"call_1","output":output})
    );
    assert!(
        second["instructions"]
            .as_str()
            .unwrap()
            .ends_with(BUDGET_USED)
    );
    assert!(!String::from_utf8_lossy(&requests[1].body).contains(SECRET));
    // A second-turn tool is rejected, not turned into an executable third-turn continuation.
    let tool = ToolCall {
        call_id: "call_1".into(),
        message_id: "message-current".into(),
    };
    is_rejected(
        round(&server, &p, Some(tool.into_continuation("sources".into())))
            .await
            .unwrap(),
    );
    assert_eq!(server.requests().len(), 3);
    server.shutdown().await;
}

#[tokio::test]
async fn illegal_function_calls_reject_every_item_and_never_return_executable_subset() {
    let mut calls = Vec::new();
    for (field, value) in [
        ("namespace", json!("other")),
        ("name", json!("shell")),
        ("call_id", json!("")),
        ("call_id", json!("x\ny")),
        ("call_id", json!("x".repeat(129))),
        ("arguments", json!("{\"message_id\":\"old\"}")),
        (
            "arguments",
            json!("{\"message_id\":\"message-current\",\"query\":\"private-secret\"}"),
        ),
        (
            "arguments",
            json!("{\"message_id\":\"message-current\",\"message_id\":\"message-current\"}"),
        ),
        ("arguments", json!({"message_id":"message-current"})),
        ("status", json!("in_progress")),
        ("caller", json!({"type":"program","caller_id":"program_1"})),
        ("async", json!(true)),
    ] {
        let mut illegal = call();
        illegal[field] = value;
        calls.push(vec![illegal]);
    }
    calls.push(vec![call(), call()]);
    calls.push(vec![
        call(),
        json!({"type":"web_search_call","status":"completed"}),
    ]);
    calls.push(vec![
        json!({"type":"mcp_call","name":"web_search","arguments":"{}"}),
    ]);
    let count = calls.len();
    let server = HttpServer::spawn(
        calls
            .into_iter()
            .map(|output| Scenario::new(completed(output)))
            .collect(),
    )
    .await;
    for _ in 0..count {
        is_rejected(round(&server, &prompt(), None).await.unwrap());
    }
    assert_eq!(server.requests().len(), count);
    server.shutdown().await;
}

#[tokio::test]
async fn search_disabled_or_no_frozen_targets_omits_tools_and_rejects_calls() {
    let server = HttpServer::spawn(vec![
        Scenario::new(completed(vec![call()])),
        Scenario::new(completed(vec![call()])),
    ])
    .await;
    let mut p = prompt();
    p.web_search = false;
    is_rejected(round(&server, &p, None).await.unwrap());
    p.web_search = true;
    p.search_targets.clear();
    is_rejected(round(&server, &p, None).await.unwrap());
    for request in server.requests() {
        assert!(request.json().get("tools").is_none());
    }
    server.shutdown().await;
}

#[tokio::test]
async fn request_context_and_wire_limits_fail_before_http_and_tokens_never_appear_in_errors() {
    let server = HttpServer::spawn(vec![]).await;
    let mut api = config();
    let p = prompt();
    let client = test_client();
    api.base_url = server.base_url();
    let error = request_at(
        &client,
        auth(),
        &api,
        &p,
        None,
        &server.url("/v1/responses"),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ResponseError::Configuration(_)));
    let mut huge = prompt();
    huge.text = "x".repeat(CONTEXT_LIMIT);
    assert!(matches!(
        round(&server, &huge, None).await.unwrap_err(),
        ResponseError::Configuration("应用输入超过24KiB")
    ));
    // JSON escaping expands the wire body without expanding the application's UTF-8 byte budget.
    huge.text = "\0".repeat(CONTEXT_LIMIT - crate::workspace::CONTRACT.len());
    assert!(matches!(
        round(&server, &huge, None).await.unwrap_err(),
        ResponseError::Configuration("HTTP请求超过48KiB")
    ));
    let call = ToolCall {
        call_id: "call_1".into(),
        message_id: "message-current".into(),
    };
    assert!(matches!(
        round(
            &server,
            &p,
            Some(call.into_continuation("x".repeat(SEARCH_LIMIT + 1)))
        )
        .await
        .unwrap_err(),
        ResponseError::Configuration(_)
    ));
    assert!(server.requests().is_empty());
    assert!(!format!("{error:?} {error}").contains(SECRET));
    server.shutdown().await;
}

#[tokio::test]
async fn model_catalog_uses_account_visibility_and_server_order() {
    let server=HttpServer::spawn(vec![Scenario::new(Response::json(json!({"models":[
        {"visibility":"hidden","slug":"hidden"},{"visibility":"list","slug":"second","display_name":"Second"},
        {"visibility":"list","slug":"first","display_name":"First"}]})))]).await;
    let models = list_models_at(&test_client(), auth(), &server.url("/v1/models"))
        .await
        .unwrap();
    assert_eq!(
        models,
        vec![
            ModelChoice {
                slug: "second".into(),
                display_name: "Second".into()
            },
            ModelChoice {
                slug: "first".into(),
                display_name: "First".into()
            }
        ]
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/v1/models");
    assert!(requests[0].body.is_empty());
    assert!(requests[0].header("authorization") == auth().to_str().ok());
    server.shutdown().await;
}

#[tokio::test]
async fn catalog_errors_never_invent_choices_or_leak_server_text() {
    let invalid = vec![
        json!({"data":[]}),
        json!({"models":"wrong"}),
        json!({"models":[{"visibility":7}]}),
        json!({"models":[{"visibility":"list","slug":"","display_name":"Empty"}]}),
        json!({"models":[{"visibility":"list","slug":"a","display_name":SECRET.repeat(256)}]}),
        json!({"models":[{"visibility":"list","slug":"a","display_name":"A"},{"visibility":"list","slug":"a","display_name":"A"}]}),
    ];
    let mut scenarios = invalid
        .into_iter()
        .map(|value| Scenario::new(Response::json(value)))
        .collect::<Vec<_>>();
    scenarios.push(Scenario::new(
        Response::json(json!({"detail":SECRET})).status(403),
    ));
    scenarios.push(Scenario::new(Response::chunked(vec![vec![
        b'x';
        MODEL_CATALOG_LIMIT
            + 1
    ]])));
    let server = HttpServer::spawn(scenarios).await;
    for _ in 0..8 {
        let error = list_models_at(&test_client(), auth(), &server.url("/v1/models"))
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
    assert_eq!(server.requests().len(), 8);
    server.shutdown().await;
}

#[tokio::test]
async fn dropping_inflight_request_cannot_admit_a_late_candidate() {
    let (scenario, received, release) =
        Scenario::new(completed(vec![message(&candidate())])).barrier();
    let server = HttpServer::spawn(vec![scenario]).await;
    let p = prompt();
    let client = test_client();
    let api = config();
    let endpoint = server.url("/v1/responses");
    {
        let request = request_at(&client, auth(), &api, &p, None, &endpoint);
        tokio::pin!(request);
        tokio::select! { _ = received => {}, result = &mut request => panic!("request completed before release: {result:?}") }
        // Leaving this scope drops the response future, just as the parent cancellation branch does.
    }
    let _ = release.send(());
    assert_eq!(server.requests().len(), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn terminal_calls_allow_documented_optional_status_and_cr_only_sse() {
    let mut accepted = call();
    accepted.as_object_mut().unwrap().remove("status");
    let stream = sse(&event(
        "response.completed",
        response("completed", vec![accepted]),
    ))
    .replace('\n', "\r");
    let server = HttpServer::spawn(vec![Scenario::new(
        Response::chunked(stream.as_bytes().chunks(7).map(<[u8]>::to_vec).collect())
            .header("content-type", "text/event-stream"),
    )])
    .await;
    assert!(matches!(
        round(&server, &prompt(), None).await.unwrap().result,
        ResponseResult::ToolCall(_)
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn model_catalog_count_is_bounded_even_for_hidden_entries() {
    let visible = (0..256)
        .map(|index| {
            json!({"visibility":"list", "slug":format!("model-{index}"),
        "display_name":format!("Model {index}")})
        })
        .collect::<Vec<_>>();
    let mut mixed = visible.clone();
    mixed.extend(std::iter::repeat_n(
        json!({"visibility":"hidden", "slug":null}),
        768,
    ));
    let mut too_many_visible = visible.clone();
    too_many_visible.push(json!({"visibility":"list", "slug":"extra", "display_name":"Extra"}));
    let server = HttpServer::spawn(vec![
        Scenario::new(Response::json(json!({"models":mixed}))),
        Scenario::new(Response::json(
            json!({"models":vec![json!({"visibility":"hidden"});1025]}),
        )),
        Scenario::new(Response::json(json!({"models":too_many_visible}))),
    ])
    .await;
    let choices = list_models_at(&test_client(), auth(), &server.url("/v1/models"))
        .await
        .unwrap();
    assert_eq!(choices.len(), 256);
    for (index, choice) in choices.iter().enumerate() {
        assert_eq!(choice.slug, format!("model-{index}"));
        assert_eq!(choice.display_name, format!("Model {index}"));
    }
    for _ in 0..2 {
        assert!(matches!(
            list_models_at(&test_client(), auth(), &server.url("/v1/models"))
                .await
                .unwrap_err(),
            ResponseError::Protocol(_)
        ));
    }
    assert_eq!(server.requests().len(), 3);
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_ignored_metadata_remains_a_protocol_error() {
    let bad_events = vec![
        json!({"type":"response.output_item.done","item":7}),
        json!({"type":"response.content_part.added","part":{"type":7}}),
        json!({"type":"response.output_text.done","text":7}),
        json!({"type":"response.output_text.delta","delta":"ignored","item_id":7}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"ignored","summary_index":"bad"}),
    ];
    let done = sse(&event(
        "response.completed",
        response("completed", vec![message(&candidate())]),
    ));
    let mut scenarios = bad_events
        .into_iter()
        .map(|bad| {
            Scenario::new(
                Response::bytes(format!("{}{done}", sse(&bad)).into_bytes())
                    .header("content-type", "text/event-stream"),
            )
        })
        .collect::<Vec<_>>();
    scenarios.push(Scenario::new(completed(vec![
        json!({"type":"reasoning","id":"rs_1","summary":"wrong"}),
        message(&candidate()),
    ])));
    let server = HttpServer::spawn(scenarios).await;
    for _ in 0..6 {
        assert!(matches!(
            round(&server, &prompt(), None).await.unwrap_err(),
            ResponseError::Protocol(_)
        ));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn test_injection_rejects_nonloopback_wrong_route_and_url_credentials() {
    let client = test_client();
    let p = prompt();
    for endpoint in [
        "https://api.openai.com/v1/responses",
        "http://localhost:1/v1/responses",
        "http://127.0.0.1:1/v1/messages",
        "http://user@127.0.0.1:1/v1/responses",
        "http://127.0.0.1:1/v1/responses?query=private",
    ] {
        assert!(matches!(
            request_with_endpoint(&client, auth(), &config(), &p, None, endpoint)
                .await
                .unwrap_err(),
            ResponseError::Configuration(_)
        ));
    }
    let server = HttpServer::spawn(vec![Scenario::new(Response::json(json!({"models":[]})))]).await;
    assert!(
        list_models_with_endpoint(&client, auth(), &server.url("/v1/models"))
            .await
            .unwrap()
            .is_empty()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn installed_origin_routes_public_entrypoints_in_isolated_process() {
    const PHASE: &str = "DANMU_TEST_RESPONSES_ORIGIN_PHASE";
    match std::env::var(PHASE) {
        Err(std::env::VarError::NotPresent) => {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runner::responses::tests::installed_origin_routes_public_entrypoints_in_isolated_process", "--nocapture", "--test-threads=1"])
                .env(PHASE, "child").output().unwrap();
            assert!(
                output.status.success(),
                "isolated origin child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        Ok(phase) if phase == "child" => {}
        _ => panic!("unknown isolated origin phase"),
    }
    for origin in [
        "https://api.openai.com",
        "http://localhost:1",
        "http://127.0.0.1:1/v1",
        "http://user@127.0.0.1:1",
        "http://127.0.0.1:1?private=query",
    ] {
        assert!(install_test_api_origin(origin).is_err());
    }
    let server = HttpServer::spawn(vec![
        Scenario::new(Response::json(json!({"models":[{"visibility":"list","slug":"account-selected-model","display_name":"Account model"}]}))),
        Scenario::new(completed(vec![message(&candidate())])),
    ]).await;
    install_test_api_origin(&server.url("/")).unwrap();
    assert!(install_test_api_origin(&server.url("/")).is_err());
    let client = client().unwrap();
    let models = tokio::time::timeout(Duration::from_secs(3), list_models(&client, auth()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(models.len(), 1);
    let turn = tokio::time::timeout(
        Duration::from_secs(3),
        request(&client, auth(), &config(), &prompt(), None),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        turn.result,
        ResponseResult::Answer(Answer::Candidates(_))
    ));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/v1/models");
    assert_eq!(requests[1].path, "/v1/responses");
    server.shutdown().await;
}

fn catalog_with_metadata(size: usize) -> Vec<u8> {
    let prefix = br#"{"models":[{"visibility":"hidden","slug":7,"display_name":null},{"visibility":"list","slug":"account-selected-model","display_name":"Account model","unused":{"nested":[true,null,{"blob":""#;
    let suffix = br#""}]}}],"unused_envelope":{"nested":[1,2,3]}}"#;
    assert!(size >= prefix.len() + suffix.len());
    let mut body = Vec::with_capacity(size);
    body.extend_from_slice(prefix);
    body.resize(size - suffix.len(), b'x');
    body.extend_from_slice(suffix);
    body
}
fn gzip(body: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}
fn framed_response(body: Vec<u8>, framing: usize) -> Response {
    match framing {
        0 => Response::bytes(body),
        1 => Response::chunked(body.chunks(8191).map(<[u8]>::to_vec).collect()),
        2 => Response::chunked(gzip(&body).chunks(97).map(<[u8]>::to_vec).collect())
            .header("content-encoding", "gzip"),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn large_catalog_metadata_and_inclusive_decompressed_budget_preserve_choices() {
    let scenarios = [
        RESPONSE_LIMIT + 1,
        MODEL_CATALOG_LIMIT,
        MODEL_CATALOG_LIMIT + 1,
    ]
    .into_iter()
    .flat_map(|size| {
        (0..3).map(move |framing| {
            Scenario::respond(move |_| framed_response(catalog_with_metadata(size), framing))
        })
    })
    .collect();
    let server = HttpServer::spawn(scenarios).await;
    for size in [
        RESPONSE_LIMIT + 1,
        MODEL_CATALOG_LIMIT,
        MODEL_CATALOG_LIMIT + 1,
    ] {
        for _ in 0..3 {
            let result = list_models_at(&test_client(), auth(), &server.url("/v1/models")).await;
            if size <= MODEL_CATALOG_LIMIT {
                assert_eq!(
                    result.unwrap(),
                    vec![ModelChoice {
                        slug: "account-selected-model".into(),
                        display_name: "Account model".into(),
                    }]
                );
            } else {
                let error = result.unwrap_err();
                assert!(matches!(error, ResponseError::Protocol(_)));
                assert!(!format!("{error:?} {error}").contains(SECRET));
            }
        }
    }
    assert_eq!(server.requests().len(), 9);
    server.shutdown().await;
}

#[tokio::test]
async fn catalog_http_errors_keep_small_budget_and_whitelisted_diagnostics() {
    let code = "subscription_sharing_usage_limit_exceeded";
    let scenarios = [RESPONSE_LIMIT, RESPONSE_LIMIT + 1]
        .into_iter()
        .flat_map(|size| {
            (0..3).map(move |framing| {
                Scenario::respond(move |_| {
                    let mut body =
                        serde_json::to_vec(&json!({"error":{"code":code,"message":SECRET}}))
                            .unwrap();
                    body.resize(size, b' ');
                    framed_response(body, framing)
                        .status(429)
                        .header("x-request-id", "req_safe-1")
                })
            })
        })
        .collect();
    let server = HttpServer::spawn(scenarios).await;
    for size in [RESPONSE_LIMIT, RESPONSE_LIMIT + 1] {
        for _ in 0..3 {
            let error = list_models_at(&test_client(), auth(), &server.url("/v1/models"))
                .await
                .unwrap_err();
            if size == RESPONSE_LIMIT {
                assert!(matches!(&error, ResponseError::Http {
                    status: 429, code: Some(observed), request_id: Some(id),
                } if *observed == code && id == "req_safe-1"));
            } else {
                assert!(matches!(error, ResponseError::Protocol(_)));
            }
            assert!(!format!("{error:?} {error}").contains(SECRET));
        }
    }
    assert_eq!(server.requests().len(), 6);
    server.shutdown().await;
}

fn terminal_stream() -> String {
    sse(&event(
        "response.completed",
        response("completed", vec![message(&candidate())]),
    ))
}
fn assert_candidate(turn: ResponseTurn) {
    let ResponseResult::Answer(Answer::Candidates(candidates)) = turn.result else {
        panic!("expected terminal candidate");
    };
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].message_id, "message-current");
    assert_eq!(candidates[0].text, "增量游标标记已经处理的位置。");
}
fn sized_delta(size: usize) -> String {
    let mut value = json!({"type":"response.reasoning_text.delta",
        "response_id":"resp_1","delta":""});
    let overhead = sse(&value).len();
    assert!(size >= overhead);
    value["delta"] = json!("x".repeat(size - overhead));
    let event = sse(&value);
    assert_eq!(event.len(), size);
    event
}
fn sized_stream(size: usize) -> Vec<u8> {
    let terminal = terminal_stream();
    let mut bytes = Vec::with_capacity(size);
    let mut remaining = size - terminal.len();
    while remaining > 64 * 1024 {
        // Leave enough bytes for a final real JSON event, not arbitrary padding.
        let next = (64 * 1024).min(remaining - 256);
        bytes.extend_from_slice(sized_delta(next).as_bytes());
        remaining -= next;
    }
    bytes.extend_from_slice(sized_delta(remaining).as_bytes());
    bytes.extend_from_slice(terminal.as_bytes());
    assert_eq!(bytes.len(), size);
    bytes
}

#[tokio::test]
async fn sse_decompressed_stream_budget_is_inclusive_across_all_framings() {
    let server = HttpServer::spawn(
        [SSE_STREAM_LIMIT, SSE_STREAM_LIMIT + 1]
            .into_iter()
            .flat_map(|size| {
                (0..3).map(move |framing| {
                    Scenario::respond(move |_| {
                        framed_response(sized_stream(size), framing)
                            .header("content-type", "text/event-stream")
                    })
                })
            })
            .collect(),
    )
    .await;
    for size in [SSE_STREAM_LIMIT, SSE_STREAM_LIMIT + 1] {
        for _ in 0..3 {
            let result = round(&server, &prompt(), None).await;
            if size == SSE_STREAM_LIMIT {
                assert_candidate(result.unwrap());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    ResponseError::Protocol("SSE流超过8MiB")
                );
            }
        }
    }
    assert_eq!(server.requests().len(), 6);
    server.shutdown().await;
}

#[tokio::test]
async fn sse_event_budget_counts_data_fields_comments_lines_and_terminal_snapshot() {
    let mut bodies = Vec::new();
    for size in [SSE_EVENT_LIMIT, SSE_EVENT_LIMIT + 1] {
        // JSON data, a single comment, many short fields/comments, and the terminal
        // snapshot all consume the same per-event budget, including delimiters.
        bodies.push((format!("{}{}", sized_delta(size), terminal_stream()), size));
        let comment = format!(":{}\r\n\r\n", "x".repeat(size - 5));
        bodies.push((format!("{comment}{}", terminal_stream()), size));
        let mut fields = "id: ignored\r\nretry: 1\r\n".to_owned();
        while fields.len() + 7 < size {
            fields.push_str(": x\r\n");
        }
        fields.push(':');
        fields.extend(std::iter::repeat_n('x', size - fields.len() - 4));
        fields.push_str("\r\n\r\n");
        assert_eq!(fields.len(), size);
        bodies.push((format!("{fields}{}", terminal_stream()), size));
        let mut value = event(
            "response.completed",
            response("completed", vec![message(&candidate())]),
        );
        value["response"]["metadata"] = json!("");
        let overhead = sse(&value).len();
        value["response"]["metadata"] = json!("x".repeat(size - overhead));
        let terminal = sse(&value);
        assert_eq!(terminal.len(), size);
        bodies.push((terminal, size));
    }
    let expectations: Vec<_> = bodies.iter().flat_map(|(_, size)| [*size; 3]).collect();
    let server = HttpServer::spawn(
        bodies
            .into_iter()
            .flat_map(|(body, _)| {
                (0..3).map(move |framing| {
                    let bytes = body.as_bytes().to_vec();
                    Scenario::new(
                        framed_response(bytes, framing).header("content-type", "text/event-stream"),
                    )
                })
            })
            .collect(),
    )
    .await;
    for size in expectations {
        let result = round(&server, &prompt(), None).await;
        if size == SSE_EVENT_LIMIT {
            assert_candidate(result.unwrap());
        } else {
            assert_eq!(
                result.unwrap_err(),
                ResponseError::Protocol("SSE事件超过256KiB")
            );
        }
    }
    assert_eq!(server.requests().len(), 24);
    server.shutdown().await;
}

#[tokio::test]
async fn sse_data_event_count_includes_terminal_and_done_but_not_comments() {
    let delta =
        sse(&json!({"type":"response.output_text.delta","response_id":"resp_1","delta":SECRET}));
    let scenarios = [SSE_DATA_EVENT_LIMIT, SSE_DATA_EVENT_LIMIT + 1]
        .into_iter()
        .flat_map(|count| {
            [false, true].into_iter().map({
                let delta = delta.clone();
                move |done| {
                    let mut stream = String::new();
                    for _ in 0..count - 1 - usize::from(done) {
                        stream.push_str(": comment\n\n");
                        stream.push_str(&delta);
                    }
                    stream.push_str(&terminal_stream());
                    if done {
                        stream.push_str("data: [DONE]\n\n");
                    }
                    Scenario::new(
                        framed_response(stream.into_bytes(), 1)
                            .header("content-type", "text/event-stream"),
                    )
                }
            })
        })
        .collect();
    let server = HttpServer::spawn(scenarios).await;
    for count in [SSE_DATA_EVENT_LIMIT, SSE_DATA_EVENT_LIMIT + 1] {
        for _ in 0..2 {
            let result = round(&server, &prompt(), None).await;
            if count == SSE_DATA_EVENT_LIMIT {
                assert_candidate(result.unwrap());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    ResponseError::Protocol("SSE数据事件超过4096个")
                );
            }
        }
    }
    assert_eq!(server.requests().len(), 4);
    server.shutdown().await;
}

#[tokio::test]
async fn responses_http_errors_keep_256kib_decompressed_budget() {
    let server = HttpServer::spawn(
        [RESPONSE_LIMIT, RESPONSE_LIMIT + 1]
            .into_iter()
            .flat_map(|size| {
                (0..3).map(move |framing| {
                    let mut bytes = serde_json::to_vec(&json!({"error":{
                "code":"subscription_sharing_usage_limit_exceeded","message":SECRET}}))
                    .unwrap();
                    bytes.resize(size, b' ');
                    Scenario::new(framed_response(bytes, framing).status(429))
                })
            })
            .collect(),
    )
    .await;
    for size in [RESPONSE_LIMIT, RESPONSE_LIMIT + 1] {
        for _ in 0..3 {
            let error = round(&server, &prompt(), None).await.unwrap_err();
            if size == RESPONSE_LIMIT {
                assert!(matches!(
                    error,
                    ResponseError::Http {
                        status: 429,
                        code: Some("subscription_sharing_usage_limit_exceeded"),
                        ..
                    }
                ));
            } else {
                assert_eq!(error, ResponseError::Protocol("HTTP错误响应超过256KiB"));
            }
            assert!(!format!("{error:?} {error}").contains(SECRET));
        }
    }
    assert_eq!(server.requests().len(), 6);
    server.shutdown().await;
}

#[tokio::test]
async fn sse_chunk_boundaries_preserve_bom_utf8_cr_crlf_and_multiline_data() {
    let value = event(
        "response.completed",
        response("completed", vec![message(&candidate())]),
    );
    let multiline = serde_json::to_string_pretty(&value)
        .unwrap()
        .lines()
        .map(|line| format!("data: {line}\r\n"))
        .collect::<String>();
    let stream = format!(
        "\u{feff}: 注释\r: ignored\r\nevent: response.completed\r\n{multiline}\r\ndata: [DONE]\r\r"
    );
    let p = prompt();
    // One-byte feeds force every UTF-8/BOM and CRLF boundary; HTTP may coalesce chunks.
    let mut parser = SseParser::new(&p, false, None);
    for byte in stream.as_bytes() {
        parser.feed(std::slice::from_ref(byte)).unwrap();
    }
    assert_candidate(parser.finish().unwrap());
    let server = HttpServer::spawn(vec![Scenario::new(
        Response::chunked(stream.as_bytes().chunks(1).map(<[u8]>::to_vec).collect())
            .header("content-type", "text/event-stream"),
    )])
    .await;
    assert_candidate(round(&server, &p, None).await.unwrap());
    server.shutdown().await;
}

#[tokio::test]
async fn terminal_does_not_hide_invalid_utf8_unfinished_or_malformed_late_data() {
    let mut bodies = Vec::new();
    for suffix in [
        b"data: [DONE]\n\ndata: {bad}\n\n".as_slice(),
        b"data: [DONE]",
        b"data: ",
        b"data\n",
        b": \xff\n\n",
        b"event: response.completed\nevent: response.completed\n\n",
    ] {
        let mut body = terminal_stream().into_bytes();
        body.extend_from_slice(suffix);
        bodies.push(body);
    }
    let count = bodies.len();
    let server = HttpServer::spawn(
        bodies
            .into_iter()
            .map(|body| {
                Scenario::new(framed_response(body, 1).header("content-type", "text/event-stream"))
            })
            .collect(),
    )
    .await;
    for _ in 0..count {
        let error = round(&server, &prompt(), None).await.unwrap_err();
        assert!(matches!(error, ResponseError::Protocol(_)));
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn projected_catalog_still_rejects_malformed_json_hidden_items_and_trailing_data() {
    let bodies: &[&[u8]] = &[
        br#"[{"models":[]}]"#,
        br#"{"models":[7]}"#,
        br#"{"models":[{"slug":"hidden"}]}"#,
        br#"{"models":[{"visibility":null}]}"#,
        br#"{"models":[{"visibility":true}]}"#,
        br#"{"models":[{"visibility":"list","slug":7,"display_name":"Name"}]}"#,
        br#"{"models":[{"visibility":"list","slug":"a","display_name":null}]}"#,
        br#"{"models":[{"visibility":"list","slug":"a","display_name":"Name\n"}]}"#,
        br#"{"models":[{"visibility":"hidden","unused":[true,]}]}"#,
        br#"{"models":[],"unused":{"bad":"\uD800"}}"#,
        br#"{"models":[]} {"models":[]}"#,
        br#"{"models":[]} trailing"#,
        b"{\"models\":[],\"unused\":\"\xff\"}",
    ];
    let server = HttpServer::spawn(
        bodies
            .iter()
            .map(|body| Scenario::new(Response::bytes(body.to_vec())))
            .collect(),
    )
    .await;
    for (index, _) in bodies.iter().enumerate() {
        let result = list_models_at(&test_client(), auth(), &server.url("/v1/models")).await;
        assert!(
            matches!(result, Err(ResponseError::Protocol(_))),
            "case {index}: {result:?}"
        );
    }
    assert_eq!(server.requests().len(), bodies.len());
    server.shutdown().await;
}

// A separate mid-body barrier proves that parsing a terminal is not admission.
// The shared fixture's barrier intentionally occurs before response headers.
async fn gated_sse_tail(
    prefix: Vec<u8>,
    tail: Vec<u8>,
    disconnect: bool,
) -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let (written_tx, written_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() <= 64 * 1024);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..end]).unwrap();
                let length: usize = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .unwrap()
                    .1
                    .trim()
                    .parse()
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let length = prefix.len() + tail.len() + usize::from(disconnect);
        socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n").as_bytes())
            .await.unwrap();
        socket.write_all(&prefix).await.unwrap();
        socket.flush().await.unwrap();
        written_tx.send(()).unwrap();
        if release_rx.await.is_ok() {
            // A cancelled client may already have closed; no candidate is retried.
            let _ = socket.write_all(&tail).await;
            let _ = socket.shutdown().await;
        }
    });
    (endpoint, written_rx, release_tx, task)
}

#[tokio::test]
async fn parsed_terminal_waits_for_eof_and_late_invalid_events_or_disconnect_invalidate_it() {
    for mode in 0..3 {
        let prefix = format!("{}{}", sized_delta(64 * 1024).repeat(5), terminal_stream());
        assert!(prefix.len() > RESPONSE_LIMIT);
        let tail = if mode == 1 {
            sse(&json!({"type":"response.output_text.delta","response_id":"resp_1","delta":SECRET}))
        } else {
            "data: [DONE]\n\n".to_owned()
        };
        let (endpoint, written, release, server) =
            gated_sse_tail(prefix.into_bytes(), tail.into_bytes(), mode == 2).await;
        let client = test_client();
        let api = config();
        let p = prompt();
        let mut request = Box::pin(request_with_endpoint(
            &client,
            auth(),
            &api,
            &p,
            None,
            &endpoint,
        ));
        tokio::select! {
            result = &mut request => panic!("admitted before terminal EOF: {result:?}"),
            result = written => result.unwrap(),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut request)
                .await
                .is_err(),
            "terminal admitted before HTTP EOF"
        );
        release.send(()).unwrap();
        let result = request.await;
        match mode {
            0 => assert_candidate(result.unwrap()),
            1 => assert_eq!(
                result.unwrap_err(),
                ResponseError::Protocol("终态后仍有事件")
            ),
            2 => assert_eq!(result.unwrap_err(), ResponseError::Transport("响应中断")),
            _ => unreachable!(),
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_or_expiring_a_stream_reader_discards_already_parsed_terminal() {
    for expired in [false, true] {
        let prefix = format!("{}{}", sized_delta(64 * 1024).repeat(5), terminal_stream());
        let (endpoint, written, release, server) =
            gated_sse_tail(prefix.into_bytes(), b"data: [DONE]\n\n".to_vec(), false).await;
        let client = test_client();
        let api = config();
        let p = prompt();
        let mut request = Box::pin(request_with_endpoint(
            &client,
            auth(),
            &api,
            &p,
            None,
            &endpoint,
        ));
        tokio::select! {
            result = &mut request => panic!("admitted before cancellation: {result:?}"),
            result = written => result.unwrap(),
        }
        // The worker wraps the entire future in its cancellation/deadline select.
        // Expiry and user cancellation both drop this reader, including its terminal.
        if expired {
            assert!(
                tokio::time::timeout_at(
                    tokio::time::Instant::now() + Duration::from_millis(20),
                    &mut request
                )
                .await
                .is_err()
            );
        }
        drop(request);
        release.send(()).unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn large_stream_still_validates_metadata_identity_and_terminal_structure() {
    let invalid = [
        json!({"type":"response.reasoning_text.delta","delta":SECRET,"summary_index":-1}),
        json!({"type":"response.output_item.done","item":{"type":7}}),
        json!({"type":"response.output_text.done","text":SECRET,"item_id":"bad\nprivate"}),
        json!({"type":"response.output_text.delta","delta":SECRET,"response_id":"resp_other"}),
        event(
            "response.completed",
            json!({"id":"resp_1","object":"response",
            "status":"completed","output":[{"type":"message","id":"msg_1",
            "role":"user","status":"completed","content":[]}]}),
        ),
    ];
    let server = HttpServer::spawn(
        invalid
            .into_iter()
            .map(|value| {
                let stream = format!(
                    "{}{}{}",
                    sized_delta(64 * 1024).repeat(5),
                    sse(&value),
                    terminal_stream()
                );
                Scenario::new(
                    framed_response(stream.into_bytes(), 2)
                        .header("content-type", "text/event-stream"),
                )
            })
            .collect(),
    )
    .await;
    for _ in 0..5 {
        let error = round(&server, &prompt(), None).await.unwrap_err();
        assert!(matches!(error, ResponseError::Protocol(_)));
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
    assert_eq!(server.requests().len(), 5);
    server.shutdown().await;
}
