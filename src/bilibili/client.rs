use super::{
    CrossOriginDeduplicator, RECONNECT_DELAYS_SECONDS, encode_packet, parse_command, parse_packets,
    parser::command_name,
};
use crate::domain::{DanmuEvent, DanmuEventKind, DanmuEventOrigin};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{ACCEPT, COOKIE, ORIGIN, REFERER, SET_COOKIE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{OnceCell, mpsc, watch};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WebSocketError, Message, client::IntoClientRequest},
};
use url::Url;
use uuid::Uuid;

const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const HISTORY_RECONCILIATION_INTERVAL_SECONDS: u64 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BilibiliClientEvent {
    Connected { room_id: String },
    Disconnected { reason: String },
    Danmu(DanmuEvent),
    Heartbeat,
    Watched(u64),
    Likes(u64),
    UnhandledCommand { command: String },
    Error(String),
}

fn realtime_connection_error_event(error: anyhow::Error) -> Option<BilibiliClientEvent> {
    let recoverable = error.chain().any(|cause| {
        if let Some(error) = cause.downcast_ref::<WebSocketError>() {
            return match error {
                WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed => true,
                WebSocketError::Io(error) => is_recoverable_socket_close(error.kind()),
                _ => false,
            };
        }
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| is_recoverable_socket_close(error.kind()))
    });

    (!recoverable).then(|| BilibiliClientEvent::Error(error.to_string()))
}

