mod meter;

pub use meter::MicrophoneLevel;

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::StreamExt;
use meter::{LevelSmoother, SILENCE_DB, peak_db};
use obws::{Client, client::ConnectConfig, events::Event, requests::EventSubscription};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv6Addr},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, watch},
    time::{Instant, MissedTickBehavior, interval, sleep, timeout},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsConfiguration {
    pub host: String,
    pub port: u16,
    pub microphone_input_name: String,
    pub default_live_scene: String,
}

impl Default for ObsConfiguration {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 4455,
            microphone_input_name: "Mic/Aux".into(),
            default_live_scene: String::new(),
        }
    }
}

impl ObsConfiguration {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(data) => Ok(serde_json::from_slice(&data).context("OBS 配置格式错误")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let path = std::path::absolute(path)?;
        crate::persistence::check_path(&path)?;
        crate::workspace_config::check_file(&path)?;
        let parent = path.parent().context("OBS 配置路径没有父目录")?;
        crate::bridge::wire::private_root(parent)?;
        crate::storage::write_private_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }
}

const OBS_PASSWORD_FILENAME: &str = "obs-password";
const MAX_OBS_PASSWORD: u64 = 16 * 1024;

/// Host-owned credential storage. Errors are authoritative: callers must not fall
/// back to another store (in particular, from Keychain to a plaintext file).
pub trait ObsPasswordStore: Send + Sync {
    fn read(&self, host: &str, port: u16) -> Result<Option<String>>;
    fn write(&self, host: &str, port: u16, password: &str) -> Result<()>;
}

/// The TUI's existing environment → private-file policy. Desktop hosts inject
/// their own store instead; isolated/local hosts disable environment inheritance.
pub struct FileObsPasswordStore {
    configuration_path: PathBuf,
    allow_environment: bool,
}

impl FileObsPasswordStore {
    pub fn new(configuration_path: PathBuf, allow_environment: bool) -> Self {
        Self {
            configuration_path,
            allow_environment,
        }
    }

    fn path(&self, create: bool) -> Result<Option<PathBuf>> {
        let path = std::path::absolute(
            self.configuration_path
                .with_file_name(OBS_PASSWORD_FILENAME),
        )?;
        crate::persistence::check_path(&path)?;
        let parent = path.parent().context("OBS 密码路径没有父目录")?;
        if !create && !parent.try_exists()? {
            return Ok(None);
        }
        crate::bridge::wire::private_root(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                std::fs::metadata(parent)?.uid() == unsafe { libc::geteuid() },
                "OBS 密码目录须属于当前用户"
            );
        }
        crate::workspace_config::check_file(&path).context("OBS 密码文件不安全")?;
        Ok(Some(path))
    }
}

