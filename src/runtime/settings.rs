pub use crate::runner::settings::{Persona, ReplyActivity};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiProtocol {
    OpenaiChat,
    AnthropicMessages,
    ChatgptSubscription,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpenaiTokenLimit {
    MaxCompletionTokens,
    MaxTokens,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiSettings {
    pub protocol: ApiProtocol,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
    pub openai_token_limit: OpenaiTokenLimit,
    pub chatgpt_account: Option<String>,
}
/// Format-4 settings only; credentials use dedicated commands and are never serialized here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AssistantSettings {
    pub format: u8,
    pub api: ApiSettings,
    pub name: String,
    pub preferences: String,
    pub use_profile: bool,
    pub use_project: bool,
    pub workspace: Option<PathBuf>,
    pub automatic: bool,
    pub resume_on_start: bool,
    pub reply_activity: ReplyActivity,
    pub dynamic_host_priority: bool,
    pub dynamic_muted_support: bool,
    pub thank_gifts: bool,
    pub thank_likes: bool,
    pub thank_follows: bool,
    pub thank_shares: bool,
    pub mention_sender: bool,
    pub repair_blocked: bool,
    pub web_search: bool,
    pub blocked_words: Vec<String>,
    pub skills: Vec<String>,
    pub persona: Persona,
}
impl AssistantSettings {
    pub(crate) fn from_core(value: &crate::runner::settings::Settings) -> Result<Self> {
        Ok(serde_json::from_value(serde_json::to_value(value)?)?)
    }
    pub(crate) fn into_core(self) -> Result<crate::runner::settings::Settings> {
        let value: crate::runner::settings::Settings =
            serde_json::from_value(serde_json::to_value(self)?)?;
        value.validate()?;
        Ok(value)
    }
}
use super::*;
use crate::obs::ObsConfiguration;
use anyhow::Context;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalField {
    ObsHost,
    ObsPort,
    ObsMicrophone,
    ObsScene,
    RoomTitle,
    RoomCover,
}

impl ExternalField {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ObsHost => "接入地址",
            Self::ObsPort => "接入端口",
            Self::ObsMicrophone => "麦克风输入",
            Self::ObsScene => "当前场景",
            Self::RoomTitle => "直播间标题",
            Self::RoomCover => "直播间封面",
        }
    }

    pub(crate) fn is_obs(self) -> bool {
        !matches!(self, Self::RoomTitle | Self::RoomCover)
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::ObsHost => {
                "OBS WebSocket 主机名或 IP，不含协议、路径和端口。保存后下次连接使用新地址。"
            }
            Self::ObsPort => "OBS WebSocket 端口，1–65535；通常为4455。保存不会开始或停止推流。",
            Self::ObsMicrophone => "填写 OBS 中的输入名称；保存前验证输入存在。",
            Self::ObsScene => "填写 OBS 中的场景名称，Enter 立即切换当前节目场景。",
            Self::RoomTitle => {
                "公开修改当前B站直播间标题；仅主账号本人直播间可修改，与AI本场主题相互独立。结果以B站回执和审核为准。"
            }
            Self::RoomCover => {
                "填写本地图片完整路径，Enter 上传并提交直播间封面。仅主账号本人直播间可修改；提交审核不等于已公开生效。"
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct SavedSetting {
    pub message: String,
    pub level: NoticeLevel,
    pub obs_configuration: Option<ObsConfiguration>,
}

fn room_receipt(receipt: crate::bilibili::RoomUpdateReceipt) -> Result<(String, NoticeLevel)> {
    use crate::bilibili::RoomUpdateReceipt;
    let level = match receipt {
        RoomUpdateReceipt::Applied { .. } => NoticeLevel::Success,
        RoomUpdateReceipt::Pending { .. } => NoticeLevel::Info,
        RoomUpdateReceipt::Rejected { .. } | RoomUpdateReceipt::Unknown { .. } => {
            anyhow::bail!("{receipt}")
        }
    };
    Ok((receipt.to_string(), level))
}

impl RuntimeState {
    pub(crate) fn start_external_setting(
        &mut self,
        field: ExternalField,
        value: String,
        tx: mpsc::Sender<CoreEvent>,
    ) -> Result<uuid::Uuid> {
        anyhow::ensure!(
            self.mode != RuntimeMode::Replay
                && if field.is_obs() {
                    self.obs_allowed
                } else {
                    self.accounts_allowed()
                },
            "本地与回放模式禁止真实账号与OBS操作"
        );
        anyhow::ensure!(
            self.settings_operation.is_none(),
            "上一设置仍在执行，请等待回执"
        );
        let value = value.trim().to_owned();
        anyhow::ensure!(!value.is_empty(), "{}不能为空", field.label());
        let account = if field.is_obs() {
            None
        } else {
            anyhow::ensure!(
                matches!(self.account_status, AccountStatus::SignedIn { .. }),
                "请先登录直播间所属B站主账号"
            );
            Some(self.account.snapshot()?)
        };
        let room_id = self.session.room_id.clone();
        let obs = self.obs.clone();
        let token = uuid::Uuid::new_v4();
        self.settings_operation = Some(token);
        self.set_notice(format!("正在提交{}…", field.label()), NoticeLevel::Progress);
        self.tasks.spawn(async move {
            let result: Result<SavedSetting> = async {
                let (message, level) = match field {
                    ExternalField::ObsHost => {
                        obs.set_host(value).await?;
                        (
                            "OBS接入地址已保存，连接将重新建立".into(),
                            NoticeLevel::Success,
                        )
                    }
                    ExternalField::ObsPort => {
                        let port: u16 = value.parse().context("端口必须是1–65535的整数")?;
                        obs.set_port(port).await?;
                        (
                            "OBS接入端口已保存，连接将重新建立".into(),
                            NoticeLevel::Success,
                        )
                    }
                    ExternalField::ObsMicrophone => {
                        obs.set_microphone_name(value).await?;
                        ("OBS麦克风输入已保存".into(), NoticeLevel::Success)
                    }
                    ExternalField::ObsScene => {
                        obs.switch_scene(&value).await?;
                        (format!("已切换OBS场景：{value}"), NoticeLevel::Success)
                    }
                    ExternalField::RoomTitle => room_receipt(
                        account
                            .as_ref()
                            .expect("room account pinned")
                            .update_room_title(&room_id, &value)
                            .await?,
                    )?,
                    ExternalField::RoomCover => room_receipt(
                        account
                            .as_ref()
                            .expect("room account pinned")
                            .update_room_cover(&room_id, std::path::Path::new(&value))
                            .await?,
                    )?,
                };
                let obs_configuration = if field.is_obs() {
                    Some(obs.configuration().await)
                } else {
                    None
                };
                Ok(SavedSetting {
                    message,
                    level,
                    obs_configuration,
                })
            }
            .await;
            let _ = tx
                .send(CoreEvent::SettingSaved {
                    token,
                    result: result.map_err(|e| e.to_string()),
                })
                .await;
        });
        Ok(token)
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub assistant: AssistantSettings,
    pub obs: crate::obs::ObsConfiguration,
    pub resolved_workspace: PathBuf,
    pub api_key: ApiKeyStatus,
}
#[derive(Debug, Clone)]
pub enum ApiKeyStatus {
    Set,
    Unset,
    Error { reason: String },
}

impl RuntimeState {
    pub(crate) fn finish_setting_save(
        &mut self,
        token: uuid::Uuid,
        result: std::result::Result<SavedSetting, String>,
    ) {
        if self.settings_operation != Some(token) {
            return;
        }
        self.settings_operation = None;
        #[cfg(feature = "tui")]
        self.project_setting_save(token, &result);
        match result {
            Ok(saved) => self.set_notice(saved.message, saved.level),
            Err(error) => self.set_notice(format!("保存失败：{error}"), NoticeLevel::Error),
        }
    }
}
