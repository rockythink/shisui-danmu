mod meter;

pub use meter::MicrophoneLevel;

use crate::storage::OBS_KEYRING_SERVICE;
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use meter::{LevelSmoother, SILENCE_DB, peak_db};
use obws::{Client, client::ConnectConfig, events::Event, requests::EventSubscription};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
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
}

#[derive(Clone)]
pub struct ObsController {
    state: Arc<Mutex<ObsConnectionState>>,
    connect_lock: Arc<Mutex<()>>,
    configuration_path: PathBuf,
}

impl ObsController {
    pub fn new(configuration: ObsConfiguration, configuration_path: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(ObsConnectionState {
                configuration,
                client: None,
            })),
            configuration_path,
            connect_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn update_configuration(&self, configuration: ObsConfiguration) -> Result<()> {
        configuration.save(&self.configuration_path)?;
        let mut state = self.state.lock().await;
        state.configuration = configuration;
        state.client = None;
        Ok(())
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
        let mut configuration = self.configuration().await;
        configuration.microphone_input_name = name;
        self.update_configuration(configuration).await
    }

    pub fn set_password(password: &str) -> Result<()> {
        let entry = keyring::Entry::new(OBS_KEYRING_SERVICE, "default")?;
        if password.is_empty() {
            let _ = entry.delete_credential();
        } else {
            entry.set_password(password)?;
        }
        Ok(())
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

    async fn configuration(&self) -> ObsConfiguration {
        self.state.lock().await.configuration.clone()
    }

    async fn client(&self) -> Result<Arc<Client>> {
        {
            let state = self.state.lock().await;
            if let Some(client) = &state.client {
                return Ok(Arc::clone(client));
            }
        }
        let _connect_guard = self.connect_lock.lock().await;
        loop {
            let configuration = {
                let state = self.state.lock().await;
                if let Some(client) = &state.client {
                    return Ok(Arc::clone(client));
                }
                state.configuration.clone()
            };
            let password = keyring::Entry::new(OBS_KEYRING_SERVICE, "default")
                .ok()
                .and_then(|entry| entry.get_password().ok())
                .or_else(|| std::env::var("OBS_API_PASSWORD").ok());
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
                    format!(
                        "无法连接 OBS WebSocket {}:{}",
                        configuration.host, configuration.port
                    )
                })?;
            let client = Arc::new(client);
            let mut state = self.state.lock().await;
            if state.configuration != configuration {
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

async fn wait_for_stop(stop: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    tokio::select! {
        changed = stop.changed() => changed.is_err() || *stop.borrow(),
        _ = sleep(duration) => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_standard_obs_websocket_endpoint() {
        let configuration = ObsConfiguration::default();
        assert_eq!(configuration.host, "localhost");
        assert_eq!(configuration.port, 4455);
        assert_eq!(configuration.microphone_input_name, "Mic/Aux");
    }

    #[test]
    fn configuration_round_trips_without_external_cli_fields() {
        let temp = tempfile::tempdir().unwrap();
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
        let temp = tempfile::tempdir().unwrap();
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
}