impl ObsPasswordStore for FileObsPasswordStore {
    fn read(&self, _host: &str, _port: u16) -> Result<Option<String>> {
        use std::io::Read;
        if self.allow_environment
            && let Some(password) = std::env::var("OBS_API_PASSWORD")
                .ok()
                .filter(|password| !password.is_empty())
        {
            ensure!(
                password.len() as u64 <= MAX_OBS_PASSWORD,
                "OBS 密码超过16KiB"
            );
            return Ok(Some(password));
        }
        let Some(path) = self.path(false)? else {
            return Ok(None);
        };
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("读取 OBS 密码文件失败"),
        };
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "OBS 密码必须为普通文件");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.nlink() == 1
                    && metadata.mode() & 0o077 == 0,
                "OBS 密码文件须由当前用户独占且仅本人可访问（0600）"
            );
        }
        let mut password = String::new();
        file.take(MAX_OBS_PASSWORD + 1)
            .read_to_string(&mut password)
            .context("读取 OBS 密码文件失败")?;
        ensure!(
            password.len() as u64 <= MAX_OBS_PASSWORD,
            "OBS 密码超过16KiB"
        );
        Ok((!password.is_empty()).then_some(password))
    }

    fn write(&self, _host: &str, _port: u16, password: &str) -> Result<()> {
        ensure!(
            password.len() as u64 <= MAX_OBS_PASSWORD,
            "OBS 密码超过16KiB"
        );
        let Some(path) = self.path(!password.is_empty())? else {
            return Ok(());
        };
        if password.is_empty() {
            return match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            };
        }
        crate::storage::write_private_atomic(&path, password.as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    Unknown,
    Stopped,
    Starting,
    Live,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneState {
    Unknown,
    Unmuted,
    Muted,
}

#[derive(Debug, Clone)]
pub struct ObsStatus {
    pub current_scene: String,
    pub stream: StreamState,
    pub microphone: MicrophoneState,
    pub compatibility_warning: Option<String>,
}

struct ObsConnectionState {
    configuration: ObsConfiguration,
    client: Option<Arc<Client>>,
    password_generation: u64,
}

#[derive(Clone)]
pub struct ObsController {
    state: Arc<Mutex<ObsConnectionState>>,
    connect_lock: Arc<Mutex<()>>,
    configuration_path: PathBuf,
    passwords: Arc<dyn ObsPasswordStore>,
}

impl ObsController {
    pub fn new(
        configuration: ObsConfiguration,
        configuration_path: PathBuf,
        passwords: Arc<dyn ObsPasswordStore>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(ObsConnectionState {
                configuration,
                client: None,
                password_generation: 0,
            })),
            configuration_path,
            passwords,
            connect_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn update_configuration(&self, configuration: ObsConfiguration) -> Result<()> {
        let _connect_guard = self.connect_lock.lock().await;
        let mut state = self.state.lock().await;
        configuration.save(&self.configuration_path)?;
        state.configuration = configuration;
        state.client = None;
        Ok(())
    }

    pub async fn set_password(&self, password: &str) -> Result<()> {
        let configuration = self.configuration().await;
        let passwords = Arc::clone(&self.passwords);
        let host = configuration.host.clone();
        let port = configuration.port;
        let password = password.to_owned();
        tokio::task::spawn_blocking(move || passwords.write(&host, port, &password))
            .await
            .context("OBS 密码存储任务失败")??;
        let _connect_guard = self.connect_lock.lock().await;
        let mut state = self.state.lock().await;
        state.password_generation = state.password_generation.wrapping_add(1);
        if state.configuration.host == configuration.host
            && state.configuration.port == configuration.port
        {
            state.client = None;
        }
        Ok(())
    }

    pub async fn set_host(&self, host: String) -> Result<()> {
        let host = normalize_obs_host(&host)?;
        self.update_configuration_field(|configuration| configuration.host = host)
            .await
    }

    pub async fn set_port(&self, port: u16) -> Result<()> {
        if port == 0 {
            bail!("OBS WebSocket 端口必须大于 0");
        }
        self.update_configuration_field(|configuration| configuration.port = port)
            .await
    }

    pub async fn set_microphone_name(&self, name: String) -> Result<()> {
        let name = name.trim().to_string();
        if name.is_empty() {
            bail!("麦克风输入名称不能为空");
        }
        let inputs = self.list_inputs().await?;
        if !inputs.iter().any(|input| input == &name) {
            bail!(
                "OBS 中不存在输入「{name}」；可用输入：{}",
                inputs.join("、")
            );
        }
        self.update_configuration_field(|configuration| configuration.microphone_input_name = name)
            .await
    }

    pub async fn fetch_status(&self) -> Result<ObsStatus> {
        let client = self.client().await?;
        let microphone_name = self.microphone_input_name().await;
        let scenes = client.scenes();
        let streaming = client.streaming();
        let inputs = client.inputs();
        let result = tokio::try_join!(
            scenes.current_program_scene(),
            streaming.status(),
            inputs.muted(microphone_name.as_str().into()),
        );
        let (scene, stream, muted) = match result {
            Ok(result) => result,
            Err(error) => return Err(self.invalidate_after(error).await),
        };
        Ok(ObsStatus {
            current_scene: scene.id.name,
            stream: if stream.active {
                StreamState::Live
            } else {
                StreamState::Stopped
            },
            microphone: if muted {
                MicrophoneState::Muted
            } else {
                MicrophoneState::Unmuted
            },
            compatibility_warning: None,
        })
    }

    pub async fn list_scenes(&self) -> Result<Vec<String>> {
        let client = self.client().await?;
        match client.scenes().list().await {
            Ok(scenes) => Ok(scenes
                .scenes
                .into_iter()
                .map(|scene| scene.id.name)
                .collect()),
            Err(error) => Err(self.invalidate_after(error).await),
        }
    }

    pub async fn list_inputs(&self) -> Result<Vec<String>> {
        let client = self.client().await?;
        match client.inputs().list(None).await {
            Ok(inputs) => Ok(inputs.into_iter().map(|input| input.id.name).collect()),
            Err(error) => Err(self.invalidate_after(error).await),
        }
    }

    pub async fn switch_scene(&self, scene: &str) -> Result<()> {
        let client = self.client().await?;
        if let Err(error) = client.scenes().set_current_program_scene(scene).await {
            return Err(self.invalidate_after(error).await);
        }
        let current = match client.scenes().current_program_scene().await {
            Ok(current) => current,
            Err(error) => return Err(self.invalidate_after(error).await),
        };
        if current.id.name.trim() != scene.trim() {
            bail!("无法确认 OBS 已切换到场景「{scene}」");
        }
        Ok(())
    }

    pub async fn set_microphone_muted(&self, muted: bool) -> Result<()> {
        let input = self.microphone_input_name().await;
        if input.trim().is_empty() {
            bail!("尚未配置 OBS 麦克风输入");
        }
        let client = self.client().await?;
        if let Err(error) = client
            .inputs()
            .set_muted(input.as_str().into(), muted)
            .await
        {
            return Err(self.invalidate_after(error).await);
        }
        let confirmed = match client.inputs().muted(input.as_str().into()).await {
            Ok(confirmed) => confirmed,
            Err(error) => return Err(self.invalidate_after(error).await),
        };
        if confirmed != muted {
            bail!("无法确认 OBS 麦克风状态");
        }
        Ok(())
    }

    pub async fn start_stream(&self) -> Result<()> {
        let scene = self.configuration().await.default_live_scene;
        if !scene.trim().is_empty() {
            self.switch_scene(&scene).await?;
        }
        let client = self.client().await?;
        if let Err(error) = client.streaming().start().await {
            return Err(self.invalidate_after(error).await);
        }
        self.confirm_stream(true).await
    }

    pub async fn stop_stream(&self) -> Result<()> {
        let client = self.client().await?;
        if let Err(error) = client.streaming().stop().await {
            return Err(self.invalidate_after(error).await);
        }
        self.confirm_stream(false).await
    }

    pub async fn monitor_microphone_levels(
        &self,
        output: watch::Sender<Option<MicrophoneLevel>>,
        mut stop: watch::Receiver<bool>,
    ) {
        const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
        const STALE_AFTER: Duration = Duration::from_millis(500);
        const RETRY_AFTER: Duration = Duration::from_secs(1);

        loop {
            if *stop.borrow() {
                return;
            }

            let client = match self.client().await {
                Ok(client) => client,
                Err(_) => {
                    if output.send(None).is_err() || wait_for_stop(&mut stop, RETRY_AFTER).await {
                        return;
                    }
                    continue;
                }
            };
            let mut events = match client.events() {
                Ok(events) => Box::pin(events),
                Err(_) => {
                    self.invalidate_client(&client).await;
                    if output.send(None).is_err() || wait_for_stop(&mut stop, RETRY_AFTER).await {
                        return;
                    }
                    continue;
                }
            };
            let microphone_name = self.microphone_input_name().await;
            let mut ticker = interval(SAMPLE_INTERVAL);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await;
            let mut smoother = LevelSmoother::default();
            let mut pending_peak = None::<f32>;
            let mut last_sample = Instant::now();
            let mut received_sample = false;

            loop {
                tokio::select! {
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() {
                            return;
                        }
                    }
                    event = events.next() => {
                        let Some(event) = event else {
                            break;
                        };
                        if let Event::InputVolumeMeters { inputs } = event
                            && let Some(input) = inputs.iter().find(|input| input.name == microphone_name)
                        {
                            let sample = peak_db(&input.levels);
                            pending_peak = Some(pending_peak.map_or(sample, |peak| peak.max(sample)));
                            last_sample = Instant::now();
                            received_sample = true;
                        }
                    }
                    _ = ticker.tick() => {
                        if !received_sample || last_sample.elapsed() > STALE_AFTER {
                            if output.send(None).is_err() {
                                return;
                            }
                            continue;
                        }
                        let sample = pending_peak.take().unwrap_or(SILENCE_DB);
                        let level = smoother.update(sample, SAMPLE_INTERVAL.as_secs_f32());
                        if output.send(Some(level)).is_err() {
                            return;
                        }
                    }
                }
            }

            self.invalidate_client(&client).await;
            if output.send(None).is_err() || wait_for_stop(&mut stop, RETRY_AFTER).await {
                return;
            }
        }
    }
    pub async fn microphone_input_name(&self) -> String {
        self.configuration().await.microphone_input_name
    }

    pub(crate) async fn configuration(&self) -> ObsConfiguration {
        self.state.lock().await.configuration.clone()
    }
    async fn update_configuration_field(
        &self,
        update: impl FnOnce(&mut ObsConfiguration),
    ) -> Result<()> {
        let _connect_guard = self.connect_lock.lock().await;
        let mut state = self.state.lock().await;
        let mut configuration = state.configuration.clone();
        update(&mut configuration);
        if configuration == state.configuration {
            return Ok(());
        }
        configuration.save(&self.configuration_path)?;
        state.configuration = configuration;
        state.client = None;
        Ok(())
    }

    async fn client(&self) -> Result<Arc<Client>> {
        {
            let state = self.state.lock().await;
            if let Some(client) = &state.client {
                return Ok(Arc::clone(client));
            }
        }
        loop {
            let (configuration, password_generation) = {
                let state = self.state.lock().await;
                if let Some(client) = &state.client {
                    return Ok(Arc::clone(client));
                }
                (state.configuration.clone(), state.password_generation)
            };
            let passwords = Arc::clone(&self.passwords);
            let host = configuration.host.clone();
            let port = configuration.port;
            let password = tokio::task::spawn_blocking(move || passwords.read(&host, port))
                .await
                .context("OBS 密码存储任务失败")?
                .context("读取 OBS 密码失败")?;
            let _connect_guard = self.connect_lock.lock().await;
            {
                let state = self.state.lock().await;
                if let Some(client) = &state.client {
                    return Ok(Arc::clone(client));
                }
                if state.configuration != configuration
                    || state.password_generation != password_generation
                {
                    continue;
                }
            }
            let password_configured = password.is_some();
            let connect = Client::connect_with_config(ConnectConfig {
                host: configuration.host.clone(),
                port: configuration.port,
                dangerous: None,
                password,
                event_subscriptions: Some(
                    EventSubscription::INPUTS
                        | EventSubscription::SCENES
                        | EventSubscription::OUTPUTS
                        | EventSubscription::INPUT_VOLUME_METERS,
                ),
                broadcast_capacity: 64,
                connect_timeout: Duration::from_secs(8),
            });
            let client = timeout(Duration::from_secs(9), connect)
                .await
                .context("连接 OBS WebSocket 超时")?
                .with_context(|| {
                    let target = format!(
                        "无法连接 OBS WebSocket {}:{}",
                        configuration.host, configuration.port
                    );
                    if password_configured {
                        target
                    } else {
                        format!("{target}；OBS 已启用认证时，请在 OBS 设置中保存连接密码")
                    }
                })?;
            let client = Arc::new(client);
            let mut state = self.state.lock().await;
            if state.configuration != configuration
                || state.password_generation != password_generation
            {
                continue;
            }
            if let Some(existing) = &state.client {
                return Ok(Arc::clone(existing));
            }
            state.client = Some(Arc::clone(&client));
            return Ok(client);
        }
    }
    async fn invalidate_client(&self, client: &Arc<Client>) {
        let mut state = self.state.lock().await;
        if state
            .client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, client))
        {
            state.client = None;
        }
    }

    async fn invalidate_after(&self, error: obws::error::Error) -> anyhow::Error {
        self.state.lock().await.client = None;
        anyhow!("OBS WebSocket 操作失败：{error}")
    }

    async fn confirm_stream(&self, expected_active: bool) -> Result<()> {
        for _ in 0..10 {
            let client = self.client().await?;
            match client.streaming().status().await {
                Ok(status) if status.active == expected_active => return Ok(()),
                Ok(_) => {}
                Err(error) => return Err(self.invalidate_after(error).await),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        bail!("无法确认 OBS 推流状态，请立即到 OBS 核对")
    }
}
fn normalize_obs_host(host: &str) -> Result<String> {
    let host = host.trim();
    if host.is_empty() {
        bail!("OBS WebSocket 主机不能为空");
    }
    if host.contains("://")
        || host.contains(['/', '?', '#', '@'])
        || host.chars().any(char::is_whitespace)
    {
        bail!("OBS WebSocket 主机只填写主机名或 IP，协议和端口需分开设置");
    }

    if let Ok(address) = IpAddr::from_str(host) {
        return Ok(match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        });
    }
    if let Some(address) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        return Ipv6Addr::from_str(address)
            .map(|address| format!("[{address}]"))
            .map_err(|_| anyhow!("OBS WebSocket IPv6 地址格式错误"));
    }
    if host.contains([':', '[', ']']) {
        bail!("OBS WebSocket 主机只填写主机名或 IP，端口需分开设置");
    }

    match url::Host::parse(host).context("OBS WebSocket 主机格式错误")? {
        url::Host::Domain(domain) if !domain.is_empty() => Ok(domain),
        url::Host::Ipv4(address) => Ok(address.to_string()),
        url::Host::Ipv6(address) => Ok(format!("[{address}]")),
        _ => bail!("OBS WebSocket 主机格式错误"),
    }
}

