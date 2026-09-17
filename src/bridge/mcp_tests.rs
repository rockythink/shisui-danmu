use super::*;
use crate::bridge::Bridge;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, DuplexStream};

struct Client {
    input: BufReader<tokio::io::ReadHalf<DuplexStream>>,
    output: tokio::io::WriteHalf<DuplexStream>,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl Client {
    async fn open(root: &Path) -> Self {
        let (client, server) = tokio::io::duplex(65536);
        let (input, output) = tokio::io::split(client);
        let (server_in, server_out) = tokio::io::split(server);
        let root = root.to_path_buf();
        let task = tokio::spawn(async move { run_io(&root, server_in, server_out).await });
        let mut client = Self {
            input: BufReader::new(input),
            output,
            task,
        };
        client
            .send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .await;
        assert!(client.recv().await["result"]["protocolVersion"].is_string());
        client
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        client
    }
    async fn send(&mut self, frame: Value) {
        write(&mut self.output, frame).await.unwrap();
    }
    async fn recv(&mut self) -> Value {
        let bytes = tokio::time::timeout(Duration::from_secs(3), wire::read_frame(&mut self.input))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn call(&mut self, id: Value, name: &str, args: Value) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})).await;
    }
    async fn cancel(&mut self, id: Value) {
        self.send(
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}}),
        )
        .await;
    }
    async fn quiet(&mut self, millis: u64) {
        // fill_buf is cancellation safe; don't discard a partially received frame.
        assert!(
            tokio::time::timeout(Duration::from_millis(millis), self.input.fill_buf())
                .await
                .is_err()
        );
    }
    async fn close(mut self) {
        self.output.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
fn data(response: &Value) -> Value {
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn full_completed_batch_releases_capacity_without_reopening_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("instance");
    let bridge = Bridge::new(true);
    let session = bridge.status()["session"].clone();
    let _server = wire::serve(&root, bridge).await.unwrap();
    let mut c = Client::open(&root).await;
    for round in 0..2 {
        for id in 100..116 {
            c.call(
                json!(id),
                "danmu_messages",
                json!({"session":session,"cursor":0,"wait_ms":100}),
            )
            .await;
        }
        c.call(json!(999), "danmu_status", json!({})).await;
        assert_eq!(c.recv().await["error"]["code"], -32000);
        let mut ids = std::collections::BTreeSet::new();
        for _ in 0..16 {
            let response = c.recv().await;
            assert_eq!(data(&response)["session"], session);
            assert!(ids.insert(response["id"].as_u64().unwrap()));
        }
        assert_eq!(ids, (100..116).collect());
        c.call(json!(200 + round), "danmu_status", json!({})).await;
        assert_eq!(data(&c.recv().await)["local_transport"], true);
        c.send(json!({"jsonrpc":"2.0","method":"ping","id":300}))
            .await;
        assert_eq!(c.recv().await["result"], json!({}));
    }
    c.close().await;
}

#[tokio::test]
async fn cancellation_is_connection_and_id_scoped_and_releases_full_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("instance");
    let bridge = Bridge::new(true);
    let session = bridge.status()["session"].clone();
    let _server = wire::serve(&root, bridge).await.unwrap();
    let mut a = Client::open(&root).await;
    let mut b = Client::open(&root).await;
    b.call(
        json!(900),
        "danmu_messages",
        json!({"session":session,"wait_ms":200}),
    )
    .await;
    for id in 900..916 {
        a.call(
            json!(id),
            "danmu_messages",
            json!({"session":session,"wait_ms":1200}),
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    a.cancel(json!("900")).await; // String ID must not match numeric ID.
    a.cancel(json!(12345)).await;
    a.call(json!(1000), "danmu_status", json!({})).await;
    assert_eq!(a.recv().await["error"]["code"], -32000);
    for id in 900..916 {
        a.cancel(json!(id)).await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    a.call(json!(1001), "danmu_status", json!({})).await;
    assert_eq!(data(&a.recv().await)["session"], session);
    let response = b.recv().await;
    assert_eq!(response["id"], 900);
    assert_eq!(data(&response)["messages"], json!([]));
    b.cancel(json!(900)).await; // Completed ID is harmless.
    b.call(json!(1002), "danmu_status", json!({})).await;
    assert_eq!(data(&b.recv().await)["sending_enabled"], false);
    a.quiet(1250).await;
    b.quiet(30).await;
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn completion_during_partial_input_preserves_frame_and_emits_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("instance");
    let bridge = Bridge::new(true);
    let session = bridge.status()["session"].clone();
    let _server = wire::serve(&root, bridge).await.unwrap();
    let mut c = Client::open(&root).await;
    c.call(
        json!("wait"),
        "danmu_messages",
        json!({"session":session,"wait_ms":50}),
    )
    .await;
    c.output
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":")
        .await
        .unwrap();
    assert_eq!(data(&c.recv().await)["messages"], json!([]));
    c.output
        .write_all(b"\"notifications/cancelled\",\"params\":{\"requestId\":\"wait\"}}\n")
        .await
        .unwrap();
    c.call(json!(2), "danmu_status", json!({})).await;
    assert_eq!(data(&c.recv().await)["session"], session);
    c.quiet(80).await;
    c.close().await;
}
