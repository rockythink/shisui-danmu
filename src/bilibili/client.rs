use super::{
    CrossOriginDeduplicator, RECONNECT_DELAYS_SECONDS, encode_packet, parse_command, parse_packets,
    parser::{command_name, parse_gift_v2},
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
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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
const PUBLIC_PROFILE_ENDPOINT: &str = "https://api.bilibili.com/x/web-interface/card";
const PUBLIC_PROFILE_TTL: Duration = Duration::from_secs(15 * 60);
const PUBLIC_PROFILE_ERROR_TTL: Duration = Duration::from_secs(60);
const PUBLIC_PROFILE_CACHE_LIMIT: usize = 64;
const PUBLIC_PROFILE_RESPONSE_LIMIT: usize = 64 * 1024;
const PUBLIC_PROFILE_DESCRIPTION_LIMIT: usize = 512;
const ROOM_TITLE_LIMIT: usize = 40;
const ROOM_COVER_LIMIT_BYTES: u64 = 2 * 1024 * 1024;

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
        if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
            return error.is_connect() || error.is_timeout();
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicProfile {
    pub user_id: String,
    pub description: String,
    pub source_url: String,
}

fn public_profile_user_id(value: &str) -> Result<u64> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("无效的主播 UID");
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow!("无效的主播 UID"))
}

fn public_profile_from_response(
    value: &Value,
    requested_user_id: u64,
) -> Result<Option<PublicProfile>> {
    let code = value
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("主页简介响应缺少状态码"))?;
    if code != 0 {
        bail!("主页简介暂不可用（B 站错误码 {code}）");
    }
    let card = value
        .pointer("/data/card")
        .ok_or_else(|| anyhow!("主页简介响应缺少用户资料"))?;
    let returned_user_id = match card.get("mid") {
        Some(Value::String(id)) => public_profile_user_id(id)?,
        Some(id) => id
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| anyhow!("主页简介来源 UID 无效"))?,
        None => bail!("主页简介响应缺少来源 UID"),
    };
    if returned_user_id != requested_user_id {
        bail!("主页简介来源与主播 UID 不一致");
    }
    let sign = card
        .get("sign")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("主页简介响应缺少签名"))?;
    let mut description: String = sign
        .chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .skip_while(|ch| ch.is_whitespace())
        .take(PUBLIC_PROFILE_DESCRIPTION_LIMIT)
        .collect();
    description.truncate(description.trim_end().len());
    if description.is_empty() {
        return Ok(None);
    }
    Ok(Some(PublicProfile {
        user_id: requested_user_id.to_string(),
        description,
        source_url: format!("https://space.bilibili.com/{requested_user_id}"),
    }))
}

struct CachedPublicProfile {
    expires_at: Instant,
    result: std::result::Result<Option<PublicProfile>, String>,
}