async fn wait_for_stop(stop: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    tokio::select! {
        changed = stop.changed() => changed.is_err() || *stop.borrow(),
        _ = sleep(duration) => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    fn spawn_fake_obs(listener: TcpListener) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(socket).await.unwrap();
            websocket
                .send(Message::Text(
                    json!({"op": 0, "d": {"obsWebSocketVersion": "5.6.0", "rpcVersion": 1}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let identify: Value =
                serde_json::from_str(websocket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(identify["op"], 1);
            websocket
                .send(Message::Text(
                    json!({"op": 2, "d": {"negotiatedRpcVersion": 1}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();

            let request: Value =
                serde_json::from_str(websocket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(request["d"]["requestType"], "GetVersion");
            websocket
                .send(Message::Text(
                    json!({"op": 7, "d": {
                        "requestType": "GetVersion",
                        "requestId": request["d"]["requestId"],
                        "requestStatus": {"result": true, "code": 100},
                        "responseData": {
                            "obsVersion": "31.0.0",
                            "obsWebSocketVersion": "5.6.0",
                            "rpcVersion": 1,
                            "availableRequests": [],
                            "supportedImageFormats": [],
                            "platform": "macos",
                            "platformDescription": "macOS"
                        }
                    }})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        })
    }

    #[test]
    fn defaults_to_the_standard_obs_websocket_endpoint() {
        let configuration = ObsConfiguration::default();
        assert_eq!(configuration.host, "localhost");
        assert_eq!(configuration.port, 4455);
        assert_eq!(configuration.microphone_input_name, "Mic/Aux");
    }

    #[test]
    fn configuration_round_trips_without_external_cli_fields() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let path = temp.path().join("obs.json");
        let configuration = ObsConfiguration {
            host: "192.168.1.20".into(),
            port: 4456,
            microphone_input_name: "主播麦克风".into(),
            default_live_scene: "直播".into(),
        };
        configuration.save(&path).unwrap();
        assert_eq!(ObsConfiguration::load(&path).unwrap(), configuration);
    }

    #[test]
    fn legacy_external_cli_field_is_ignored_when_loading_configuration() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let path = temp.path().join("obs.json");
        std::fs::write(
            &path,
            r#"{
                "host": "localhost",
                "port": 4455,
                "executablePath": "/old/external-client",
                "microphoneInputName": "Mic/Aux",
                "defaultLiveScene": "直播"
            }"#,
        )
        .unwrap();
        let configuration = ObsConfiguration::load(&path).unwrap();
        assert_eq!(configuration.default_live_scene, "直播");
    }
    #[test]
    fn password_round_trips_in_private_app_file() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let configuration_path = temp.path().join("obs-control.json");

        let passwords = FileObsPasswordStore::new(configuration_path.clone(), false);
        passwords.write("localhost", 4455, "secret-value").unwrap();

        assert_eq!(
            passwords.read("localhost", 4455).unwrap().as_deref(),
            Some("secret-value"),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(configuration_path.with_file_name(OBS_PASSWORD_FILENAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
            );
        }
    }
    #[test]
    fn private_password_store_bounds_reads_and_clears_only_its_file() {
        let root = crate::test_support::private_tempdir().unwrap();
        let configuration = root.path().join("obs.json");
        let passwords = FileObsPasswordStore::new(configuration, false);
        assert!(passwords.read("localhost", 4455).unwrap().is_none());
        passwords
            .write("localhost", 4455, "synthetic-password")
            .unwrap();
        assert_eq!(
            passwords.read("localhost", 4455).unwrap().as_deref(),
            Some("synthetic-password")
        );
        let path = root.path().join(OBS_PASSWORD_FILENAME);
        crate::storage::write_private_atomic(&path, &vec![b'x'; MAX_OBS_PASSWORD as usize + 1])
            .unwrap();
        assert!(passwords.read("localhost", 4455).is_err());
        assert!(
            passwords
                .write(
                    "localhost",
                    4455,
                    &"x".repeat(MAX_OBS_PASSWORD as usize + 1)
                )
                .is_err()
        );
        passwords.write("localhost", 4455, "").unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_password_store_rejects_links_and_unsafe_permissions_without_touching_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for hard_link in [false, true] {
            let root = crate::test_support::private_tempdir().unwrap();
            let outside = crate::test_support::private_tempdir().unwrap();
            let target = outside.path().join("original");
            crate::storage::write_private_atomic(&target, b"preserve-original").unwrap();
            let path = root.path().join(OBS_PASSWORD_FILENAME);
            if hard_link {
                std::fs::hard_link(&target, &path).unwrap();
            } else {
                symlink(&target, &path).unwrap();
            }
            let passwords = FileObsPasswordStore::new(root.path().join("obs.json"), false);
            assert!(passwords.read("localhost", 4455).is_err());
            assert!(passwords.write("localhost", 4455, "replacement").is_err());
            assert!(passwords.write("localhost", 4455, "").is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"preserve-original");
        }
        let root = crate::test_support::private_tempdir().unwrap();
        let passwords = FileObsPasswordStore::new(root.path().join("obs.json"), false);
        passwords.write("localhost", 4455, "private").unwrap();
        std::fs::set_permissions(
            root.path().join(OBS_PASSWORD_FILENAME),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(passwords.read("localhost", 4455).is_err());
        assert!(passwords.write("localhost", 4455, "replacement").is_err());
        std::fs::set_permissions(
            root.path().join(OBS_PASSWORD_FILENAME),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(passwords.read("localhost", 4455).is_err());
        assert!(passwords.write("localhost", 4455, "replacement").is_err());
        assert_eq!(
            std::fs::read(root.path().join(OBS_PASSWORD_FILENAME)).unwrap(),
            b"private"
        );
    }

    #[tokio::test]
    async fn password_callback_uses_frozen_endpoint_without_holding_controller_locks() {
        struct BlockingStore {
            started: Mutex<Option<tokio::sync::oneshot::Sender<(String, u16)>>>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl ObsPasswordStore for BlockingStore {
            fn read(&self, _host: &str, _port: u16) -> Result<Option<String>> {
                Ok(None)
            }
            fn write(&self, host: &str, port: u16, _password: &str) -> Result<()> {
                self.started
                    .blocking_lock()
                    .take()
                    .unwrap()
                    .send((host.into(), port))
                    .unwrap();
                self.release.blocking_lock().recv()?;
                Ok(())
            }
        }
        let root = crate::test_support::private_tempdir().unwrap();
        let (started, observed) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let controller = ObsController::new(
            ObsConfiguration {
                host: "old-host".into(),
                port: 4456,
                ..ObsConfiguration::default()
            },
            root.path().join("obs.json"),
            Arc::new(BlockingStore {
                started: Mutex::new(Some(started)),
                release: Mutex::new(wait),
            }),
        );
        let writer = controller.clone();
        let write = tokio::spawn(async move { writer.set_password("synthetic").await });
        assert_eq!(observed.await.unwrap(), ("old-host".into(), 4456));
        timeout(
            Duration::from_secs(2),
            controller.set_host("new-host".into()),
        )
        .await
        .unwrap()
        .unwrap();
        release.send(()).unwrap();
        write.await.unwrap().unwrap();
        assert_eq!(controller.configuration().await.host, "new-host");
    }

    #[tokio::test]
    async fn password_read_does_not_lock_configuration_or_connect_a_stale_endpoint() {
        struct BlockingRead {
            started: Mutex<Option<tokio::sync::oneshot::Sender<(String, u16)>>>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
            endpoints: Mutex<Vec<(String, u16)>>,
        }
        impl ObsPasswordStore for BlockingRead {
            fn read(&self, host: &str, port: u16) -> Result<Option<String>> {
                self.endpoints.blocking_lock().push((host.into(), port));
                if let Some(started) = self.started.blocking_lock().take() {
                    started.send((host.into(), port)).unwrap();
                    self.release.blocking_lock().recv()?;
                }
                Ok(None)
            }
            fn write(&self, _host: &str, _port: u16, _password: &str) -> Result<()> {
                bail!("unexpected password write")
            }
        }
        let root = crate::test_support::private_tempdir().unwrap();
        let old_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old_port = old_listener.local_addr().unwrap().port();
        let new_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let new_port = new_listener.local_addr().unwrap().port();
        let server = spawn_fake_obs(new_listener);
        let (started, observed) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let passwords = Arc::new(BlockingRead {
            started: Mutex::new(Some(started)),
            release: Mutex::new(wait),
            endpoints: Mutex::new(Vec::new()),
        });
        let controller = ObsController::new(
            ObsConfiguration {
                host: "127.0.0.1".into(),
                port: old_port,
                ..ObsConfiguration::default()
            },
            root.path().join("obs.json"),
            passwords.clone(),
        );
        let connector = controller.clone();
        let connect = tokio::spawn(async move { connector.client().await });
        assert_eq!(observed.await.unwrap(), ("127.0.0.1".into(), old_port));
        timeout(Duration::from_secs(2), controller.set_port(new_port))
            .await
            .unwrap()
            .unwrap();
        release.send(()).unwrap();
        timeout(Duration::from_secs(2), connect)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.await.unwrap();
        assert_eq!(
            *passwords.endpoints.lock().await,
            [
                ("127.0.0.1".into(), old_port),
                ("127.0.0.1".into(), new_port)
            ]
        );
        assert!(
            timeout(Duration::from_millis(10), old_listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn injected_password_store_errors_never_fall_back_to_the_local_password_file() {
        struct UnavailableStore;
        impl ObsPasswordStore for UnavailableStore {
            fn read(&self, _host: &str, _port: u16) -> Result<Option<String>> {
                bail!("synthetic storage unavailable")
            }
            fn write(&self, _host: &str, _port: u16, _password: &str) -> Result<()> {
                bail!("synthetic storage unavailable")
            }
        }
        let root = crate::test_support::private_tempdir().unwrap();
        let path = root.path().join("obs.json");
        let file = FileObsPasswordStore::new(path.clone(), false);
        file.write("localhost", 4455, "preserve-local-password")
            .unwrap();
        let controller = ObsController::new(
            ObsConfiguration::default(),
            path,
            Arc::new(UnavailableStore),
        );
        assert!(
            controller
                .client()
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("读取 OBS 密码失败")
        );
        assert!(controller.set_password("replacement").await.is_err());
        assert_eq!(
            file.read("localhost", 4455).unwrap().as_deref(),
            Some("preserve-local-password")
        );
    }

    #[test]
    fn host_validation_matches_obws_host_and_port_contract() {
        assert_eq!(normalize_obs_host(" localhost ").unwrap(), "localhost");
        assert_eq!(normalize_obs_host("127.0.0.1").unwrap(), "127.0.0.1");
        assert_eq!(normalize_obs_host("::1").unwrap(), "[::1]");
        assert_eq!(normalize_obs_host("[::1]").unwrap(), "[::1]");
        assert!(normalize_obs_host("http://localhost").is_err());
        assert!(normalize_obs_host("localhost:4455").is_err());
        assert!(normalize_obs_host("localhost/path").is_err());
    }

    #[tokio::test]
    async fn setting_port_discards_cached_client_and_connects_new_endpoint() {
        let old_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let new_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old_port = old_listener.local_addr().unwrap().port();
        let new_port = new_listener.local_addr().unwrap().port();
        let old_server = spawn_fake_obs(old_listener);
        let new_server = spawn_fake_obs(new_listener);
        let temp = crate::test_support::private_tempdir().unwrap();
        let configuration_path = temp.path().join("obs.json");
        let controller = ObsController::new(
            ObsConfiguration {
                host: "127.0.0.1".into(),
                port: old_port,
                microphone_input_name: "主播麦克风".into(),
                default_live_scene: "直播".into(),
            },
            configuration_path.clone(),
            Arc::new(FileObsPasswordStore::new(configuration_path.clone(), false)),
        );

        controller.set_password("secret-value").await.unwrap();
        controller.client().await.unwrap();
        old_server.await.unwrap();
        controller.set_port(new_port).await.unwrap();
        controller.client().await.unwrap();
        new_server.await.unwrap();
        controller.set_host(" LOCALHOST ".into()).await.unwrap();

        let saved = ObsConfiguration::load(&configuration_path).unwrap();
        assert_eq!(saved.port, new_port);
        assert_eq!(saved.host, "localhost");
        assert_eq!(saved.microphone_input_name, "主播麦克风");
        assert_eq!(saved.default_live_scene, "直播");
        assert_eq!(
            controller
                .passwords
                .read("localhost", new_port)
                .unwrap()
                .as_deref(),
            Some("secret-value")
        );
    }

    #[tokio::test]
    async fn concurrent_endpoint_updates_do_not_lose_each_other() {
        let temp = crate::test_support::private_tempdir().unwrap();
        let configuration_path = temp.path().join("obs.json");
        let controller = ObsController::new(
            ObsConfiguration {
                microphone_input_name: "主播麦克风".into(),
                default_live_scene: "直播".into(),
                ..ObsConfiguration::default()
            },
            configuration_path.clone(),
            Arc::new(FileObsPasswordStore::new(configuration_path.clone(), false)),
        );

        let (host_result, port_result) = tokio::join!(
            controller.set_host("127.0.0.1".into()),
            controller.set_port(4456),
        );
        host_result.unwrap();
        port_result.unwrap();

        assert_eq!(
            ObsConfiguration::load(&configuration_path).unwrap(),
            ObsConfiguration {
                host: "127.0.0.1".into(),
                port: 4456,
                microphone_input_name: "主播麦克风".into(),
                default_live_scene: "直播".into(),
            }
        );
    }
    #[tokio::test]
    async fn endpoint_validation_and_save_failure_preserve_configuration_password_and_client() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = spawn_fake_obs(listener);
        let temp = crate::test_support::private_tempdir().unwrap();
        let configuration_path = temp.path().join("obs.json");
        let original = ObsConfiguration {
            host: "127.0.0.1".into(),
            port,
            microphone_input_name: "主播麦克风".into(),
            default_live_scene: "直播".into(),
        };
        original.save(&configuration_path).unwrap();
        let passwords = Arc::new(FileObsPasswordStore::new(configuration_path.clone(), false));
        passwords.write("127.0.0.1", port, "secret-value").unwrap();
        let controller = ObsController::new(
            original.clone(),
            configuration_path.clone(),
            passwords.clone(),
        );
        let cached = controller.client().await.unwrap();
        server.await.unwrap();

        assert!(
            controller
                .set_host("http://localhost".into())
                .await
                .is_err()
        );
        assert!(controller.set_port(0).await.is_err());
        assert_eq!(controller.configuration().await, original);
        assert!(Arc::ptr_eq(&cached, &controller.client().await.unwrap()));

        std::fs::remove_file(&configuration_path).unwrap();
        std::fs::create_dir(&configuration_path).unwrap();
        assert!(controller.set_host("localhost".into()).await.is_err());
        assert_eq!(controller.configuration().await, original);
        assert!(Arc::ptr_eq(&cached, &controller.client().await.unwrap()));
        assert_eq!(
            passwords.read("127.0.0.1", port).unwrap().as_deref(),
            Some("secret-value")
        );
    }
}