fn is_recoverable_socket_close(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RoomLiveStatus {
    Offline,
    Live,
    Rotating,
}

impl TryFrom<u64> for RoomLiveStatus {
    type Error = anyhow::Error;

    fn try_from(value: u64) -> Result<Self> {
        match value {
            0 => Ok(Self::Offline),
            1 => Ok(Self::Live),
            2 => Ok(Self::Rotating),
            _ => bail!("B 站返回未知直播状态：{value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSnapshot {
    pub room_id: String,
    pub broadcaster_id: String,
    pub broadcaster_name: String,
    pub title: String,
    pub area: String,
    pub live_started_at: Option<DateTime<Utc>>,
    pub live_status: RoomLiveStatus,
}

impl RoomSnapshot {
    pub fn is_live(&self) -> bool {
        self.live_status == RoomLiveStatus::Live
    }
}

#[derive(Debug, Deserialize)]
struct RoomBaseData {
    #[serde(default)]
    by_room_ids: BTreeMap<String, RoomBaseInfo>,
}

#[derive(Debug, Deserialize)]
struct RoomBaseInfo {
    room_id: Option<u64>,
    uid: Option<u64>,
    uname: Option<String>,
    title: Option<String>,
    parent_area_name: Option<String>,
    area_name: Option<String>,
    live_time: Option<String>,
    live_status: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceIdentity {
    buvid3: String,
    buvid4: String,
}

impl DeviceIdentity {
    fn cookie_header(&self) -> String {
        format!("buvid3={}; buvid4={}", self.buvid3, self.buvid4)
    }
}

#[derive(Clone)]
pub struct BilibiliClient {
    http: reqwest::Client,
    device_identity: Arc<OnceCell<Option<DeviceIdentity>>>,
    session_path: PathBuf,
}

impl BilibiliClient {
    pub fn new(session_path: PathBuf) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT_VALUE)
            .cookie_store(true)
            .timeout(Duration::from_secs(12))
            .build()?;
        Ok(Self {
            http,
            device_identity: Arc::new(OnceCell::new()),
            session_path,
        })
    }

    async fn device_identity(&self) -> Option<DeviceIdentity> {
        self.device_identity
            .get_or_init(|| async {
                let value: Value = self
                    .http
                    .get("https://api.bilibili.com/x/frontend/finger/spi")
                    .header(ACCEPT, "application/json,text/plain,*/*")
                    .send()
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json()
                    .await
                    .ok()?;
                device_identity_from_response(&value)
            })
            .await
            .clone()
    }

    pub async fn resolve_room(&self, room_id: &str) -> Result<String> {
        let value: Value = self
            .http
            .get("https://api.live.bilibili.com/room/v1/Room/room_init")
            .query(&[("id", room_id)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&value, "解析直播间失败")?;
        value
            .pointer("/data/room_id")
            .and_then(Value::as_u64)
            .map(|id| id.to_string())
            .ok_or_else(|| anyhow!("B 站未返回有效直播间号"))
    }

    pub async fn room_snapshot(&self, room_id: &str) -> Result<RoomSnapshot> {
        self.room_snapshot_preserving(room_id, None).await
    }

    pub async fn room_snapshot_preserving(
        &self,
        room_id: &str,
        previous: Option<&RoomSnapshot>,
    ) -> Result<RoomSnapshot> {
        let mut last_error = None;
        for attempt in 0..2 {
            let result: Result<RoomSnapshot> = async {
                let value: Value = self
                    .http
                    .get("https://api.live.bilibili.com/xlive/web-room/v1/index/getRoomBaseInfo")
                    .query(&[("room_ids", room_id), ("req_biz", "web_room_componet")])
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                room_snapshot_update_from_response(&value, room_id, previous)
            }
            .await;
            match result {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) => last_error = Some(error),
            }
            if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        Err(last_error.expect("房间快照请求至少执行一次"))
    }

    pub async fn history(&self, room_id: &str) -> Result<Vec<DanmuEvent>> {
        let value: Value = self
            .http
            .get("https://api.live.bilibili.com/xlive/web-room/v1/dM/gethistory")
            .query(&[("roomid", room_id), ("room_type", "0")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&value, "读取历史弹幕失败")?;
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let mut events: Vec<_> = value
            .pointer("/data/room")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let content = item.get("text")?.as_str()?.trim();
                if content.is_empty() {
                    return None;
                }
                let author_id = string_value(item.get("uid"));
                let username = item
                    .get("nickname")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let timeline = item
                    .get("timeline")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let timestamp = NaiveDateTime::parse_from_str(timeline, "%Y-%m-%d %H:%M:%S")
                    .ok()
                    .and_then(|value| offset.from_local_datetime(&value).single())
                    .map(|value| value.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);
                let platform_id =
                    string_value(item.get("id_str")).filter(|value| !value.is_empty());
                let id = platform_id
                    .as_ref()
                    .map(|value| format!("bili-{value}"))
                    .unwrap_or_else(|| {
                        format!(
                            "bili-history-{:016x}",
                            fnv1a64(
                                format!(
                                    "{}|{timeline}|{content}",
                                    author_id.as_deref().unwrap_or_default()
                                )
                                .as_bytes()
                            )
                        )
                    });
                Some(DanmuEvent {
                    id,
                    kind: DanmuEventKind::Danmu,
                    timestamp,
                    username,
                    author_id,
                    content: content.to_string(),
                    origin: DanmuEventOrigin::History,
                    platform_event_id: platform_id,
                    emotes: Vec::new(),
                })
            })
            .collect();
        events.sort_by_key(|event| event.timestamp);
        Ok(events)
    }

    pub async fn run(
        &self,
        room_id: String,
        tx: mpsc::Sender<BilibiliClientEvent>,
        mut stop: watch::Receiver<bool>,
    ) {
        let canonical = match self.resolve_room(&room_id).await {
            Ok(value) => value,
            Err(error) => {
                let _ = tx.send(BilibiliClientEvent::Error(error.to_string())).await;
                return;
            }
        };
        for event in self.history(&canonical).await.unwrap_or_default() {
            if tx.send(BilibiliClientEvent::Danmu(event)).await.is_err() {
                return;
            }
        }
        let history_client = self.clone();
        let history_room_id = canonical.clone();
        let history_tx = tx.clone();
        let history_stop = stop.clone();
        tokio::spawn(async move {
            history_client
                .run_history_reconciliation(history_room_id, history_tx, history_stop)
                .await;
        });

        let mut attempt = 0usize;
        let mut observed_unhandled_commands = BTreeSet::new();
        loop {
            if *stop.borrow() {
                return;
            }
            match self
                .run_connection(&canonical, &tx, &mut stop, &mut observed_unhandled_commands)
                .await
            {
                Ok(()) if *stop.borrow() => return,
                Ok(()) => {}
                Err(error) => {
                    if let Some(event) = realtime_connection_error_event(error) {
                        let _ = tx.send(event).await;
                    }
                }
            }
            let delay = RECONNECT_DELAYS_SECONDS[attempt.min(RECONNECT_DELAYS_SECONDS.len() - 1)];
            attempt = (attempt + 1).min(RECONNECT_DELAYS_SECONDS.len() - 1);
            let _ = tx
                .send(BilibiliClientEvent::Disconnected {
                    reason: format!("{delay} 秒后重连"),
                })
                .await;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(delay)) => {},
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { return; },
            }
        }
    }

    async fn run_history_reconciliation(
        &self,
        room_id: String,
        tx: mpsc::Sender<BilibiliClientEvent>,
        mut stop: watch::Receiver<bool>,
    ) {
        let mut refresh =
            tokio::time::interval(Duration::from_secs(HISTORY_RECONCILIATION_INTERVAL_SECONDS));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        refresh.tick().await;
        loop {
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
                _ = refresh.tick() => {
                    let Ok(events) = self.history(&room_id).await else {
                        continue;
                    };
                    for event in events {
                        if tx.send(BilibiliClientEvent::Danmu(event)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }

    async fn run_connection(
        &self,
        room_id: &str,
        tx: &mpsc::Sender<BilibiliClientEvent>,
        stop: &mut watch::Receiver<bool>,
        observed_unhandled_commands: &mut BTreeSet<String>,
    ) -> Result<()> {
        let identity = self.device_identity().await;
        let credential = load_credential(&self.session_path)?;
        let cookie_header = realtime_cookie_header(credential.as_ref(), identity.as_ref());
        let mut config_request = self
            .http
            .get("https://api.live.bilibili.com/room/v1/Danmu/getConf")
            .query(&[("room_id", room_id), ("platform", "pc"), ("player", "web")]);
        if let Some(cookie_header) = &cookie_header {
            config_request = config_request.header(COOKIE, cookie_header);
        }
        let config: Value = config_request
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&config, "读取弹幕连接配置失败")?;
        let token = config
            .pointer("/data/token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("B 站未返回弹幕令牌"))?;
        let host = config
            .pointer("/data/host_server_list/0/host")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("B 站未返回弹幕服务器"))?;
        let port = config
            .pointer("/data/host_server_list/0/wss_port")
            .and_then(Value::as_u64)
            .unwrap_or(443);
        let url = format!("wss://{host}:{port}/sub");
        let mut request = url.into_client_request()?;
        request
            .headers_mut()
            .insert(ORIGIN, "https://live.bilibili.com".parse()?);
        request.headers_mut().insert(
            REFERER,
            format!("https://live.bilibili.com/{room_id}").parse()?,
        );
        request
            .headers_mut()
            .insert(USER_AGENT, USER_AGENT_VALUE.parse()?);
        request
            .headers_mut()
            .insert(ACCEPT, "application/json,text/plain,*/*".parse()?);
        if let Some(cookie_header) = &cookie_header {
            request.headers_mut().insert(COOKIE, cookie_header.parse()?);
        }
        let (mut socket, _) = connect_async(request)
            .await
            .context("连接 B 站弹幕服务器失败")?;
        let auth = realtime_auth_payload(
            room_id.parse::<u64>()?,
            token,
            credential.as_ref(),
            identity.as_ref(),
        );
        socket
            .send(Message::Binary(
                encode_packet(7, serde_json::to_string(&auth)?.as_bytes()).into(),
            ))
            .await?;
        let heartbeat_period = Duration::from_secs(30);
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + heartbeat_period,
            heartbeat_period,
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut deduplicator = CrossOriginDeduplicator::default();

        loop {
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        let _ = socket.close(None).await;
                        return Ok(());
                    }
                }
                _ = heartbeat.tick() => socket.send(Message::Binary(encode_packet(2, &[]).into())).await?,
                message = socket.next() => {
                    let Some(message) = message else { bail!("B 站弹幕连接已关闭"); };
                    let message = message?;
                    match &message {
                        Message::Ping(value) => {
                            socket.send(Message::Pong(value.clone())).await?;
                            continue;
                        }
                        Message::Close(_) => bail!("B 站弹幕连接已关闭"),
                        _ => {}
                    }
                    let Some(bytes) = realtime_frame_payload(&message) else {
                        continue;
                    };
                    for packet in parse_packets(bytes)? {
                        match packet.operation {
                            8 => { let _ = tx.send(BilibiliClientEvent::Connected { room_id: room_id.to_string() }).await; }
                            3 if packet.body.len() >= 4 => {
                                let _ = tx.send(BilibiliClientEvent::Heartbeat).await;
                            }
                            5 => {
                                for raw in packet.body.split(|byte| *byte == 0).filter(|part| !part.is_empty()) {
                                    let Ok(value) = serde_json::from_slice::<Value>(raw) else { continue; };
                                    if value.get("p_is_ack").and_then(Value::as_bool) == Some(true) {
                                        let mut ack = BTreeMap::new();
                                        ack.insert("cmd", value.get("cmd").cloned().unwrap_or(Value::Null));
                                        ack.insert("msg_id", value.get("msg_id").cloned().unwrap_or(Value::Null));
                                        ack.insert("p_msg_type", value.get("p_msg_type").cloned().unwrap_or(json!(0)));
                                        socket.send(Message::Binary(encode_packet(24, serde_json::to_string(&ack)?.as_bytes()).into())).await?;
                                    }
                                    match classify_realtime_message(&value) {
                                        RealtimeMessage::Watched(count) => {
                                            if tx.send(BilibiliClientEvent::Watched(count)).await.is_err() {
                                                return Ok(());
                                            }
                                        }
                                        RealtimeMessage::Likes(count) => {
                                            if tx.send(BilibiliClientEvent::Likes(count)).await.is_err() {
                                                return Ok(());
                                            }
                                        }
                                        RealtimeMessage::Danmu(event) => {
                                            if deduplicator.should_emit(&event)
                                                && tx.send(BilibiliClientEvent::Danmu(event)).await.is_err()
                                            {
                                                return Ok(());
                                            }
                                        }
                                        RealtimeMessage::Unhandled(command) => {
                                            if observed_unhandled_commands.insert(command.clone())
                                                && tx.send(BilibiliClientEvent::UnhandledCommand { command }).await.is_err()
                                            {
                                                return Ok(());
                                            }
                                        }
                                        RealtimeMessage::Ignored => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

fn realtime_frame_payload(message: &Message) -> Option<&[u8]> {
    match message {
        Message::Binary(bytes) => Some(bytes.as_ref()),
        Message::Text(text) => Some(text.as_bytes()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountStatus {
    SignedOut,
    SignedIn {
        display_name: String,
        user_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginChallenge {
    pub key: String,
    pub url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginPoll {
    Waiting,
    Scanned,
    Expired,
    SignedIn(AccountStatus),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credential {
    cookie_header: String,
    csrf: String,
}

#[derive(Clone)]
pub struct AccountClient {
    http: reqwest::Client,
    session_path: PathBuf,
}

impl AccountClient {
    pub fn new(session_path: PathBuf) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT_VALUE)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            session_path,
        })
    }

    pub async fn login_challenge(&self) -> Result<LoginChallenge> {
        let value: Value = self
            .http
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&value, "创建 B 站登录二维码失败")?;
        Ok(LoginChallenge {
            key: value
                .pointer("/data/qrcode_key")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("B 站未返回二维码标识"))?
                .to_string(),
            url: Url::parse(
                value
                    .pointer("/data/url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("B 站未返回二维码地址"))?,
            )?,
        })
    }

    pub async fn poll_login(&self, key: &str) -> Result<LoginPoll> {
        let response = self
            .http
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
            .query(&[("qrcode_key", key)])
            .send()
            .await?
            .error_for_status()?;
        let cookies = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let value: Value = response.json().await?;
        ensure_code_zero(&value, "轮询 B 站登录状态失败")?;
        match value
            .pointer("/data/code")
            .and_then(Value::as_i64)
            .unwrap_or(-1)
        {
            86101 => Ok(LoginPoll::Waiting),
            86090 => Ok(LoginPoll::Scanned),
            86038 => Ok(LoginPoll::Expired),
            0 => {
                let credential = credential_from_cookies(&cookies)
                    .ok_or_else(|| anyhow!("B 站登录成功但未返回完整凭据"))?;
                self.save_credential(&credential)?;
                Ok(LoginPoll::SignedIn(self.status().await?))
            }
            code => bail!("B 站登录失败（{code}）"),
        }
    }

    pub async fn current_online_viewers(
        &self,
        room_id: &str,
        broadcaster_id: &str,
    ) -> Result<Option<u64>> {
        let Some(credential) = self.load_credential()? else {
            return Ok(None);
        };
        let mut last_error = None;
        for attempt in 0..2 {
            let result: Result<u64> = async {
                let value: Value = self
                    .http
                    .get("https://api.live.bilibili.com/xlive/general-interface/v1/rank/getOnlineRank")
                    .header(COOKIE, &credential.cookie_header)
                    .header(REFERER, format!("https://live.bilibili.com/{room_id}"))
                    .query(&[
                        ("page", "1"),
                        ("pageSize", "1"),
                        ("roomId", room_id),
                        ("ruid", broadcaster_id),
                        ("platform", "pc_link"),
                    ])
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                online_viewer_count_from_response(&value)
            }
            .await;
            match result {
                Ok(viewers) => return Ok(Some(viewers)),
                Err(error) => last_error = Some(error),
            }
            if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        Err(last_error.expect("在线人数请求至少执行一次"))
    }

    pub async fn current_likes(&self, room_id: &str) -> Result<Option<u64>> {
        let Some(credential) = self.load_credential()? else {
            return Ok(None);
        };
        let mut last_error = None;
        for attempt in 0..2 {
            let result: Result<u64> = async {
                let value: Value = self
                    .http
                    .get("https://api.live.bilibili.com/xlive/web-room/v1/index/getInfoByRoom")
                    .header(COOKIE, &credential.cookie_header)
                    .header(REFERER, format!("https://live.bilibili.com/{room_id}"))
                    .query(&[("room_id", room_id)])
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                ensure_code_zero(&value, "读取直播间点赞数失败")?;
                like_count(&value).ok_or_else(|| anyhow!("B 站未返回有效点赞数"))
            }
            .await;
            match result {
                Ok(likes) => return Ok(Some(likes)),
                Err(error) => last_error = Some(error),
            }
            if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        Err(last_error.expect("点赞数请求至少执行一次"))
    }
    pub async fn status(&self) -> Result<AccountStatus> {
        let Some(credential) = self.load_credential()? else {
            return Ok(AccountStatus::SignedOut);
        };
        let value: Value = self
            .http
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header(COOKIE, &credential.cookie_header)
            .header(REFERER, "https://www.bilibili.com")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if value.get("code").and_then(Value::as_i64) != Some(0)
            || value.pointer("/data/isLogin").and_then(Value::as_bool) != Some(true)
        {
            return Ok(AccountStatus::SignedOut);
        }
        let display_name = value
            .pointer("/data/uname")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("未获取到 B 站昵称"))?
            .to_string();
        let user_id = string_value(value.pointer("/data/mid"))
            .ok_or_else(|| anyhow!("未获取到 B 站账号标识"))?;
        Ok(AccountStatus::SignedIn {
            display_name,
            user_id,
        })
    }

    pub fn sign_out(&self) -> Result<()> {
        match std::fs::remove_file(&self.session_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn send_danmu(
        &self,
        message: &str,
        room_id: &str,
        reply_to: Option<&str>,
    ) -> Result<()> {
        let credential = self
            .load_credential()?
            .ok_or_else(|| anyhow!("还没有 B 站登录态"))?;
        let message = message.trim();
        let length = unicode_segmentation::UnicodeSegmentation::graphemes(message, true).count();
        if message.is_empty() {
            bail!("弹幕内容不能为空");
        }
        if length > super::ACCOUNT_MESSAGE_LIMIT {
            bail!("弹幕不能超过 {} 个字", super::ACCOUNT_MESSAGE_LIMIT);
        }
        let now = Utc::now().timestamp().to_string();
        let form = [
            ("bubble", "0"),
            ("msg", message),
            ("color", "16777215"),
            ("mode", "1"),
            ("room_type", "0"),
            ("jumpfrom", "0"),
            ("reply_mid", reply_to.unwrap_or("0")),
            ("reply_attr", "0"),
            ("reply_uname", ""),
            ("replay_dmid", ""),
            ("statistics", "{\"appId\":100,\"platform\":5}"),
            ("reply_type", "0"),
            ("fontsize", "25"),
            ("rnd", &now),
            ("roomid", room_id),
            ("csrf", &credential.csrf),
            ("csrf_token", &credential.csrf),
        ];
        let value: Value = self
            .http
            .post("https://api.live.bilibili.com/msg/send")
            .header(ORIGIN, "https://live.bilibili.com")
            .header(REFERER, format!("https://live.bilibili.com/{room_id}"))
            .header(COOKIE, &credential.cookie_header)
            .form(&form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_danmu_accepted(&value, message)
    }

    fn load_credential(&self) -> Result<Option<Credential>> {
        load_credential(&self.session_path)
    }

    fn save_credential(&self, credential: &Credential) -> Result<()> {
        if let Some(parent) = self.session_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(credential)?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&self.session_path)?;
            file.write_all(&data)?;
        }
        #[cfg(not(unix))]
        std::fs::write(&self.session_path, data)?;
        Ok(())
    }
}

fn load_credential(path: &PathBuf) -> Result<Option<Credential>> {
    match std::fs::read(path) {
        Ok(data) => Ok(Some(
            serde_json::from_slice(&data).context("读取 B 站登录态失败")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

fn realtime_cookie_header(
    credential: Option<&Credential>,
    identity: Option<&DeviceIdentity>,
) -> Option<String> {
    let mut header = credential
        .map(|value| value.cookie_header.clone())
        .unwrap_or_default();
    if let Some(identity) = identity {
        if !header.is_empty() {
            header.push_str("; ");
        }
        header.push_str(&identity.cookie_header());
    }
    (!header.is_empty()).then_some(header)
}

fn device_identity_from_response(value: &Value) -> Option<DeviceIdentity> {
    if value.get("code").and_then(Value::as_i64) != Some(0) {
        return None;
    }
    let buvid3 = value.pointer("/data/b_3")?.as_str()?.trim();
    let buvid4 = value.pointer("/data/b_4")?.as_str()?.trim();
    if buvid3.is_empty() || buvid4.is_empty() {
        return None;
    }
    Some(DeviceIdentity {
        buvid3: buvid3.to_owned(),
        buvid4: buvid4.to_owned(),
    })
}

fn realtime_auth_payload(
    room_id: u64,
    token: &str,
    credential: Option<&Credential>,
    identity: Option<&DeviceIdentity>,
) -> Value {
    let uid = credential
        .and_then(|value| cookie_value(&value.cookie_header, "DedeUserID"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let buvid = credential
        .and_then(|value| cookie_value(&value.cookie_header, "buvid3"))
        .or_else(|| identity.map(|value| value.buvid3.as_str()))
        .unwrap_or_default();
    json!({
        "uid": uid,
        "roomid": room_id,
        "protover": 3,
        "buvid": buvid,
        "support_ack": true,
        "queue_uuid": Uuid::new_v4().simple().to_string()[..8].to_string(),
        "scene": "",
        "platform": "web",
        "type": 2,
        "key": token,
    })
}

fn credential_from_cookies(headers: &[String]) -> Option<Credential> {
    let mut pairs = Vec::new();
    let mut csrf = None;
    for header in headers {
        let pair = header.split(';').next()?.trim();
        let (name, value) = pair.split_once('=')?;
        if [
            "SESSDATA",
            "bili_jct",
            "DedeUserID",
            "DedeUserID__ckMd5",
            "sid",
            "buvid3",
            "buvid4",
        ]
        .contains(&name)
        {
            if name == "bili_jct" {
                csrf = Some(value.to_string());
            }
            pairs.push(format!("{name}={value}"));
        }
    }
    let csrf = csrf?;
    if !pairs.iter().any(|pair| pair.starts_with("SESSDATA=")) {
        return None;
    }
    Some(Credential {
        cookie_header: pairs.join("; "),
        csrf,
    })
}

#[cfg(test)]
fn room_snapshot_from_response(value: &Value, requested_room_id: &str) -> Result<RoomSnapshot> {
    room_snapshot_update_from_response(value, requested_room_id, None)
}

fn room_snapshot_update_from_response(
    value: &Value,
    requested_room_id: &str,
    previous: Option<&RoomSnapshot>,
) -> Result<RoomSnapshot> {
    ensure_code_zero(value, "读取直播间信息失败")?;
    let data: RoomBaseData = serde_json::from_value(
        value
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow!("B 站未返回直播间信息"))?,
    )
    .context("B 站直播间字段契约已变化")?;
    let room = if let Some(room) = data.by_room_ids.get(requested_room_id) {
        room
    } else if data.by_room_ids.len() == 1 {
        data.by_room_ids.values().next().unwrap()
    } else {
        bail!("B 站未返回请求的直播间：{requested_room_id}");
    };
    let required_text = |current: Option<&String>, previous: Option<&String>, label: &str| {
        current
            .filter(|value| !value.trim().is_empty())
            .or(previous)
            .cloned()
            .ok_or_else(|| anyhow!("B 站未返回{label}"))
    };
    let live_status = room
        .live_status
        .map(RoomLiveStatus::try_from)
        .transpose()?
        .or_else(|| previous.map(|snapshot| snapshot.live_status))
        .ok_or_else(|| anyhow!("B 站未返回直播状态"))?;
    let live_started_at = if live_status != RoomLiveStatus::Live {
        None
    } else if let Some(live_time) = room.live_time.as_deref().filter(|value| !value.is_empty()) {
        match parse_live_started_at(live_time, live_status) {
            Ok(value) => value,
            Err(_error) if previous.is_some() => {
                previous.and_then(|snapshot| snapshot.live_started_at)
            }
            Err(error) => return Err(error),
        }
    } else {
        previous.and_then(|snapshot| snapshot.live_started_at)
    };
    let partial_area = [
        room.parent_area_name.as_deref().unwrap_or_default(),
        room.area_name.as_deref().unwrap_or_default(),
    ]
    .into_iter()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" / ");
    Ok(RoomSnapshot {
        room_id: room
            .room_id
            .map(|value| value.to_string())
            .or_else(|| previous.map(|snapshot| snapshot.room_id.clone()))
            .unwrap_or_else(|| requested_room_id.to_string()),
        broadcaster_id: room
            .uid
            .map(|value| value.to_string())
            .or_else(|| previous.map(|snapshot| snapshot.broadcaster_id.clone()))
            .ok_or_else(|| anyhow!("B 站未返回主播标识"))?,
        broadcaster_name: required_text(
            room.uname.as_ref(),
            previous.map(|snapshot| &snapshot.broadcaster_name),
            "主播名称",
        )?,
        title: required_text(
            room.title.as_ref(),
            previous.map(|snapshot| &snapshot.title),
            "直播间标题",
        )?,
        area: if partial_area.is_empty() {
            previous
                .map(|snapshot| snapshot.area.clone())
                .unwrap_or_default()
        } else {
            partial_area
        },
        live_started_at,
        live_status,
    })
}

fn parse_live_started_at(value: &str, status: RoomLiveStatus) -> Result<Option<DateTime<Utc>>> {
    if status != RoomLiveStatus::Live {
        return Ok(None);
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .context("B 站返回了无效开播时间")?;
    let china = FixedOffset::east_opt(8 * 3600).unwrap();
    let started_at = china
        .from_local_datetime(&naive)
        .single()
        .context("B 站返回了不唯一的开播时间")?;
    Ok(Some(started_at.with_timezone(&Utc)))
}

fn online_viewer_count_from_response(value: &Value) -> Result<u64> {
    ensure_code_zero(value, "读取直播间在线人数失败")?;
    value
        .pointer("/data/onlineNum")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("B 站未返回有效在线人数"))
}
fn ensure_danmu_accepted(value: &Value, requested_message: &str) -> Result<()> {
    ensure_code_zero(value, "发送弹幕失败")?;
    let extra = value
        .pointer("/data/mode_info/extra")
        .ok_or_else(|| anyhow!("B 站未返回弹幕发送凭据，消息可能被风控拦截"))?;
    let proof = match extra {
        Value::String(value) => serde_json::from_str::<Value>(value)
            .map_err(|error| anyhow!("B 站弹幕发送凭据格式无效：{error}"))?,
        Value::Object(_) => extra.clone(),
        _ => bail!("B 站未返回弹幕发送凭据，消息可能被风控拦截"),
    };
    if proof.get("send_from_me").and_then(Value::as_bool) != Some(true) {
        bail!("B 站未确认该弹幕由当前账号发出");
    }
    let accepted_content = proof
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("B 站弹幕发送凭据缺少消息内容"))?;
    if accepted_content != requested_message {
        bail!("B 站返回的弹幕内容与请求不一致");
    }
    Ok(())
}

fn ensure_code_zero(value: &Value, fallback: &str) -> Result<()> {
    if value.get("code").and_then(Value::as_i64) == Some(0) {
        return Ok(());
    }
    let message = value
        .get("message")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    bail!("{message}")
}

fn watched_count(value: &Value) -> Option<u64> {
    let command = value.get("cmd")?.as_str()?.split(':').next()?;
    (command == "WATCHED_CHANGE").then(|| value.pointer("/data/num")?.as_u64())?
}

fn like_count(value: &Value) -> Option<u64> {
    match command_name(value) {
        Some("LIKE_INFO_V3_UPDATE") => value.pointer("/data/click_count")?.as_u64(),
        Some(_) => None,
        None => value.pointer("/data/like_info_v3/total_likes")?.as_u64(),
    }
}

enum RealtimeMessage {
    Watched(u64),
    Likes(u64),
    Danmu(DanmuEvent),
    Unhandled(String),
    Ignored,
}

fn classify_realtime_message(value: &Value) -> RealtimeMessage {
    if let Some(count) = watched_count(value) {
        return RealtimeMessage::Watched(count);
    }
    if let Some(count) = like_count(value) {
        return RealtimeMessage::Likes(count);
    }
    if let Some(event) = parse_command(value) {
        return RealtimeMessage::Danmu(event);
    }
    match command_name(value) {
        Some("NOTICE_MSG") | None => RealtimeMessage::Ignored,
        Some(command) => RealtimeMessage::Unhandled(command.to_owned()),
    }
}

fn string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treats_tls_unexpected_eof_as_a_recoverable_disconnect() {
        let error = anyhow::Error::from(tokio_tungstenite::tungstenite::Error::Io(
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "peer closed connection without sending TLS close_notify",
            ),
        ));

        assert!(realtime_connection_error_event(error).is_none());
    }

    #[test]
    fn preserves_non_recoverable_realtime_errors() {
        let event = realtime_connection_error_event(anyhow!("B 站未返回弹幕令牌"));

        assert_eq!(
            event,
            Some(BilibiliClientEvent::Error("B 站未返回弹幕令牌".into()))
        );
    }

    #[test]
    fn accepts_binary_and_text_websocket_payloads() {
        let binary = Message::Binary(vec![1, 2, 3].into());
        let text = Message::Text("packet".into());

        assert_eq!(realtime_frame_payload(&binary), Some([1, 2, 3].as_slice()));
        assert_eq!(realtime_frame_payload(&text), Some(b"packet".as_slice()));
        assert!(realtime_frame_payload(&Message::Pong(Vec::new().into())).is_none());
    }

    #[test]
    fn classifies_unknown_commands_by_name_without_retaining_payload() {
        let value = json!({
            "cmd": "ROOM_CHANGE",
            "data": { "title": "不应进入诊断记录" }
        });
        match classify_realtime_message(&value) {
            RealtimeMessage::Unhandled(command) => assert_eq!(command, "ROOM_CHANGE"),
            _ => panic!("未知命令应进入安全观测链"),
        }
        assert!(matches!(
            classify_realtime_message(&json!({"cmd":"NOTICE_MSG","data":{}})),
            RealtimeMessage::Ignored
        ));
    }

    #[test]
    fn extracts_compatible_login_credential() {
        let credential = credential_from_cookies(&[
            "SESSDATA=secret; Path=/".into(),
            "bili_jct=csrf; Path=/".into(),
            "DedeUserID=42; Path=/".into(),
        ])
        .unwrap();
        assert_eq!(
            credential.cookie_header,
            "SESSDATA=secret; bili_jct=csrf; DedeUserID=42"
        );
        assert_eq!(credential.csrf, "csrf");
    }

    #[test]
    fn parses_device_identity_for_realtime_handshake() {
        let identity = device_identity_from_response(&json!({
            "code": 0,
            "data": { "b_3": "BUVID3", "b_4": "BUVID4" }
        }))
        .unwrap();
        assert_eq!(identity.buvid3, "BUVID3");
        assert_eq!(identity.cookie_header(), "buvid3=BUVID3; buvid4=BUVID4");
    }

    #[test]
    fn realtime_auth_payload_includes_account_and_device_identity() {
        let identity = DeviceIdentity {
            buvid3: "BUVID3".into(),
            buvid4: "BUVID4".into(),
        };
        let credential = Credential {
            cookie_header: "SESSDATA=secret; bili_jct=csrf; DedeUserID=42".into(),
            csrf: "csrf".into(),
        };
        let cookie_header = realtime_cookie_header(Some(&credential), Some(&identity)).unwrap();
        let payload = realtime_auth_payload(392612, "token", Some(&credential), Some(&identity));
        assert_eq!(
            cookie_header,
            "SESSDATA=secret; bili_jct=csrf; DedeUserID=42; buvid3=BUVID3; buvid4=BUVID4"
        );
        assert_eq!(payload["uid"], 42);
        assert_eq!(payload["roomid"], 392612);
        assert_eq!(payload["protover"], 3);
        assert_eq!(payload["buvid"], "BUVID3");
        assert_eq!(payload["support_ack"], true);
    }
    #[test]
    fn parses_realtime_watched_count_without_treating_popularity_as_viewers() {
        assert_eq!(
            watched_count(&json!({
                "cmd": "WATCHED_CHANGE",
                "data": {
                    "num": 17_903,
                    "text_large": "1.7万人看过"
                }
            })),
            Some(17_903)
        );
        assert_eq!(
            watched_count(&json!({
                "cmd": "ONLINE_RANK_COUNT",
                "data": { "count": 99 }
            })),
            None
        );
    }

    #[test]
    fn parses_realtime_like_total() {
        assert_eq!(
            like_count(&json!({
                "cmd": "LIKE_INFO_V3_UPDATE",
                "data": { "click_count": 8_621 }
            })),
            Some(8_621)
        );
        assert_eq!(
            like_count(&json!({
                "cmd": "LIKE_INFO_V3_CLICK",
                "data": { "uname": "甲" }
            })),
            None
        );
    }

    #[test]
    fn parses_initial_like_total_from_authenticated_room_info() {
        assert_eq!(
            like_count(&json!({
                "code": 0,
                "data": { "like_info_v3": { "total_likes": 221 } }
            })),
            Some(221)
        );
    }
    #[test]
    fn parses_authenticated_current_online_viewers() {
        let count = online_viewer_count_from_response(&json!({
            "code": 0,
            "data": { "onlineNum": 11, "item": [] }
        }))
        .unwrap();
        assert_eq!(count, 11);
    }

    #[test]
    fn parses_live_started_at_in_china_standard_time() {
        let started_at =
            parse_live_started_at("2026-09-01 20:00:00", RoomLiveStatus::Live).unwrap();
        assert_eq!(
            started_at.unwrap().to_rfc3339(),
            "2026-09-01T12:00:00+00:00"
        );
        assert_eq!(
            parse_live_started_at("0000-00-00 00:00:00", RoomLiveStatus::Offline).unwrap(),
            None
        );
    }

    #[test]
    fn room_snapshot_ignores_ambiguous_online_value() {
        let snapshot = room_snapshot_from_response(
            &json!({
                "code": 0,
                "data": {
                    "by_room_ids": {
                        "23058": {
                            "room_id": 23058,
                            "uid": 11153765,
                            "uname": "3号直播间",
                            "title": "哔哩哔哩音悦台",
                            "parent_area_name": "电台",
                            "area_name": "唱见电台",
                            "attention": 248859,
                            "online": 6697,
                            "live_status": 2,
                            "live_time": "2026-08-01 20:00:00"
                        }
                    }
                }
            }),
            "23058",
        )
        .unwrap();

        assert_eq!(snapshot.room_id, "23058");
        assert_eq!(snapshot.broadcaster_id, "11153765");
        assert_eq!(snapshot.broadcaster_name, "3号直播间");
        assert_eq!(snapshot.area, "电台 / 唱见电台");
        assert_eq!(snapshot.live_status, RoomLiveStatus::Rotating);
        assert_eq!(snapshot.live_started_at, None);
        assert!(!snapshot.is_live());
    }

    #[test]
    fn room_snapshot_updates_available_fields_and_preserves_missing_fields() {
        let previous = RoomSnapshot {
            room_id: "1".into(),
            broadcaster_id: "2".into(),
            broadcaster_name: "主播".into(),
            title: "旧标题".into(),
            area: "知识 / 社科".into(),
            live_started_at: Some(
                DateTime::parse_from_rfc3339("2026-09-01T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            live_status: RoomLiveStatus::Live,
        };
        let updated = room_snapshot_update_from_response(
            &json!({
                "code": 0,
                "data": {
                    "by_room_ids": {
                        "1": { "title": "新标题" }
                    }
                }
            }),
            "1",
            Some(&previous),
        )
        .unwrap();

        assert_eq!(updated.title, "新标题");
        assert_eq!(updated.broadcaster_name, previous.broadcaster_name);
        assert_eq!(updated.area, previous.area);
        assert_eq!(updated.live_status, previous.live_status);
        assert_eq!(updated.live_started_at, previous.live_started_at);
    }

    #[test]
    fn room_snapshot_rejects_unknown_live_status() {
        let error = room_snapshot_from_response(
            &json!({
                "code": 0,
                "data": {
                    "by_room_ids": {
                        "1": {
                            "room_id": 1,
                            "uid": 2,
                            "uname": "主播",
                            "title": "标题",
                            "parent_area_name": "知识",
                            "area_name": "社科",
                            "online": 3,
                            "live_time": "0000-00-00 00:00:00",
                            "live_status": 9
                        }
                    }
                }
            }),
            "1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("未知直播状态"));
    }
    #[test]
    fn danmu_send_requires_server_delivery_proof() {
        let error = ensure_danmu_accepted(
            &json!({"code": 0, "message": "0", "data": null}),
            "测试弹幕",
        )
        .unwrap_err();

        assert!(error.to_string().contains("未返回弹幕发送凭据"));
    }

    #[test]
    fn danmu_send_accepts_matching_server_delivery_proof() {
        let response = json!({
            "code": 0,
            "message": "0",
            "data": {
                "mode_info": {
                    "extra": serde_json::to_string(&json!({
                        "content": "测试弹幕",
                        "send_from_me": true,
                        "is_audited": false,
                        "id_str": "123"
                    }))
                    .unwrap()
                }
            }
        });

        ensure_danmu_accepted(&response, "测试弹幕").unwrap();
    }

    #[test]
    fn danmu_send_rejects_mismatched_delivery_proof() {
        let response = json!({
            "code": 0,
            "data": {
                "mode_info": {
                    "extra": "{\"content\":\"另一条弹幕\",\"send_from_me\":true}"
                }
            }
        });

        let error = ensure_danmu_accepted(&response, "测试弹幕").unwrap_err();
        assert!(error.to_string().contains("内容与请求不一致"));
    }
}