#[derive(Clone)]
pub struct BilibiliClient {
    http: reqwest::Client,
    public_http: reqwest::Client,
    public_profiles: Arc<Mutex<BTreeMap<u64, CachedPublicProfile>>>,
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
        // Public requests must never inherit the live client's cookie jar.
        let public_http = reqwest::Client::builder()
            .user_agent(USER_AGENT_VALUE)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8))
            .build()?;
        Ok(Self {
            http,
            public_http,
            public_profiles: Arc::new(Mutex::new(BTreeMap::new())),
            device_identity: Arc::new(OnceCell::new()),
            session_path,
        })
    }

    pub async fn broadcaster_profile(&self, broadcaster_id: &str) -> Result<Option<PublicProfile>> {
        self.broadcaster_profile_at(broadcaster_id, PUBLIC_PROFILE_ENDPOINT, Instant::now())
            .await
    }

    async fn broadcaster_profile_at(
        &self,
        broadcaster_id: &str,
        endpoint: &str,
        now: Instant,
    ) -> Result<Option<PublicProfile>> {
        let user_id = public_profile_user_id(broadcaster_id)?;
        {
            let mut cache = self
                .public_profiles
                .lock()
                .map_err(|_| anyhow!("主页简介缓存不可用"))?;
            cache.retain(|_, entry| entry.expires_at > now);
            if let Some(entry) = cache.get(&user_id) {
                return entry.result.clone().map_err(anyhow::Error::msg);
            }
        }

        let result = self.fetch_public_profile(user_id, endpoint).await;
        let ttl = if result.is_ok() {
            PUBLIC_PROFILE_TTL
        } else {
            PUBLIC_PROFILE_ERROR_TTL
        };
        let mut cache = self
            .public_profiles
            .lock()
            .map_err(|_| anyhow!("主页简介缓存不可用"))?;
        if cache.len() >= PUBLIC_PROFILE_CACHE_LIMIT
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(id, _)| *id)
        {
            cache.remove(&oldest);
        }
        cache.insert(
            user_id,
            CachedPublicProfile {
                expires_at: now + ttl,
                result: result
                    .as_ref()
                    .map(Clone::clone)
                    .map_err(|error| error.to_string()),
            },
        );
        result
    }

    async fn fetch_public_profile(
        &self,
        user_id: u64,
        endpoint: &str,
    ) -> Result<Option<PublicProfile>> {
        // The public card's sign is the homepage signature; description is unrelated.
        // https://github.com/pskdje/bilibili-API-collect/blob/main/docs/user/info.md#用户名片信息
        let mut response = self
            .public_http
            .get(endpoint)
            .query(&[("mid", user_id)])
            .header(ACCEPT, "application/json")
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > PUBLIC_PROFILE_RESPONSE_LIMIT as u64)
        {
            bail!("主页简介响应过大");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > PUBLIC_PROFILE_RESPONSE_LIMIT - body.len() {
                bail!("主页简介响应过大");
            }
            body.extend_from_slice(&chunk);
        }
        public_profile_from_response(&serde_json::from_slice(&body)?, user_id)
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
                    reply_to: None,
                    origin: DanmuEventOrigin::History,
                    platform_event_id: platform_id,
                    emotes: Vec::new(),
                    gift: None,
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
                                        RealtimeMessage::DanmuBatch(events) => {
                                            for event in events {
                                                if deduplicator.should_emit(&event)
                                                    && tx.send(BilibiliClientEvent::Danmu(event)).await.is_err()
                                                {
                                                    return Ok(());
                                                }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountStatus {
    SignedOut,
    SignedIn {
        display_name: String,
        user_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomUpdateReceipt {
    Applied {
        canonical_room_id: String,
    },
    Pending {
        canonical_room_id: String,
        reason: Option<String>,
    },
    Rejected {
        canonical_room_id: String,
        reason: String,
    },
    Unknown {
        canonical_room_id: String,
        status: Option<i64>,
        reason: Option<String>,
    },
}

impl std::fmt::Display for RoomUpdateReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Applied { .. } => formatter.write_str("已更新"),
            Self::Pending { reason, .. } => match reason.as_deref() {
                Some(reason) => write!(formatter, "已提交审核：{reason}"),
                None => formatter.write_str("已提交审核"),
            },
            Self::Rejected { reason, .. } => write!(formatter, "审核未通过：{reason}"),
            Self::Unknown { status, reason, .. } => {
                formatter.write_str("请求已受理，状态未知（")?;
                match status {
                    Some(status) => write!(formatter, "{status}"),
                    None => formatter.write_str("未知"),
                }?;
                formatter.write_str("）")?;
                if let Some(reason) = reason {
                    write!(formatter, "：{reason}")?;
                }
                Ok(())
            }
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<AccountStatus>,
}

#[derive(Clone)]
enum AccountStorage {
    File(PathBuf),
    Memory(Arc<Mutex<Option<Credential>>>),
}

#[derive(Clone)]
pub struct AccountClient {
    http: reqwest::Client,
    storage: AccountStorage,
    #[cfg(test)]
    test_endpoint: Option<Url>,
}
impl std::fmt::Debug for AccountClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountClient").finish_non_exhaustive()
    }
}

impl AccountClient {
    pub fn new(session_path: PathBuf) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT_VALUE)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            storage: AccountStorage::File(session_path),
            #[cfg(test)]
            test_endpoint: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_endpoint(mut self, endpoint: Url) -> Self {
        self.test_endpoint = Some(endpoint);
        self
    }

    fn endpoint(&self, endpoint: &'static str) -> std::borrow::Cow<'static, str> {
        #[cfg(test)]
        if let Some(base) = &self.test_endpoint {
            let original = Url::parse(endpoint).expect("static account endpoint");
            return base
                .join(original.path())
                .expect("test endpoint path")
                .to_string()
                .into();
        }
        endpoint.into()
    }

    pub(crate) fn session_path(&self) -> Option<&std::path::Path> {
        match &self.storage {
            AccountStorage::File(path) => Some(path),
            AccountStorage::Memory(_) => None,
        }
    }

    /// QR credentials remain in memory until the human confirms the displayed identity.
    pub(crate) fn staged(&self) -> Self {
        Self {
            http: self.http.clone(),
            storage: AccountStorage::Memory(Arc::default()),
            #[cfg(test)]
            test_endpoint: self.test_endpoint.clone(),
        }
    }

    /// Pin before verification so replacing a session file cannot change the POST identity.
    pub(crate) fn snapshot(&self) -> Result<Self> {
        Ok(Self {
            http: self.http.clone(),
            storage: AccountStorage::Memory(Arc::new(Mutex::new(self.load_credential()?))),
            #[cfg(test)]
            test_endpoint: self.test_endpoint.clone(),
        })
    }

    pub(crate) fn persist_to(&self, path: PathBuf) -> Result<Self> {
        let credential = self.load_credential()?.context("没有可保存的登录态")?;
        let account = Self {
            http: self.http.clone(),
            storage: AccountStorage::File(path),
            #[cfg(test)]
            test_endpoint: self.test_endpoint.clone(),
        };
        account.save_credential(&credential)?;
        Ok(account)
    }

    pub(crate) fn cached_status(&self) -> Result<AccountStatus> {
        let Some(credential) = self.load_credential()? else {
            return Ok(AccountStatus::SignedOut);
        };
        Ok(credential.identity.unwrap_or_else(|| {
            match cookie_value(&credential.cookie_header, "DedeUserID") {
                Some(user_id) => AccountStatus::SignedIn {
                    display_name: "已保存账号（待验证）".into(),
                    user_id: user_id.into(),
                },
                None => AccountStatus::SignedOut,
            }
        }))
    }

    pub async fn login_challenge(&self) -> Result<LoginChallenge> {
        let value: Value = self
            .http
            .get(
                self.endpoint("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
                    .as_ref(),
            )
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
            .get(
                self.endpoint("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
                    .as_ref(),
            )
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
                let mut credential = credential_from_cookies(&cookies)
                    .ok_or_else(|| anyhow!("B 站登录成功但未返回完整凭据"))?;
                let status = self.status_with_credential(&credential).await?;
                if !matches!(status, AccountStatus::SignedIn { .. }) {
                    bail!("登录态验证失败；原账号未更改");
                }
                credential.identity = Some(status.clone());
                self.save_credential(&credential)?;
                Ok(LoginPoll::SignedIn(status))
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
        self.status_with_credential(&credential).await
    }

    async fn status_with_credential(&self, credential: &Credential) -> Result<AccountStatus> {
        let value: Value = self
            .http
            .get(
                self.endpoint("https://api.bilibili.com/x/web-interface/nav")
                    .as_ref(),
            )
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
        let AccountStorage::File(path) = &self.storage else {
            if let AccountStorage::Memory(credential) = &self.storage {
                *credential.lock().map_err(|_| anyhow!("登录态锁不可用"))? = None;
            }
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn update_room_title(&self, room_id: &str, title: &str) -> Result<RoomUpdateReceipt> {
        let title = title.trim();
        let length = unicode_segmentation::UnicodeSegmentation::graphemes(title, true).count();
        if title.is_empty() {
            bail!("直播间标题不能为空");
        }
        if length > ROOM_TITLE_LIMIT {
            bail!("直播间标题不能超过 {ROOM_TITLE_LIMIT} 个字");
        }

        let pinned = self.snapshot()?;
        let canonical_room_id = pinned.verify_room_owner(room_id).await?;
        let credential = pinned
            .load_credential()?
            .ok_or_else(|| anyhow!("还没有 B 站登录态"))?;
        let value: Value = pinned
            .http
            .post(
                pinned
                    .endpoint("https://api.live.bilibili.com/room/v1/Room/update")
                    .as_ref(),
            )
            .header(ORIGIN, "https://live.bilibili.com")
            .header(
                REFERER,
                format!("https://live.bilibili.com/{canonical_room_id}"),
            )
            .header(COOKIE, &credential.cookie_header)
            .form(&[
                ("room_id", canonical_room_id.as_str()),
                ("title", title),
                ("platform", "web"),
                ("csrf", credential.csrf.as_str()),
                ("csrf_token", credential.csrf.as_str()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&value, "更新直播间标题失败")?;
        room_update_receipt(&value, canonical_room_id)
    }

    pub async fn update_room_cover(&self, room_id: &str, path: &Path) -> Result<RoomUpdateReceipt> {
        use std::io::Read as _;

        let mut file = std::fs::File::open(path)
            .with_context(|| format!("无法读取封面：{}", path.display()))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            bail!("封面路径不是文件");
        }
        if metadata.len() == 0 {
            bail!("封面文件为空");
        }
        if metadata.len() > ROOM_COVER_LIMIT_BYTES {
            bail!("封面不能超过 2 MiB");
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.by_ref()
            .take(ROOM_COVER_LIMIT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > ROOM_COVER_LIMIT_BYTES {
            bail!("封面不能超过 2 MiB");
        }
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            "image/jpeg"
        } else {
            bail!("封面只支持 PNG 或 JPEG 图片");
        };

        let pinned = self.snapshot()?;
        let canonical_room_id = pinned.verify_room_owner(room_id).await?;
        let credential = pinned
            .load_credential()?
            .ok_or_else(|| anyhow!("还没有 B 站登录态"))?;
        let file_name =
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(if mime == "image/png" {
                    "cover.png"
                } else {
                    "cover.jpg"
                });
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name.to_owned())
            .mime_str(mime)?;
        let upload: Value = pinned
            .http
            .post(
                pinned
                    .endpoint("https://api.bilibili.com/x/upload/web/image")
                    .as_ref(),
            )
            .header(ORIGIN, "https://live.bilibili.com")
            .header(
                REFERER,
                format!("https://live.bilibili.com/{canonical_room_id}"),
            )
            .header(COOKIE, &credential.cookie_header)
            .query(&[("csrf", credential.csrf.as_str())])
            .multipart(
                reqwest::multipart::Form::new()
                    .text("bucket", "live")
                    .text("dir", "new_room_cover")
                    .part("file", part),
            )
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&upload, "上传直播间封面失败")?;
        let location = upload
            .pointer("/data/location")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("B 站未返回封面地址"))?;
        let location_url = Url::parse(location).context("B 站返回了无效封面地址")?;
        let trusted_host = location_url
            .host_str()
            .is_some_and(|host| host == "hdslb.com" || host.ends_with(".hdslb.com"));
        if location_url.scheme() != "https" || !trusted_host {
            bail!("B 站返回了不受信任的封面地址");
        }

        let value: Value = pinned
            .http
            .post(
                pinned
                    .endpoint("https://api.live.bilibili.com/xlive/app-blink/v1/preLive/UpdatePreLiveInfo")
                    .as_ref(),
            )
            .header(ORIGIN, "https://live.bilibili.com")
            .header(REFERER, format!("https://live.bilibili.com/{canonical_room_id}"))
            .header(COOKIE, &credential.cookie_header)
            .form(&[
                ("platform", "web"),
                ("mobi_app", "web"),
                ("build", "1"),
                ("room_id", canonical_room_id.as_str()),
                ("cover", location),
                ("coverVertical", ""),
                ("liveDirectionType", "1"),
                ("visit_id", ""),
                ("csrf", credential.csrf.as_str()),
                ("csrf_token", credential.csrf.as_str()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&value, "更新直播间封面失败")?;
        room_update_receipt(&value, canonical_room_id)
    }

    async fn verify_room_owner(&self, room_id: &str) -> Result<String> {
        let requested_room_id = room_id
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| anyhow!("直播间号无效"))?
            .to_string();
        let credential = self
            .load_credential()?
            .ok_or_else(|| anyhow!("还没有 B 站登录态"))?;
        let account_id = match self.status_with_credential(&credential).await? {
            AccountStatus::SignedIn { user_id, .. } => user_id,
            AccountStatus::SignedOut => bail!("B 站登录态已失效"),
        };
        let value: Value = self
            .http
            .get(
                self.endpoint("https://api.live.bilibili.com/room/v1/Room/room_init")
                    .as_ref(),
            )
            .header(COOKIE, &credential.cookie_header)
            .header(
                REFERER,
                format!("https://live.bilibili.com/{requested_room_id}"),
            )
            .query(&[("id", requested_room_id.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure_code_zero(&value, "核验直播间归属失败")?;
        let canonical_room_id = string_value(value.pointer("/data/room_id"))
            .filter(|value| value != "0")
            .ok_or_else(|| anyhow!("B 站未返回有效直播间号"))?;
        let broadcaster_id = string_value(value.pointer("/data/uid"))
            .filter(|value| value != "0")
            .ok_or_else(|| anyhow!("B 站未返回直播间主播账号"))?;
        if broadcaster_id != account_id {
            bail!("当前 B 站账号不是该直播间主播，未执行修改");
        }
        Ok(canonical_room_id)
    }

    pub async fn send_danmu(
        &self,
        message: &str,
        room_id: &str,
        reply_to: Option<&str>,
        authorized: impl Fn() -> bool + Send,
    ) -> Result<Option<DanmuResponse>> {
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
        let request = self
            .http
            .post(
                self.endpoint("https://api.live.bilibili.com/msg/send")
                    .as_ref(),
            )
            .header(ORIGIN, "https://live.bilibili.com")
            .header(REFERER, format!("https://live.bilibili.com/{room_id}"))
            .header(COOKIE, &credential.cookie_header)
            .form(&form);
        if !authorized() {
            return Ok(None);
        }
        // No await separates the final authorization check from starting this pinned POST.
        let response = request.send().await?;
        if !response.status().is_success() {
            return Ok(Some(DanmuResponse::UncertainResponse {
                authentication_failed: matches!(response.status().as_u16(), 401 | 403),
                detail: format!(
                    "HTTP {}响应已收到，但业务结果不明；不自动重发",
                    response.status()
                ),
            }));
        }
        let value = match response.json::<Value>().await {
            Ok(value) => value,
            Err(error) => {
                return Ok(Some(DanmuResponse::UncertainResponse {
                    authentication_failed: false,
                    detail: format!(
                        "已收到响应头，响应体读取或解析失败：{error}；结果未知，不自动重发"
                    ),
                }));
            }
        };
        Ok(Some(classify_danmu_response(&value, message)))
    }

    fn load_credential(&self) -> Result<Option<Credential>> {
        match &self.storage {
            AccountStorage::File(path) => load_credential(path),
            AccountStorage::Memory(value) => {
                Ok(value.lock().map_err(|_| anyhow!("登录态锁不可用"))?.clone())
            }
        }
    }

    fn save_credential(&self, credential: &Credential) -> Result<()> {
        match &self.storage {
            AccountStorage::File(path) => {
                crate::storage::write_private_atomic(path, &serde_json::to_vec_pretty(credential)?)
            }
            AccountStorage::Memory(value) => {
                *value.lock().map_err(|_| anyhow!("登录态锁不可用"))? = Some(credential.clone());
                Ok(())
            }
        }
    }
}

fn room_update_receipt(value: &Value, canonical_room_id: String) -> Result<RoomUpdateReceipt> {
    let audit = value.pointer("/data/audit_info");
    let status = audit
        .and_then(|value| value.get("audit_title_status"))
        .and_then(Value::as_i64);
    let reason = audit
        .and_then(|value| value.get("audit_title_reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let rejected = reason.as_deref().is_some_and(|reason| {
        ["拒绝", "驳回", "不通过", "未通过"]
            .iter()
            .any(|word| reason.contains(word))
    });
    Ok(match (status, rejected) {
        (_, true) => RoomUpdateReceipt::Rejected {
            canonical_room_id,
            reason: reason.expect("rejected receipt has a reason"),
        },
        (Some(0), false) => RoomUpdateReceipt::Applied { canonical_room_id },
        (Some(status), false) if status > 0 => RoomUpdateReceipt::Pending {
            canonical_room_id,
            reason,
        },
        (status, false) => RoomUpdateReceipt::Unknown {
            canonical_room_id,
            status,
            reason,
        },
    })
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
        identity: None,
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
#[derive(Debug)]
pub enum DanmuResponse {
    UncertainResponse {
        detail: String,
        authentication_failed: bool,
    },
    Responded {
        proof: bool,
        detail: String,
    },
    ContentRejected {
        code: i64,
        message: String,
    },
    Rejected {
        code: i64,
        message: String,
    },
}
fn classify_danmu_response(value: &Value, requested_message: &str) -> DanmuResponse {
    let Some(code) = value.get("code").and_then(Value::as_i64) else {
        return DanmuResponse::UncertainResponse {
            authentication_failed: false,
            detail: "响应已收到但缺少业务状态码；结果未知，不自动重发".into(),
        };
    };
    let message = value
        .get("message")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("");
    // Only explicit content evidence in the decoded platform response, never network error text.
    let content_rejection = ["屏蔽词", "敏感词", "内容违规", "内容被拒绝"]
        .iter()
        .any(|word| message.contains(word));
    if content_rejection {
        return DanmuResponse::ContentRejected {
            code,
            message: message.into(),
        };
    }
    if code != 0 {
        return DanmuResponse::Rejected {
            code,
            message: message.into(),
        };
    }
    let extra = value.pointer("/data/mode_info/extra");
    let decoded = extra
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let proof = decoded.as_ref().or(extra);
    let detail = match proof {
        Some(proof) if proof.get("send_from_me").and_then(Value::as_bool) != Some(true) => {
            "响应已收到，凭据未确认当前账号"
        }
        Some(proof) if proof.get("content").and_then(Value::as_str) != Some(requested_message) => {
            "响应已收到，凭据内容与请求不一致或缺失"
        }
        Some(_) => "响应已收到，发送凭据匹配；仍须真实回显",
        None => "响应已收到，无发送凭据；仍等待真实回显",
    };
    let accepted = proof.is_some_and(|p| {
        p.get("send_from_me").and_then(Value::as_bool) == Some(true)
            && p.get("content").and_then(Value::as_str) == Some(requested_message)
    });
    DanmuResponse::Responded {
        proof: accepted,
        detail: detail.into(),
    }
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
    DanmuBatch(Vec<DanmuEvent>),
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
    if command_name(value) == Some("SEND_GIFT_V2") {
        let data = value.get("data").unwrap_or(&Value::Null);
        if let Some(events) = parse_gift_v2(data).filter(|events| !events.is_empty()) {
            return RealtimeMessage::DanmuBatch(events);
        }
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
    use crate::bilibili::parser::SEND_GIFT_V2_FIXTURE;

    async fn account_server(responses: Vec<String>) -> (Url, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0; 1024];
                    let count = socket.read(&mut bytes).await.unwrap();
                    assert!(count > 0 && request.len() + count <= 16384);
                    request.extend_from_slice(&bytes[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (base, task)
    }

    fn account_poll_response() -> String {
        let body = r#"{"code":0,"data":{"code":0}}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: SESSDATA=assistant-secret; Path=/\r\nSet-Cookie: bili_jct=assistant-csrf; Path=/\r\nSet-Cookie: DedeUserID=22; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn account_fixture(uid: &str) -> Credential {
        Credential {
            cookie_header: format!("SESSDATA=secret-{uid}; bili_jct=csrf-{uid}; DedeUserID={uid}"),
            csrf: format!("csrf-{uid}"),
            identity: Some(AccountStatus::SignedIn {
                display_name: format!("用户{uid}"),
                user_id: uid.into(),
            }),
        }
    }

    fn room_nav_response(uid: u64) -> String {
        public_profile_http_response(&format!(
            r#"{{"code":0,"data":{{"isLogin":true,"uname":"用户{uid}","mid":{uid}}}}}"#
        ))
    }

    fn room_owner_response(room_id: u64, uid: u64) -> String {
        public_profile_http_response(&format!(
            r#"{{"code":0,"data":{{"room_id":{room_id},"uid":{uid}}}}}"#
        ))
    }

    fn room_editor(base: Url, path: PathBuf) -> AccountClient {
        let mut account = AccountClient::new(path).unwrap();
        account.test_endpoint = Some(base);
        account.save_credential(&account_fixture("11")).unwrap();
        account
    }

    fn write_test_cover(path: &Path) {
        std::fs::write(path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    }

    #[test]
    fn room_update_receipts_never_claim_unknown_or_rejected_applied() {
        let unknown = room_update_receipt(&json!({"code": 0, "data": {}}), "9".into()).unwrap();
        assert!(matches!(
            unknown,
            RoomUpdateReceipt::Unknown { status: None, .. }
        ));
        let rejected = room_update_receipt(
            &json!({"code": 0, "data": {"audit_info": {
                "audit_title_status": 1,
                "audit_title_reason": "内容审核不通过"
            }}}),
            "9".into(),
        )
        .unwrap();
        assert!(
            matches!(rejected, RoomUpdateReceipt::Rejected { reason, .. } if reason == "内容审核不通过")
        );
    }

    #[tokio::test]
    async fn room_ownership_check_prevents_any_write() {
        let (base, server) =
            account_server(vec![room_nav_response(11), room_owner_response(9001, 22)]).await;
        let temp = tempfile::tempdir().unwrap();
        let account = room_editor(base, temp.path().join("account.json"));

        let error = account
            .update_room_title("123", "新标题")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("不是该直播间主播"));
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
        assert!(requests[1].starts_with("GET /room/v1/Room/room_init?id=123 "));
    }

    #[tokio::test]
    async fn room_title_update_uses_canonical_room_csrf_cookie_and_form_encoding() {
        let (base, server) = account_server(vec![
            room_nav_response(11),
            room_owner_response(9001, 11),
            public_profile_http_response(
                r#"{"code":0,"data":{"audit_info":{"audit_title_status":0,"audit_title_reason":""}}}"#,
            ),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let account = room_editor(base, temp.path().join("account.json"));

        let receipt = account.update_room_title("123", "甲 &乙").await.unwrap();
        assert_eq!(
            receipt,
            RoomUpdateReceipt::Applied {
                canonical_room_id: "9001".into()
            }
        );
        let requests = server.await.unwrap();
        let write = &requests[2];
        assert!(write.starts_with("POST /room/v1/Room/update "));
        assert!(
            write
                .to_ascii_lowercase()
                .contains("cookie: sessdata=secret-11; bili_jct=csrf-11; dedeuserid=11")
        );
        assert!(write.contains("room_id=9001"));
        assert!(write.contains("title=%E7%94%B2+%26%E4%B9%99"));
        assert!(write.contains("csrf=csrf-11"));
        assert!(write.contains("csrf_token=csrf-11"));
    }

    #[tokio::test]
    async fn room_cover_rejects_invalid_file_before_network() {
        let (base, server) = account_server(Vec::new()).await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cover.txt");
        std::fs::write(&path, b"not an image").unwrap();
        let account = room_editor(base, temp.path().join("account.json"));

        let error = account.update_room_cover("123", &path).await.unwrap_err();
        assert!(error.to_string().contains("PNG 或 JPEG"));
        let oversized = temp.path().join("oversized.png");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(ROOM_COVER_LIMIT_BYTES + 1).unwrap();
        let error = account
            .update_room_cover("123", &oversized)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("2 MiB"));
        assert!(server.await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn room_cover_upload_failure_does_not_attempt_update() {
        let (base, server) = account_server(vec![
            room_nav_response(11),
            room_owner_response(9001, 11),
            public_profile_http_response(r#"{"code":1001,"message":"上传被拒绝"}"#),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cover.png");
        write_test_cover(&path);
        let account = room_editor(base, temp.path().join("account.json"));

        let error = account.update_room_cover("123", &path).await.unwrap_err();
        assert!(error.to_string().contains("上传被拒绝"));
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[2].starts_with("POST /x/upload/web/image?csrf=csrf-11 "));
        assert!(requests[2].contains("name=\"bucket\""));
        assert!(requests[2].contains("name=\"dir\""));
        assert!(requests[2].contains("name=\"file\"; filename=\"cover.png\""));
        assert!(requests[2].contains("Content-Type: image/png"));
    }

    #[tokio::test]
    async fn room_cover_update_failure_propagates_after_upload() {
        let (base, server) = account_server(vec![
            room_nav_response(11),
            room_owner_response(9001, 11),
            public_profile_http_response(
                r#"{"code":0,"data":{"location":"https://i0.hdslb.com/bfs/live/cover.png"}}"#,
            ),
            public_profile_http_response(r#"{"code":100402,"message":"图片地址不合法"}"#),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cover.png");
        write_test_cover(&path);
        let account = room_editor(base, temp.path().join("account.json"));

        let error = account.update_room_cover("123", &path).await.unwrap_err();
        assert!(error.to_string().contains("图片地址不合法"));
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[3].starts_with("POST /xlive/app-blink/v1/preLive/UpdatePreLiveInfo "));
    }

    #[tokio::test]
    async fn room_cover_success_reports_pending_audit_truthfully() {
        let (base, server) = account_server(vec![
            room_nav_response(11),
            room_owner_response(9001, 11),
            public_profile_http_response(
                r#"{"code":0,"data":{"location":"https://i0.hdslb.com/bfs/live/cover.png"}}"#,
            ),
            public_profile_http_response(
                r#"{"code":0,"data":{"audit_info":{"audit_title_status":2,"audit_title_reason":"先发后审"}}}"#,
            ),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cover.png");
        write_test_cover(&path);
        let account = room_editor(base, temp.path().join("account.json"));

        let receipt = account.update_room_cover("123", &path).await.unwrap();
        assert_eq!(
            receipt,
            RoomUpdateReceipt::Pending {
                canonical_room_id: "9001".into(),
                reason: Some("先发后审".into())
            }
        );
        let requests = server.await.unwrap();
        let update = &requests[3];
        assert!(update.contains("room_id=9001"));
        assert!(update.contains("cover=https%3A%2F%2Fi0.hdslb.com%2Fbfs%2Flive%2Fcover.png"));
        assert!(update.contains("csrf=csrf-11"));
        assert!(update.contains("csrf_token=csrf-11"));
    }

    #[tokio::test]
    async fn account_isolation_real_qr_preflight_and_pinned_post() {
        let (base, server) = account_server(vec![
            public_profile_http_response(r#"{"code":0,"data":{"qrcode_key":"qr-token","url":"https://passport.bilibili.com/qr-test"}}"#),
            account_poll_response(),
            public_profile_http_response(r#"{"code":0,"data":{"isLogin":true,"uname":"助手测试","mid":22}}"#),
            public_profile_http_response(r#"{"code":0,"message":""}"#),
        ]).await;
        let temp = tempfile::tempdir().unwrap();
        let main_path = temp.path().join("main.json");
        let mut main = AccountClient::new(main_path.clone()).unwrap();
        main.test_endpoint = Some(base);
        main.save_credential(&account_fixture("11")).unwrap();
        let original = std::fs::read(&main_path).unwrap();
        let staged = main.staged();
        let challenge = staged.login_challenge().await.unwrap();
        assert_eq!(challenge.key, "qr-token");
        let login = staged.poll_login(&challenge.key).await.unwrap();
        assert!(
            matches!(login, LoginPoll::SignedIn(AccountStatus::SignedIn { user_id, .. }) if user_id == "22")
        );
        assert_eq!(std::fs::read(&main_path).unwrap(), original);
        let independent = staged
            .persist_to(temp.path().join("independent.json"))
            .unwrap();
        let pinned = independent.snapshot().unwrap();
        // Neither replacement nor logout changes the identity pinned before the POST.
        independent.save_credential(&account_fixture("33")).unwrap();
        independent.sign_out().unwrap();
        assert!(
            pinned
                .send_danmu("不应发送", "1", None, || false)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            pinned
                .send_danmu("隔离测试", "1", Some("44"), || true)
                .await
                .unwrap(),
            Some(DanmuResponse::Responded { .. })
        ));
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("GET /x/passport-login/web/qrcode/generate "));
        assert!(
            requests[1].starts_with("GET /x/passport-login/web/qrcode/poll?qrcode_key=qr-token ")
        );
        assert!(
            !requests[0].to_lowercase().contains("cookie:")
                && !requests[1].to_lowercase().contains("cookie:")
        );
        assert!(requests[2].contains("SESSDATA=assistant-secret"));
        let post = &requests[3];
        assert!(post.starts_with("POST /msg/send "));
        assert!(
            post.contains("SESSDATA=assistant-secret")
                && post.contains("csrf=assistant-csrf")
                && post.contains("reply_mid=44")
        );
        assert!(!post.contains("secret-11") && !post.contains("secret-33"));
        assert_eq!(std::fs::read(&main_path).unwrap(), original);
    }

    #[tokio::test]
    async fn account_isolation_failed_login_keeps_original_file() {
        let (base, server) = account_server(vec![
            account_poll_response(),
            public_profile_http_response(r#"{"code":-101,"data":{"isLogin":false}}"#),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("account.json");
        let mut account = AccountClient::new(path.clone()).unwrap();
        account.test_endpoint = Some(base);
        account.save_credential(&account_fixture("11")).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(account.poll_login("failed").await.is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn account_isolation_post_authentication_loss_is_uncertain_and_revocable() {
        let (base, server) = account_server(vec![
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let mut account = AccountClient::new(temp.path().join("account.json")).unwrap();
        account.test_endpoint = Some(base);
        account.save_credential(&account_fixture("22")).unwrap();
        assert!(matches!(
            account
                .send_danmu("鉴权边界", "1", None, || true)
                .await
                .unwrap(),
            Some(DanmuResponse::UncertainResponse {
                authentication_failed: true,
                ..
            })
        ));
        assert_eq!(server.await.unwrap().len(), 1);
    }

    fn public_profile_card(uid: u64, sign: &str) -> String {
        json!({"code": 0, "data": {"card": {"mid": uid.to_string(), "sign": sign}}}).to_string()
    }

    fn public_profile_http_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn public_profile_server(
        responses: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/card", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0 && request.len() + count <= 8192);
                    request.extend_from_slice(&buffer[..count]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(!request.to_ascii_lowercase().contains("\r\ncookie:"));
                assert!(!request.to_ascii_lowercase().contains("\r\nauthorization:"));
                requests.push(request.lines().next().unwrap().to_owned());
                // A size-limited consumer may close before consuming the whole body.
                let _ = socket.write_all(response.as_bytes()).await;
            }
            requests
        });
        (url, server)
    }

    fn public_profile_test_client() -> BilibiliClient {
        let mut client =
            BilibiliClient::new(PathBuf::from("/nonexistent/public-profile-session")).unwrap();
        client.public_http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        client
    }

    #[test]
    fn public_profile_parses_signature_with_verified_source_and_safe_text() {
        let value = json!({"code": 0, "data": {"card": {
            "mid": "42", "sign": " \u{1b}知识\n分享\u{202e} \t", "description": "不应读取的字段"
        }}});
        assert_eq!(
            public_profile_from_response(&value, 42).unwrap(),
            Some(PublicProfile {
                user_id: "42".into(),
                description: "知识分享".into(),
                source_url: "https://space.bilibili.com/42".into(),
            })
        );
        let long = json!({"code": 0, "data": {"card": {"mid": 42, "sign": "知".repeat(513)}}});
        assert_eq!(
            public_profile_from_response(&long, 42)
                .unwrap()
                .unwrap()
                .description,
            "知".repeat(512)
        );
        let empty = json!({"code": 0, "data": {"card": {"mid": "42", "sign": " \n\u{0}\u{202e}"}}});
        assert_eq!(public_profile_from_response(&empty, 42).unwrap(), None);
    }

    #[test]
    fn public_profile_rejects_unverified_or_malformed_sources() {
        for invalid in [
            "",
            "0",
            "-42",
            "+42",
            "42/other",
            " 42",
            "４２",
            "18446744073709551616",
        ] {
            assert!(public_profile_user_id(invalid).is_err(), "{invalid:?}");
        }
        for value in [
            json!({"code": -412}),
            json!({"data": {"card": {"mid": "42", "sign": "简介"}}}),
            json!({"code": 0, "data": null}),
            json!({"code": 0, "data": {"card": {"sign": "简介"}}}),
            json!({"code": 0, "data": {"card": {"mid": "43", "sign": "简介"}}}),
            json!({"code": 0, "data": {"card": {"mid": "42", "sign": null}}}),
        ] {
            assert!(public_profile_from_response(&value, 42).is_err(), "{value}");
        }
    }

    #[tokio::test]
    async fn public_profile_cache_shares_clones_isolates_uids_and_expires() {
        let bodies = [
            public_profile_card(42, "甲"),
            public_profile_card(43, "乙"),
            public_profile_card(42, "更新"),
        ];
        let (url, server) = public_profile_server(
            bodies
                .iter()
                .map(|body| public_profile_http_response(body))
                .collect(),
        )
        .await;
        let client = public_profile_test_client();
        let clone = client.clone();
        let now = Instant::now();
        let first = client
            .broadcaster_profile_at("42", &url, now)
            .await
            .unwrap();
        let second = clone
            .broadcaster_profile_at("43", &url, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.description, "乙");
        assert_eq!(second.source_url, "https://space.bilibili.com/43");
        assert_eq!(
            clone
                .broadcaster_profile_at(
                    "00042",
                    &url,
                    now + PUBLIC_PROFILE_TTL - Duration::from_nanos(1)
                )
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            client
                .broadcaster_profile_at("42", &url, now + PUBLIC_PROFILE_TTL)
                .await
                .unwrap()
                .unwrap()
                .description,
            "更新"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap(),
            [
                "GET /card?mid=42 HTTP/1.1",
                "GET /card?mid=43 HTTP/1.1",
                "GET /card?mid=42 HTTP/1.1"
            ]
        );
    }

    #[tokio::test]
    async fn public_profile_caches_errors_and_empty_profiles_with_separate_ttls() {
        let bodies = [
            json!({"code": -412}).to_string(),
            public_profile_card(42, ""),
            public_profile_card(42, "恢复"),
        ];
        let (url, server) = public_profile_server(
            bodies
                .iter()
                .map(|body| public_profile_http_response(body))
                .collect(),
        )
        .await;
        let client = public_profile_test_client();
        let now = Instant::now();
        assert!(
            client
                .broadcaster_profile_at("42", &url, now)
                .await
                .is_err()
        );
        assert!(
            client
                .clone()
                .broadcaster_profile_at(
                    "42",
                    &url,
                    now + PUBLIC_PROFILE_ERROR_TTL - Duration::from_nanos(1)
                )
                .await
                .is_err()
        );
        let recovered = now + PUBLIC_PROFILE_ERROR_TTL;
        assert_eq!(
            client
                .broadcaster_profile_at("42", &url, recovered)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            client
                .broadcaster_profile_at(
                    "42",
                    &url,
                    recovered + PUBLIC_PROFILE_TTL - Duration::from_nanos(1)
                )
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            client
                .broadcaster_profile_at("42", &url, recovered + PUBLIC_PROFILE_TTL)
                .await
                .unwrap()
                .unwrap()
                .description,
            "恢复"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap(),
            vec!["GET /card?mid=42 HTTP/1.1"; 3]
        );
    }

    #[tokio::test]
    async fn public_profile_cache_evicts_when_bounded_capacity_is_reached() {
        let responses = (1..=PUBLIC_PROFILE_CACHE_LIMIT + 1)
            .map(|uid| public_profile_http_response(&public_profile_card(uid as u64, "初始")))
            .chain(std::iter::once(public_profile_http_response(
                &public_profile_card(1, "重新请求"),
            )))
            .collect();
        let (url, server) = public_profile_server(responses).await;
        let client = public_profile_test_client();
        let now = Instant::now();
        for uid in 1..=PUBLIC_PROFILE_CACHE_LIMIT + 1 {
            assert_eq!(
                client
                    .broadcaster_profile_at(&uid.to_string(), &url, now)
                    .await
                    .unwrap()
                    .unwrap()
                    .description,
                "初始"
            );
        }
        assert_eq!(
            client
                .broadcaster_profile_at("1", &url, now)
                .await
                .unwrap()
                .unwrap()
                .description,
            "重新请求"
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn public_profile_rejects_oversized_http_bodies_and_redirects() {
        let oversized = "x".repeat(PUBLIC_PROFILE_RESPONSE_LIMIT + 1);
        let responses = vec![
            public_profile_http_response(&oversized),
            format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{oversized}\r\n0\r\n\r\n", oversized.len()),
            "HTTP/1.1 302 Found\r\nLocation: https://space.bilibili.com/42\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ];
        let (url, server) = public_profile_server(responses).await;
        let client = public_profile_test_client();
        for uid in ["42", "43", "44"] {
            assert!(
                client
                    .broadcaster_profile_at(uid, &url, Instant::now())
                    .await
                    .is_err()
            );
        }
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

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
    fn classifies_current_send_gift_v2_as_a_gift_batch() {
        let value = json!({
            "cmd": "SEND_GIFT_V2",
            "data": {"pb": SEND_GIFT_V2_FIXTURE}
        });

        let RealtimeMessage::DanmuBatch(events) = classify_realtime_message(&value) else {
            panic!("SEND_GIFT_V2 should be delivered as a gift batch");
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, DanmuEventKind::Gift);
        assert_eq!(events[0].content, "开启 心动盲盒 ×1，爆出 电影票 ×1");
    }

    #[tokio::test]
    async fn treats_http_connection_failures_as_recoverable() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap_err();

        assert!(error.is_connect());
        assert!(realtime_connection_error_event(error.into()).is_none());
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
            identity: None,
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
    fn danmu_response_without_or_mismatched_proof_still_waits_for_echo() {
        for response in [
            json!({"code":0,"data":null}),
            json!({"code":0,"data":{"mode_info":{"extra":"{\"content\":\"other\",\"send_from_me\":true}"}}}),
        ] {
            assert!(matches!(
                classify_danmu_response(&response, "正文"),
                DanmuResponse::Responded { proof: false, .. }
            ));
        }
        assert!(matches!(
            classify_danmu_response(
                &json!({"code":0,"data":{"mode_info":{"extra":{"content":"正文","send_from_me":true}}}}),
                "正文"
            ),
            DanmuResponse::Responded { proof: true, .. }
        ));
    }
    #[test]
    fn only_platform_content_evidence_is_repairable_rejection() {
        assert!(matches!(
            classify_danmu_response(&json!({"code":100,"message":"内容违规"}), "正文"),
            DanmuResponse::ContentRejected { code: 100, .. }
        ));
        assert!(matches!(
            classify_danmu_response(&json!({"code":-101,"message":"账号未登录"}), "正文"),
            DanmuResponse::Rejected { code: -101, .. }
        ));
        assert!(matches!(
            classify_danmu_response(&json!({"message":"敏感词"}), "正文"),
            DanmuResponse::UncertainResponse { .. }
        ));
    }
}
