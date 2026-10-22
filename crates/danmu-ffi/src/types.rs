// SPDX-License-Identifier: MPL-2.0
//! Typed desktop DTOs. No credentials or platform JSON cross the production boundary.
use crate::{CoreError, absolute_path, parse_time, parse_uuid, path_string};
use shisui_danmu::{bilibili, bridge, delivery, domain, obs, runtime as rt};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopMode {
    Live,
    Local,
    Replay,
}

impl From<rt::RuntimeMode> for DesktopMode {
    fn from(value: rt::RuntimeMode) -> Self {
        match value {
            rt::RuntimeMode::Live => Self::Live,
            rt::RuntimeMode::Local => Self::Local,
            rt::RuntimeMode::Replay => Self::Replay,
        }
    }
}

impl From<DesktopMode> for rt::RuntimeMode {
    fn from(value: DesktopMode) -> Self {
        match value {
            DesktopMode::Live => Self::Live,
            DesktopMode::Local => Self::Local,
            DesktopMode::Replay => Self::Replay,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopIdentity {
    Main,
    Assistant,
}

impl From<rt::Identity> for DesktopIdentity {
    fn from(value: rt::Identity) -> Self {
        match value {
            rt::Identity::Main => Self::Main,
            rt::Identity::Assistant => Self::Assistant,
        }
    }
}

impl From<DesktopIdentity> for rt::Identity {
    fn from(value: DesktopIdentity) -> Self {
        match value {
            DesktopIdentity::Main => Self::Main,
            DesktopIdentity::Assistant => Self::Assistant,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopReplyPreset {
    Quiet,
    TextQa,
    Thanks,
}

impl From<rt::ReplyPreset> for DesktopReplyPreset {
    fn from(value: rt::ReplyPreset) -> Self {
        match value {
            rt::ReplyPreset::Quiet => Self::Quiet,
            rt::ReplyPreset::TextQa => Self::TextQa,
            rt::ReplyPreset::Thanks => Self::Thanks,
        }
    }
}

impl From<DesktopReplyPreset> for rt::ReplyPreset {
    fn from(value: DesktopReplyPreset) -> Self {
        match value {
            DesktopReplyPreset::Quiet => Self::Quiet,
            DesktopReplyPreset::TextQa => Self::TextQa,
            DesktopReplyPreset::Thanks => Self::Thanks,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopReplyActivity {
    Cautious,
    Balanced,
    Active,
}

impl From<rt::ReplyActivity> for DesktopReplyActivity {
    fn from(value: rt::ReplyActivity) -> Self {
        match value {
            rt::ReplyActivity::Cautious => Self::Cautious,
            rt::ReplyActivity::Balanced => Self::Balanced,
            rt::ReplyActivity::Active => Self::Active,
        }
    }
}

impl From<DesktopReplyActivity> for rt::ReplyActivity {
    fn from(value: DesktopReplyActivity) -> Self {
        match value {
            DesktopReplyActivity::Cautious => Self::Cautious,
            DesktopReplyActivity::Balanced => Self::Balanced,
            DesktopReplyActivity::Active => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopExportFormat {
    Markdown,
    Json,
}

impl From<rt::ExportFormat> for DesktopExportFormat {
    fn from(value: rt::ExportFormat) -> Self {
        match value {
            rt::ExportFormat::Markdown => Self::Markdown,
            rt::ExportFormat::Json => Self::Json,
        }
    }
}

impl From<DesktopExportFormat> for rt::ExportFormat {
    fn from(value: DesktopExportFormat) -> Self {
        match value {
            DesktopExportFormat::Markdown => Self::Markdown,
            DesktopExportFormat::Json => Self::Json,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EventKind {
    Danmu,
    Gift,
    GuardEvent,
    Superchat,
    Enter,
    Like,
    Follow,
    Share,
    Pk,
    Lottery,
    Moderation,
    RoomStatus,
    System,
}

impl From<domain::DanmuEventKind> for EventKind {
    fn from(value: domain::DanmuEventKind) -> Self {
        match value {
            domain::DanmuEventKind::Danmu => Self::Danmu,
            domain::DanmuEventKind::Gift => Self::Gift,
            domain::DanmuEventKind::GuardEvent => Self::GuardEvent,
            domain::DanmuEventKind::Superchat => Self::Superchat,
            domain::DanmuEventKind::Enter => Self::Enter,
            domain::DanmuEventKind::Like => Self::Like,
            domain::DanmuEventKind::Follow => Self::Follow,
            domain::DanmuEventKind::Share => Self::Share,
            domain::DanmuEventKind::Pk => Self::Pk,
            domain::DanmuEventKind::Lottery => Self::Lottery,
            domain::DanmuEventKind::Moderation => Self::Moderation,
            domain::DanmuEventKind::RoomStatus => Self::RoomStatus,
            domain::DanmuEventKind::System => Self::System,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EventOrigin {
    Live,
    History,
    Archived,
}

impl From<domain::DanmuEventOrigin> for EventOrigin {
    fn from(value: domain::DanmuEventOrigin) -> Self {
        match value {
            domain::DanmuEventOrigin::Live => Self::Live,
            domain::DanmuEventOrigin::History => Self::History,
            domain::DanmuEventOrigin::Archived => Self::Archived,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SessionStatus {
    Active,
    Interrupted,
    Ended,
}

impl From<domain::DanmuSessionStatus> for SessionStatus {
    fn from(value: domain::DanmuSessionStatus) -> Self {
        match value {
            domain::DanmuSessionStatus::Active => Self::Active,
            domain::DanmuSessionStatus::Interrupted => Self::Interrupted,
            domain::DanmuSessionStatus::Ended => Self::Ended,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SessionEndReason {
    Completed,
    Replaced,
}

impl From<domain::DanmuSessionEndReason> for SessionEndReason {
    fn from(value: domain::DanmuSessionEndReason) -> Self {
        match value {
            domain::DanmuSessionEndReason::Completed => Self::Completed,
            domain::DanmuSessionEndReason::Replaced => Self::Replaced,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RoomLiveStatus {
    Offline,
    Live,
    Rotating,
}

impl From<bilibili::RoomLiveStatus> for RoomLiveStatus {
    fn from(value: bilibili::RoomLiveStatus) -> Self {
        match value {
            bilibili::RoomLiveStatus::Offline => Self::Offline,
            bilibili::RoomLiveStatus::Live => Self::Live,
            bilibili::RoomLiveStatus::Rotating => Self::Rotating,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum StreamState {
    Unknown,
    Stopped,
    Starting,
    Live,
    Stopping,
}

impl From<obs::StreamState> for StreamState {
    fn from(value: obs::StreamState) -> Self {
        match value {
            obs::StreamState::Unknown => Self::Unknown,
            obs::StreamState::Stopped => Self::Stopped,
            obs::StreamState::Starting => Self::Starting,
            obs::StreamState::Live => Self::Live,
            obs::StreamState::Stopping => Self::Stopping,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MicrophoneState {
    Unknown,
    Unmuted,
    Muted,
}

impl From<obs::MicrophoneState> for MicrophoneState {
    fn from(value: obs::MicrophoneState) -> Self {
        match value {
            obs::MicrophoneState::Unknown => Self::Unknown,
            obs::MicrophoneState::Unmuted => Self::Unmuted,
            obs::MicrophoneState::Muted => Self::Muted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DeliveryOutcome {
    Confirmed,
    Rejected,
    Uncertain,
    Cancelled,
}

impl From<delivery::Outcome> for DeliveryOutcome {
    fn from(value: delivery::Outcome) -> Self {
        match value {
            delivery::Outcome::Confirmed => Self::Confirmed,
            delivery::Outcome::Rejected => Self::Rejected,
            delivery::Outcome::Uncertain => Self::Uncertain,
            delivery::Outcome::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DeliveryCause {
    EchoConfirmed,
    EchoMissing,
    ContentRejected,
    OtherRejected,
    TransportUncertain,
    KnownWord,
    Cancelled,
}

impl From<delivery::Cause> for DeliveryCause {
    fn from(value: delivery::Cause) -> Self {
        match value {
            delivery::Cause::EchoConfirmed => Self::EchoConfirmed,
            delivery::Cause::EchoMissing => Self::EchoMissing,
            delivery::Cause::ContentRejected => Self::ContentRejected,
            delivery::Cause::OtherRejected => Self::OtherRejected,
            delivery::Cause::TransportUncertain => Self::TransportUncertain,
            delivery::Cause::KnownWord => Self::KnownWord,
            delivery::Cause::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CandidateState {
    AwaitingApproval,
    Accepted,
    Sending,
    Confirmed,
    Uncertain,
    Rejected,
    Cancelled,
}

impl From<bridge::Execution> for CandidateState {
    fn from(value: bridge::Execution) -> Self {
        match value {
            bridge::Execution::AwaitingApproval => Self::AwaitingApproval,
            bridge::Execution::Accepted => Self::Accepted,
            bridge::Execution::Sending => Self::Sending,
            bridge::Execution::Confirmed => Self::Confirmed,
            bridge::Execution::Uncertain => Self::Uncertain,
            bridge::Execution::Rejected => Self::Rejected,
            bridge::Execution::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopApiProtocol {
    OpenaiChat,
    AnthropicMessages,
    ChatgptSubscription,
}

impl From<rt::settings::ApiProtocol> for DesktopApiProtocol {
    fn from(value: rt::settings::ApiProtocol) -> Self {
        match value {
            rt::settings::ApiProtocol::OpenaiChat => Self::OpenaiChat,
            rt::settings::ApiProtocol::AnthropicMessages => Self::AnthropicMessages,
            rt::settings::ApiProtocol::ChatgptSubscription => Self::ChatgptSubscription,
        }
    }
}

impl From<DesktopApiProtocol> for rt::settings::ApiProtocol {
    fn from(value: DesktopApiProtocol) -> Self {
        match value {
            DesktopApiProtocol::OpenaiChat => Self::OpenaiChat,
            DesktopApiProtocol::AnthropicMessages => Self::AnthropicMessages,
            DesktopApiProtocol::ChatgptSubscription => Self::ChatgptSubscription,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopOpenaiTokenLimit {
    MaxCompletionTokens,
    MaxTokens,
}

impl From<rt::settings::OpenaiTokenLimit> for DesktopOpenaiTokenLimit {
    fn from(value: rt::settings::OpenaiTokenLimit) -> Self {
        match value {
            rt::settings::OpenaiTokenLimit::MaxCompletionTokens => Self::MaxCompletionTokens,
            rt::settings::OpenaiTokenLimit::MaxTokens => Self::MaxTokens,
        }
    }
}

impl From<DesktopOpenaiTokenLimit> for rt::settings::OpenaiTokenLimit {
    fn from(value: DesktopOpenaiTokenLimit) -> Self {
        match value {
            DesktopOpenaiTokenLimit::MaxCompletionTokens => Self::MaxCompletionTokens,
            DesktopOpenaiTokenLimit::MaxTokens => Self::MaxTokens,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopPersona {
    Assistant,
    Broadcaster,
}

impl From<rt::settings::Persona> for DesktopPersona {
    fn from(value: rt::settings::Persona) -> Self {
        match value {
            rt::settings::Persona::Assistant => Self::Assistant,
            rt::settings::Persona::Broadcaster => Self::Broadcaster,
        }
    }
}

impl From<DesktopPersona> for rt::settings::Persona {
    fn from(value: DesktopPersona) -> Self {
        match value {
            DesktopPersona::Assistant => Self::Assistant,
            DesktopPersona::Broadcaster => Self::Broadcaster,
        }
    }
}

impl From<rt::settings::ReplyActivity> for DesktopReplyActivity {
    fn from(value: rt::settings::ReplyActivity) -> Self {
        match value {
            rt::settings::ReplyActivity::Cautious => Self::Cautious,
            rt::settings::ReplyActivity::Balanced => Self::Balanced,
            rt::settings::ReplyActivity::Active => Self::Active,
        }
    }
}
impl From<DesktopReplyActivity> for rt::settings::ReplyActivity {
    fn from(value: DesktopReplyActivity) -> Self {
        match value {
            DesktopReplyActivity::Cautious => Self::Cautious,
            DesktopReplyActivity::Balanced => Self::Balanced,
            DesktopReplyActivity::Active => Self::Active,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopOptions {
    pub support_root: String,
    pub config_path: String,
    pub account_path: String,
    pub obs_config_path: String,
    pub sessions_root: String,
    pub workspace_default: String,
    pub external_instance: Option<String>,
    pub mode: DesktopMode,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopApiSettings {
    pub protocol: DesktopApiProtocol,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
    pub openai_token_limit: DesktopOpenaiTokenLimit,
    pub chatgpt_account: Option<String>,
}

impl From<rt::settings::ApiSettings> for DesktopApiSettings {
    fn from(value: rt::settings::ApiSettings) -> Self {
        let rt::settings::ApiSettings {
            protocol,
            base_url,
            model,
            max_output_tokens,
            openai_token_limit,
            chatgpt_account,
        } = value;
        Self {
            protocol: protocol.into(),
            base_url,
            model,
            max_output_tokens,
            openai_token_limit: openai_token_limit.into(),
            chatgpt_account,
        }
    }
}

impl From<DesktopApiSettings> for rt::settings::ApiSettings {
    fn from(value: DesktopApiSettings) -> Self {
        let DesktopApiSettings {
            protocol,
            base_url,
            model,
            max_output_tokens,
            openai_token_limit,
            chatgpt_account,
        } = value;
        Self {
            protocol: protocol.into(),
            base_url,
            model,
            max_output_tokens,
            openai_token_limit: openai_token_limit.into(),
            chatgpt_account,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopAssistantSettings {
    pub format: u8,
    pub api: DesktopApiSettings,
    pub name: String,
    pub preferences: String,
    pub use_profile: bool,
    pub use_project: bool,
    pub workspace: Option<String>,
    pub automatic: bool,
    pub resume_on_start: bool,
    pub reply_activity: DesktopReplyActivity,
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
    pub persona: DesktopPersona,
}

impl TryFrom<rt::settings::AssistantSettings> for DesktopAssistantSettings {
    type Error = CoreError;
    fn try_from(value: rt::settings::AssistantSettings) -> Result<Self, CoreError> {
        let rt::settings::AssistantSettings {
            format,
            api,
            name,
            preferences,
            use_profile,
            use_project,
            workspace,
            automatic,
            resume_on_start,
            reply_activity,
            dynamic_host_priority,
            dynamic_muted_support,
            thank_gifts,
            thank_likes,
            thank_follows,
            thank_shares,
            mention_sender,
            repair_blocked,
            web_search,
            blocked_words,
            skills,
            persona,
        } = value;
        Ok(Self {
            format,
            api: api.into(),
            name,
            preferences,
            use_profile,
            use_project,
            workspace: workspace.map(path_string).transpose()?,
            automatic,
            resume_on_start,
            reply_activity: reply_activity.into(),
            dynamic_host_priority,
            dynamic_muted_support,
            thank_gifts,
            thank_likes,
            thank_follows,
            thank_shares,
            mention_sender,
            repair_blocked,
            web_search,
            blocked_words,
            skills,
            persona: persona.into(),
        })
    }
}

impl TryFrom<DesktopAssistantSettings> for rt::settings::AssistantSettings {
    type Error = CoreError;
    fn try_from(value: DesktopAssistantSettings) -> Result<Self, CoreError> {
        let DesktopAssistantSettings {
            format,
            api,
            name,
            preferences,
            use_profile,
            use_project,
            workspace,
            automatic,
            resume_on_start,
            reply_activity,
            dynamic_host_priority,
            dynamic_muted_support,
            thank_gifts,
            thank_likes,
            thank_follows,
            thank_shares,
            mention_sender,
            repair_blocked,
            web_search,
            blocked_words,
            skills,
            persona,
        } = value;
        Ok(Self {
            format,
            api: api.into(),
            name,
            preferences,
            use_profile,
            use_project,
            workspace: workspace
                .map(|p| absolute_path(p, "assistant workspace"))
                .transpose()?,
            automatic,
            resume_on_start,
            reply_activity: reply_activity.into(),
            dynamic_host_priority,
            dynamic_muted_support,
            thank_gifts,
            thank_likes,
            thank_follows,
            thank_shares,
            mention_sender,
            repair_blocked,
            web_search,
            blocked_words,
            skills,
            persona: persona.into(),
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopObsConfiguration {
    pub host: String,
    pub port: u16,
    pub microphone_input_name: String,
    pub default_live_scene: String,
}

impl From<obs::ObsConfiguration> for DesktopObsConfiguration {
    fn from(value: obs::ObsConfiguration) -> Self {
        let obs::ObsConfiguration {
            host,
            port,
            microphone_input_name,
            default_live_scene,
        } = value;
        Self {
            host,
            port,
            microphone_input_name,
            default_live_scene,
        }
    }
}

impl From<DesktopObsConfiguration> for obs::ObsConfiguration {
    fn from(value: DesktopObsConfiguration) -> Self {
        let DesktopObsConfiguration {
            host,
            port,
            microphone_input_name,
            default_live_scene,
        } = value;
        Self {
            host,
            port,
            microphone_input_name,
            default_live_scene,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopApiKeyStatus {
    Set,

    Unset,

    Error { reason: String },
}

impl From<rt::settings::ApiKeyStatus> for DesktopApiKeyStatus {
    fn from(value: rt::settings::ApiKeyStatus) -> Self {
        match value {
            rt::settings::ApiKeyStatus::Set => Self::Set,

            rt::settings::ApiKeyStatus::Unset => Self::Unset,

            rt::settings::ApiKeyStatus::Error { reason } => Self::Error { reason },
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopSettings {
    pub assistant: DesktopAssistantSettings,
    pub obs: DesktopObsConfiguration,
    pub resolved_workspace: String,
    pub api_key: DesktopApiKeyStatus,
}

impl TryFrom<rt::settings::RuntimeSettings> for DesktopSettings {
    type Error = CoreError;
    fn try_from(value: rt::settings::RuntimeSettings) -> Result<Self, CoreError> {
        let rt::settings::RuntimeSettings {
            assistant,
            obs,
            resolved_workspace,
            api_key,
        } = value;
        Ok(Self {
            assistant: assistant.try_into()?,
            obs: obs.into(),
            resolved_workspace: path_string(resolved_workspace)?,
            api_key: api_key.into(),
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EventEmote {
    pub text: String,
    pub fallback: String,
    pub image_url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub is_animated: bool,
}

impl From<domain::DanmuEmote> for EventEmote {
    fn from(value: domain::DanmuEmote) -> Self {
        let domain::DanmuEmote {
            text,
            fallback,
            image_url,
            width,
            height,
            is_animated,
        } = value;
        Self {
            text,
            fallback,
            image_url: image_url.into(),
            width,
            height,
            is_animated,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct BlindGift {
    pub name: String,
    pub reveal_action: String,
}

impl From<domain::BlindGift> for BlindGift {
    fn from(value: domain::BlindGift) -> Self {
        let domain::BlindGift {
            name,
            reveal_action,
        } = value;
        Self {
            name,
            reveal_action,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GiftDetails {
    pub name: String,
    pub action: String,
    pub blind_gift: Option<BlindGift>,
    pub receipts: HashMap<String, u64>,
    pub reported_quantity: u64,
}

impl From<domain::GiftDetails> for GiftDetails {
    fn from(value: domain::GiftDetails) -> Self {
        let domain::GiftDetails {
            name,
            action,
            blind_gift,
            receipts,
            reported_quantity,
        } = value;
        Self {
            name,
            action,
            blind_gift: blind_gift.map(Into::into),
            receipts,
            reported_quantity,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EventData {
    pub id: String,
    pub kind: EventKind,
    pub timestamp: i64,
    pub username: Option<String>,
    pub author_id: Option<String>,
    pub content: String,
    pub reply_to: Option<String>,
    pub origin: EventOrigin,
    pub platform_event_id: Option<String>,
    pub emotes: Vec<EventEmote>,
    pub gift: Option<GiftDetails>,
}

impl From<domain::DanmuEvent> for EventData {
    fn from(value: domain::DanmuEvent) -> Self {
        let domain::DanmuEvent {
            id,
            kind,
            timestamp,
            username,
            author_id,
            content,
            reply_to,
            origin,
            platform_event_id,
            emotes,
            gift,
        } = value;
        Self {
            id,
            kind: kind.into(),
            timestamp: timestamp.timestamp_millis(),
            username,
            author_id,
            content,
            reply_to,
            origin: origin.into(),
            platform_event_id,
            emotes: emotes.into_iter().map(Into::into).collect(),
            gift: gift.map(|v| (*v).into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MessageAssistantState {
    Processing,
    Confirmed,
    Finished,
    Failed,
}

impl From<bridge::Mark> for MessageAssistantState {
    fn from(value: bridge::Mark) -> Self {
        match value {
            bridge::Mark::Processing => Self::Processing,
            bridge::Mark::Confirmed => Self::Confirmed,
            bridge::Mark::Finished => Self::Finished,
            bridge::Mark::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EventRecord {
    pub message_key: u64,
    pub event: EventData,
    pub assistant_state: Option<MessageAssistantState>,
}

impl From<rt::EventRecord> for EventRecord {
    fn from(value: rt::EventRecord) -> Self {
        let rt::EventRecord {
            message_key,
            event,
            assistant_state,
        } = value;
        Self {
            message_key,
            event: event.into(),
            assistant_state: assistant_state.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EventCount {
    pub kind: EventKind,
    pub count: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SessionMetrics {
    pub total_event_count: u64,
    pub event_counts: Vec<EventCount>,
}

impl From<domain::SessionMetrics> for SessionMetrics {
    fn from(value: domain::SessionMetrics) -> Self {
        let domain::SessionMetrics {
            total_event_count,
            event_counts,
        } = value;
        Self {
            total_event_count,
            event_counts: {
                let mut counts: Vec<_> = event_counts
                    .into_iter()
                    .map(|(kind, count)| EventCount {
                        kind: kind.into(),
                        count,
                    })
                    .collect();
                counts.sort_by_key(|entry| entry.kind as u8);
                counts
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SessionRecord {
    pub id: String,
    pub room_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: SessionStatus,
    pub end_reason: Option<SessionEndReason>,
    pub recent_events: Vec<EventData>,
    pub metrics: SessionMetrics,
    pub featured_event: Option<EventData>,
    pub event_limit: u64,
}

impl From<domain::DanmuSession> for SessionRecord {
    fn from(value: domain::DanmuSession) -> Self {
        Self {
            id: value.id,
            room_id: value.room_id,
            started_at: value.started_at.timestamp_millis(),
            ended_at: value.ended_at.map(|t| t.timestamp_millis()),
            status: value.status.into(),
            end_reason: value.end_reason.map(Into::into),
            recent_events: value.recent_events.into_iter().map(Into::into).collect(),
            metrics: value.metrics.into(),
            featured_event: value.featured_event.map(Into::into),
            event_limit: value.event_limit as u64,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct RoomRecord {
    pub room_id: String,
    pub broadcaster_id: String,
    pub broadcaster_name: String,
    pub title: String,
    pub area: String,
    pub live_started_at: Option<i64>,
    pub live_status: RoomLiveStatus,
}

impl From<bilibili::RoomSnapshot> for RoomRecord {
    fn from(value: bilibili::RoomSnapshot) -> Self {
        let bilibili::RoomSnapshot {
            room_id,
            broadcaster_id,
            broadcaster_name,
            title,
            area,
            live_started_at,
            live_status,
        } = value;
        Self {
            room_id,
            broadcaster_id,
            broadcaster_name,
            title,
            area,
            live_started_at: live_started_at.map(|t| t.timestamp_millis()),
            live_status: live_status.into(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum AccountStatus {
    SignedOut,

    SignedIn {
        display_name: String,
        user_id: String,
    },
}

impl From<bilibili::AccountStatus> for AccountStatus {
    fn from(value: bilibili::AccountStatus) -> Self {
        match value {
            bilibili::AccountStatus::SignedOut => Self::SignedOut,

            bilibili::AccountStatus::SignedIn {
                display_name,
                user_id,
            } => Self::SignedIn {
                display_name,
                user_id,
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
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

impl From<bilibili::RoomUpdateReceipt> for RoomUpdateReceipt {
    fn from(value: bilibili::RoomUpdateReceipt) -> Self {
        match value {
            bilibili::RoomUpdateReceipt::Applied { canonical_room_id } => {
                Self::Applied { canonical_room_id }
            }

            bilibili::RoomUpdateReceipt::Pending {
                canonical_room_id,
                reason,
            } => Self::Pending {
                canonical_room_id,
                reason,
            },

            bilibili::RoomUpdateReceipt::Rejected {
                canonical_room_id,
                reason,
            } => Self::Rejected {
                canonical_room_id,
                reason,
            },

            bilibili::RoomUpdateReceipt::Unknown {
                canonical_room_id,
                status,
                reason,
            } => Self::Unknown {
                canonical_room_id,
                status,
                reason,
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ObsStatus {
    pub current_scene: String,
    pub stream: StreamState,
    pub microphone: MicrophoneState,
    pub compatibility_warning: Option<String>,
}

impl From<obs::ObsStatus> for ObsStatus {
    fn from(value: obs::ObsStatus) -> Self {
        let obs::ObsStatus {
            current_scene,
            stream,
            microphone,
            compatibility_warning,
        } = value;
        Self {
            current_scene,
            stream: stream.into(),
            microphone: microphone.into(),
            compatibility_warning,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MicrophoneLevel {
    pub peak_db: f32,
}

impl From<obs::MicrophoneLevel> for MicrophoneLevel {
    fn from(value: obs::MicrophoneLevel) -> Self {
        let obs::MicrophoneLevel { peak_db } = value;
        Self { peak_db }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DeliveryDiagnosis {
    pub cause: DeliveryCause,
    pub detail: String,
    pub response_received: bool,
    pub acceptance_proof: bool,
    pub platform_code: Option<i64>,
    pub suspected_block: bool,
    pub duplicate_risk: bool,
}

impl From<delivery::Diagnosis> for DeliveryDiagnosis {
    fn from(value: delivery::Diagnosis) -> Self {
        let delivery::Diagnosis {
            cause,
            detail,
            response_received,
            acceptance_proof,
            platform_code,
            suspected_block,
            duplicate_risk,
        } = value;
        Self {
            cause: cause.into(),
            detail,
            response_received,
            acceptance_proof,
            platform_code,
            suspected_block,
            duplicate_risk,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DeliveryRecord {
    pub outcome: DeliveryOutcome,
    pub diagnosis: DeliveryDiagnosis,
    pub confirmed_segments: Vec<String>,
    pub unconfirmed_segments: Vec<String>,
}

impl From<delivery::Delivery> for DeliveryRecord {
    fn from(value: delivery::Delivery) -> Self {
        let delivery::Delivery {
            outcome,
            diagnosis,
            confirmed_segments,
            unconfirmed_segments,
        } = value;
        Self {
            outcome: outcome.into(),
            diagnosis: diagnosis.into(),
            confirmed_segments,
            unconfirmed_segments,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ReviewedCandidate {
    pub session: String,
    pub caller: String,
    pub request_id: String,
    pub revision: u64,
    pub identity_generation: u64,
}

impl From<rt::ReviewedCandidate> for ReviewedCandidate {
    fn from(value: rt::ReviewedCandidate) -> Self {
        let rt::ReviewedCandidate {
            session,
            caller,
            request_id,
            revision,
            identity_generation,
        } = value;
        Self {
            session,
            caller,
            request_id,
            revision,
            identity_generation,
        }
    }
}

impl From<ReviewedCandidate> for rt::ReviewedCandidate {
    fn from(value: ReviewedCandidate) -> Self {
        let ReviewedCandidate {
            session,
            caller,
            request_id,
            revision,
            identity_generation,
        } = value;
        Self {
            session,
            caller,
            request_id,
            revision,
            identity_generation,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ReplyTargetRecord {
    pub user_id: String,
    pub username: String,
}

impl From<rt::ReplyTargetRecord> for ReplyTargetRecord {
    fn from(value: rt::ReplyTargetRecord) -> Self {
        let rt::ReplyTargetRecord { user_id, username } = value;
        Self { user_id, username }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct CandidateRecord {
    pub expected: ReviewedCandidate,
    pub message_id: String,
    pub text: String,
    pub planned_segments: Vec<String>,
    pub reply_to: Option<ReplyTargetRecord>,
    pub state: CandidateState,
    pub reason: String,
    pub order: u64,
    pub approval_required: bool,
    pub retry_of: Option<String>,
    pub diagnosis: Option<DeliveryDiagnosis>,
    pub confirmed_segments: Vec<String>,
    pub unconfirmed_segments: Vec<String>,
    pub duplicate_risk: bool,
}

impl From<rt::CandidateRecord> for CandidateRecord {
    fn from(value: rt::CandidateRecord) -> Self {
        let rt::CandidateRecord {
            expected,
            message_id,
            text,
            planned_segments,
            reply_to,
            state,
            reason,
            order,
            approval_required,
            retry_of,
            diagnosis,
            confirmed_segments,
            unconfirmed_segments,
            duplicate_risk,
        } = value;
        Self {
            expected: expected.into(),
            message_id,
            text,
            planned_segments,
            reply_to: reply_to.map(Into::into),
            state: state.into(),
            reason,
            order,
            approval_required,
            retry_of,
            diagnosis: diagnosis.map(Into::into),
            confirmed_segments,
            unconfirmed_segments,
            duplicate_risk,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SubscriptionAccountRecord {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub signed_in: bool,
    pub plan_enabled: bool,
}

impl From<rt::SubscriptionAccountRecord> for SubscriptionAccountRecord {
    fn from(value: rt::SubscriptionAccountRecord) -> Self {
        let rt::SubscriptionAccountRecord {
            id,
            label,
            email,
            signed_in,
            plan_enabled,
        } = value;
        Self {
            id,
            label,
            email,
            signed_in,
            plan_enabled,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ModelRecord {
    pub model_id: String,
    pub display_name: String,
}

impl From<rt::ModelRecord> for ModelRecord {
    fn from(value: rt::ModelRecord) -> Self {
        let rt::ModelRecord {
            model_id,
            display_name,
        } = value;
        Self {
            model_id,
            display_name,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopSearchState {
    NotRequested,
    Completed,
    Unavailable,
}

impl From<rt::SearchStateRecord> for DesktopSearchState {
    fn from(value: rt::SearchStateRecord) -> Self {
        match value {
            rt::SearchStateRecord::NotRequested => Self::NotRequested,
            rt::SearchStateRecord::Completed => Self::Completed,
            rt::SearchStateRecord::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopState {
    pub session_id: String,
    pub room_id: String,
    pub room: Option<RoomRecord>,
    pub connection: String,
    pub watched: Option<u64>,
    pub likes: Option<u64>,
    pub online_viewers: Option<u64>,
    pub main_account: AccountStatus,
    pub assistant_account: AccountStatus,
    pub identity_generation: u64,
    pub assistant_running: bool,
    pub automatic_permission: bool,
    pub resume_pending: bool,
    pub assistant_note: String,
    pub assistant_failure: Option<String>,
    pub assistant_search: DesktopSearchState,
    pub assistant_returned_model: Option<String>,
    pub obs: Option<ObsStatus>,
    pub microphone_level: Option<MicrophoneLevel>,
    pub obs_error: Option<String>,
    pub stop_confirmation: Option<String>,
    pub stop_seconds_remaining: Option<u64>,
    pub login_url: Option<String>,
    pub assistant_login_url: Option<String>,
    pub featured_message_id: Option<String>,
}

impl From<rt::RuntimeStateRecord> for DesktopState {
    fn from(value: rt::RuntimeStateRecord) -> Self {
        let rt::RuntimeStateRecord {
            session_id,
            room_id,
            room,
            connection,
            watched,
            likes,
            online_viewers,
            main_account,
            assistant_account,
            identity_generation,
            assistant_running,
            automatic_permission,
            resume_pending,
            assistant_note,
            assistant_failure,
            assistant_search,
            assistant_returned_model,
            obs,
            microphone_level,
            obs_error,
            stop_confirmation,
            stop_seconds_remaining,
            login_url,
            assistant_login_url,
            featured_message_id,
        } = value;
        Self {
            session_id,
            room_id,
            room: room.map(Into::into),
            connection,
            watched,
            likes,
            online_viewers,
            main_account: main_account.into(),
            assistant_account: assistant_account.into(),
            identity_generation,
            assistant_running,
            automatic_permission,
            resume_pending,
            assistant_note,
            assistant_failure,
            assistant_search: assistant_search.into(),
            assistant_returned_model,
            obs: obs.map(Into::into),
            microphone_level: microphone_level.map(Into::into),
            obs_error,
            stop_confirmation: stop_confirmation.map(|v| v.to_string()),
            stop_seconds_remaining,
            login_url,
            assistant_login_url,
            featured_message_id,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopSnapshot {
    pub state: DesktopState,
    pub events: Vec<EventRecord>,
    pub candidates: Vec<CandidateRecord>,
    pub deliveries: Vec<CandidateRecord>,
    pub new_message_count: u64,
}

impl From<rt::RuntimeSnapshot> for DesktopSnapshot {
    fn from(value: rt::RuntimeSnapshot) -> Self {
        let rt::RuntimeSnapshot {
            state,
            events,
            candidates,
            deliveries,
            new_message_count,
        } = value;
        Self {
            state: state.into(),
            events: events.into_iter().map(Into::into).collect(),
            candidates: candidates.into_iter().map(Into::into).collect(),
            deliveries: deliveries.into_iter().map(Into::into).collect(),
            new_message_count,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopOperationState {
    Accepted,

    Completed,

    Failed { reason: String },

    Cancelled,
}

impl From<rt::OperationState> for DesktopOperationState {
    fn from(value: rt::OperationState) -> Self {
        match value {
            rt::OperationState::Accepted => Self::Accepted,

            rt::OperationState::Completed => Self::Completed,

            rt::OperationState::Failed { reason } => Self::Failed { reason },

            rt::OperationState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OperationDiagnostic {
    pub operation_id: String,
    pub state: DesktopOperationState,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopDiagnostics {
    pub connection: String,
    pub assistant_note: String,
    pub assistant_details: String,
    pub history_warning: Option<String>,
    pub config_warning: Option<String>,
    pub obs_error: Option<String>,
    pub operations: Vec<OperationDiagnostic>,
}

impl From<rt::RuntimeDiagnostics> for DesktopDiagnostics {
    fn from(value: rt::RuntimeDiagnostics) -> Self {
        let rt::RuntimeDiagnostics {
            connection,
            assistant_note,
            assistant_details,
            history_warning,
            config_warning,
            obs_error,
            operations,
        } = value;
        Self {
            connection,
            assistant_note,
            assistant_details,
            history_warning,
            config_warning,
            obs_error,
            operations: operations
                .into_iter()
                .map(|(id, state)| OperationDiagnostic {
                    operation_id: id.to_string(),
                    state: state.into(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopOperationValue {
    None,

    StopConfirmation {
        token: String,
    },

    Receipt {
        message: String,
    },

    RoomUpdate {
        receipt: RoomUpdateReceipt,
    },

    ManualDelivery {
        draft_revision: u64,
        delivery: DeliveryRecord,
    },
}

impl From<rt::OperationValue> for DesktopOperationValue {
    fn from(value: rt::OperationValue) -> Self {
        match value {
            rt::OperationValue::None => Self::None,

            rt::OperationValue::StopConfirmation { token } => Self::StopConfirmation {
                token: token.to_string(),
            },

            rt::OperationValue::Receipt { message } => Self::Receipt { message },

            rt::OperationValue::RoomUpdate { receipt } => Self::RoomUpdate {
                receipt: receipt.into(),
            },

            rt::OperationValue::ManualDelivery {
                draft_revision,
                delivery,
            } => Self::ManualDelivery {
                draft_revision,
                delivery: delivery.into(),
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopHostEffect {
    OpenBrowser { operation_id: String, url: String },
}

impl From<rt::HostEffect> for DesktopHostEffect {
    fn from(value: rt::HostEffect) -> Self {
        match value {
            rt::HostEffect::OpenBrowser { operation_id, url } => Self::OpenBrowser {
                operation_id: operation_id.to_string(),
                url,
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopCommandResult {
    pub operation_id: String,
    pub state: DesktopOperationState,
    pub value: DesktopOperationValue,
    pub effects: Vec<DesktopHostEffect>,
}

impl From<rt::CommandResult> for DesktopCommandResult {
    fn from(value: rt::CommandResult) -> Self {
        let rt::CommandResult {
            operation_id,
            state,
            value,
            effects,
        } = value;
        Self {
            operation_id: operation_id.to_string(),
            state: state.into(),
            value: value.into(),
            effects: effects.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopChange {
    Upsert {
        record: EventRecord,
    },

    Remove {
        message_key: u64,
    },

    State {
        state: DesktopState,
    },

    Candidates {
        candidates: Vec<CandidateRecord>,
    },

    Deliveries {
        deliveries: Vec<CandidateRecord>,
    },

    Operation {
        operation_id: String,
        state: DesktopOperationState,
        value: DesktopOperationValue,
    },

    Notice {
        message: String,
        is_error: bool,
    },

    ResyncRequired,
}

impl From<rt::RuntimeChange> for DesktopChange {
    fn from(value: rt::RuntimeChange) -> Self {
        match value {
            rt::RuntimeChange::Upsert(record) => Self::Upsert {
                record: record.into(),
            },
            rt::RuntimeChange::Remove { message_key } => Self::Remove { message_key },
            rt::RuntimeChange::State(state) => Self::State {
                state: std::sync::Arc::unwrap_or_clone(state).into(),
            },
            rt::RuntimeChange::Candidates(candidates) => Self::Candidates {
                candidates: candidates.into_iter().map(Into::into).collect(),
            },
            rt::RuntimeChange::Deliveries(deliveries) => Self::Deliveries {
                deliveries: deliveries.into_iter().map(Into::into).collect(),
            },
            rt::RuntimeChange::Operation {
                operation_id,
                state,
                value,
            } => Self::Operation {
                operation_id: operation_id.to_string(),
                state: state.into(),
                value: value.into(),
            },
            rt::RuntimeChange::Notice { message, is_error } => Self::Notice { message, is_error },
            rt::RuntimeChange::ResyncRequired => Self::ResyncRequired,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DesktopBatch {
    pub sequence: u64,
    pub new_message_count: u64,
    pub snapshot: Option<DesktopSnapshot>,
    pub changes: Vec<DesktopChange>,
}

impl From<rt::RuntimeBatch> for DesktopBatch {
    fn from(value: rt::RuntimeBatch) -> Self {
        let rt::RuntimeBatch {
            sequence,
            new_message_count,
            snapshot,
            changes,
        } = value;
        Self {
            sequence,
            new_message_count,
            snapshot: snapshot.map(Into::into),
            changes: changes.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopAccountCommand {
    BeginLogin { identity: DesktopIdentity },

    CancelLogin,

    SignOut { identity: DesktopIdentity },

    UseMainForAssistant,

    UseIndependentAssistant,
}

impl From<DesktopAccountCommand> for rt::AccountCommand {
    fn from(value: DesktopAccountCommand) -> Self {
        match value {
            DesktopAccountCommand::BeginLogin { identity } => Self::BeginLogin {
                identity: identity.into(),
            },

            DesktopAccountCommand::CancelLogin => Self::CancelLogin,

            DesktopAccountCommand::SignOut { identity } => Self::SignOut {
                identity: identity.into(),
            },

            DesktopAccountCommand::UseMainForAssistant => Self::UseMainForAssistant,

            DesktopAccountCommand::UseIndependentAssistant => Self::UseIndependentAssistant,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopRoomCommand {
    SetTitle { text: String },

    SetCover { path: String },
}

impl TryFrom<DesktopRoomCommand> for rt::RoomCommand {
    type Error = CoreError;
    fn try_from(value: DesktopRoomCommand) -> Result<Self, CoreError> {
        Ok(match value {
            DesktopRoomCommand::SetTitle { text } => Self::SetTitle { text },

            DesktopRoomCommand::SetCover { path } => Self::SetCover {
                path: absolute_path(path, "room cover")?,
            },
        })
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopAssistantCommand {
    Start,

    Pause,

    Stop,

    ProcessVisible { message_ids: Vec<String> },

    SetAutomatic { enabled: bool },

    CancelPendingResume,

    SetRange { activity: DesktopReplyActivity },

    SetTopic { text: String },

    ApplyPreset { preset: DesktopReplyPreset },

    SaveSettings { settings: DesktopAssistantSettings },

    SetApiKey { value: String },

    ClearApiKey,

    BeginChatgptLogin { account_id: Option<String> },

    SignOutChatgpt { account_id: String },

    SelectChatgptAccount { account_id: String },

    RefreshModels,

    SelectModel { model_id: String },

    RepairHistory,
}

impl TryFrom<DesktopAssistantCommand> for rt::AssistantCommand {
    type Error = CoreError;
    fn try_from(value: DesktopAssistantCommand) -> Result<Self, CoreError> {
        Ok(match value {
            DesktopAssistantCommand::Start => Self::Start,

            DesktopAssistantCommand::Pause => Self::Pause,

            DesktopAssistantCommand::Stop => Self::Stop,

            DesktopAssistantCommand::ProcessVisible { message_ids } => {
                Self::ProcessVisible { message_ids }
            }

            DesktopAssistantCommand::SetAutomatic { enabled } => Self::SetAutomatic { enabled },

            DesktopAssistantCommand::CancelPendingResume => Self::CancelPendingResume,

            DesktopAssistantCommand::SetRange { activity } => Self::SetRange {
                activity: activity.into(),
            },

            DesktopAssistantCommand::SetTopic { text } => Self::SetTopic { text },

            DesktopAssistantCommand::ApplyPreset { preset } => Self::ApplyPreset {
                preset: preset.into(),
            },

            DesktopAssistantCommand::SaveSettings { settings } => Self::SaveSettings {
                settings: settings.try_into()?,
            },

            DesktopAssistantCommand::SetApiKey { value } => Self::SetApiKey { value },

            DesktopAssistantCommand::ClearApiKey => Self::ClearApiKey,

            DesktopAssistantCommand::BeginChatgptLogin { account_id } => {
                Self::BeginChatgptLogin { account_id }
            }

            DesktopAssistantCommand::SignOutChatgpt { account_id } => {
                Self::SignOutChatgpt { account_id }
            }

            DesktopAssistantCommand::SelectChatgptAccount { account_id } => {
                Self::SelectChatgptAccount { account_id }
            }

            DesktopAssistantCommand::RefreshModels => Self::RefreshModels,

            DesktopAssistantCommand::SelectModel { model_id } => Self::SelectModel { model_id },

            DesktopAssistantCommand::RepairHistory => Self::RepairHistory,
        })
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopReviewCommand {
    Edit {
        expected: ReviewedCandidate,
        text: String,
    },

    Approve {
        expected: Vec<ReviewedCandidate>,
    },

    Discard {
        expected: ReviewedCandidate,
    },
}

impl From<DesktopReviewCommand> for rt::ReviewCommand {
    fn from(value: DesktopReviewCommand) -> Self {
        match value {
            DesktopReviewCommand::Edit { expected, text } => Self::Edit {
                expected: expected.into(),
                text,
            },

            DesktopReviewCommand::Approve { expected } => Self::Approve {
                expected: expected.into_iter().map(Into::into).collect(),
            },

            DesktopReviewCommand::Discard { expected } => Self::Discard {
                expected: expected.into(),
            },
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopObsCommand {
    Connect,

    Refresh,

    SaveConfiguration {
        configuration: DesktopObsConfiguration,
    },

    SetPassword {
        password: String,
    },

    SetMicrophone {
        name: String,
    },

    SetMuted {
        muted: bool,
    },

    SetScene {
        name: String,
    },

    StartStream,

    RequestStop,

    ConfirmStop {
        token: String,
    },

    CancelStop {
        token: String,
    },
}

impl TryFrom<DesktopObsCommand> for rt::ObsCommand {
    type Error = CoreError;
    fn try_from(value: DesktopObsCommand) -> Result<Self, CoreError> {
        Ok(match value {
            DesktopObsCommand::Connect => Self::Connect,

            DesktopObsCommand::Refresh => Self::Refresh,

            DesktopObsCommand::SaveConfiguration { configuration } => Self::SaveConfiguration {
                configuration: configuration.into(),
            },

            DesktopObsCommand::SetPassword { password } => Self::SetPassword { password },

            DesktopObsCommand::SetMicrophone { name } => Self::SetMicrophone { name },

            DesktopObsCommand::SetMuted { muted } => Self::SetMuted { muted },

            DesktopObsCommand::SetScene { name } => Self::SetScene { name },

            DesktopObsCommand::StartStream => Self::StartStream,

            DesktopObsCommand::RequestStop => Self::RequestStop,

            DesktopObsCommand::ConfirmStop { token } => Self::ConfirmStop {
                token: parse_uuid(&token)?,
            },

            DesktopObsCommand::CancelStop { token } => Self::CancelStop {
                token: parse_uuid(&token)?,
            },
        })
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopCommand {
    Connect {
        room_id: String,
    },

    Disconnect,

    CancelOperation {
        operation_id: String,
    },

    SendManual {
        text: String,
        reply_to_message_id: Option<String>,
        draft_revision: u64,
    },

    Account {
        command: DesktopAccountCommand,
    },

    Room {
        command: DesktopRoomCommand,
    },

    Assistant {
        command: DesktopAssistantCommand,
    },

    Review {
        command: DesktopReviewCommand,
    },

    Obs {
        command: DesktopObsCommand,
    },

    FeatureMessage {
        message_id: Option<String>,
    },

    SetExternalEndpoint {
        root: Option<String>,
    },

    ExportSession {
        session_id: String,
        format: DesktopExportFormat,
        destination: String,
    },
}

impl TryFrom<DesktopCommand> for rt::RuntimeCommand {
    type Error = CoreError;
    fn try_from(value: DesktopCommand) -> Result<Self, CoreError> {
        Ok(match value {
            DesktopCommand::Connect { room_id } => Self::Connect { room_id },
            DesktopCommand::Disconnect => Self::Disconnect,
            DesktopCommand::CancelOperation { operation_id } => Self::CancelOperation {
                operation_id: parse_uuid(&operation_id)?,
            },
            DesktopCommand::SendManual {
                text,
                reply_to_message_id,
                draft_revision,
            } => Self::SendManual {
                text,
                reply_to_message_id,
                draft_revision,
            },
            DesktopCommand::Account { command } => Self::Account(command.into()),
            DesktopCommand::Room { command } => Self::Room(command.try_into()?),
            DesktopCommand::Assistant { command } => Self::Assistant(command.try_into()?),
            DesktopCommand::Review { command } => Self::Review(command.into()),
            DesktopCommand::Obs { command } => Self::Obs(command.try_into()?),
            DesktopCommand::FeatureMessage { message_id } => Self::FeatureMessage { message_id },
            DesktopCommand::SetExternalEndpoint { root } => Self::SetExternalEndpoint {
                root: root
                    .map(|p| absolute_path(p, "external endpoint"))
                    .transpose()?,
            },
            DesktopCommand::ExportSession {
                session_id,
                format,
                destination,
            } => Self::ExportSession {
                session_id,
                format: format.into(),
                destination: absolute_path(destination, "export destination")?,
            },
        })
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopQuery {
    Snapshot,

    Diagnostics,

    Settings,

    Candidates,

    SubscriptionAccounts,

    Models,

    ObsScenes,

    ObsInputs,

    SearchArchive {
        query: String,
        room_id: Option<String>,
        from: Option<i64>,
        to: Option<i64>,
    },

    LoadSession {
        session_id: String,
    },
}

impl TryFrom<DesktopQuery> for rt::RuntimeQuery {
    type Error = CoreError;
    fn try_from(value: DesktopQuery) -> Result<Self, CoreError> {
        Ok(match value {
            DesktopQuery::Snapshot => Self::Snapshot,

            DesktopQuery::Diagnostics => Self::Diagnostics,

            DesktopQuery::Settings => Self::Settings,

            DesktopQuery::Candidates => Self::Candidates,

            DesktopQuery::SubscriptionAccounts => Self::SubscriptionAccounts,

            DesktopQuery::Models => Self::Models,

            DesktopQuery::ObsScenes => Self::ObsScenes,

            DesktopQuery::ObsInputs => Self::ObsInputs,

            DesktopQuery::SearchArchive {
                query,
                room_id,
                from,
                to,
            } => Self::SearchArchive {
                query,
                room_id,
                from: from.map(parse_time).transpose()?,
                to: to.map(parse_time).transpose()?,
            },

            DesktopQuery::LoadSession { session_id } => Self::LoadSession { session_id },
        })
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DesktopQueryResult {
    Snapshot {
        snapshot: DesktopSnapshot,
    },

    Diagnostics {
        diagnostics: DesktopDiagnostics,
    },

    Settings {
        settings: DesktopSettings,
    },

    Candidates {
        candidates: Vec<CandidateRecord>,
    },

    SubscriptionAccounts {
        accounts: Vec<SubscriptionAccountRecord>,
    },

    Models {
        models: Vec<ModelRecord>,
    },

    ObsScenes {
        scenes: Vec<String>,
    },

    ObsInputs {
        inputs: Vec<String>,
    },

    Sessions {
        sessions: Vec<SessionRecord>,
    },

    Session {
        session: SessionRecord,
    },
}

impl TryFrom<rt::QueryResult> for DesktopQueryResult {
    type Error = CoreError;
    fn try_from(value: rt::QueryResult) -> Result<Self, CoreError> {
        Ok(match value {
            rt::QueryResult::Snapshot(snapshot) => Self::Snapshot {
                snapshot: (*snapshot).into(),
            },
            rt::QueryResult::Diagnostics(diagnostics) => Self::Diagnostics {
                diagnostics: diagnostics.into(),
            },
            rt::QueryResult::Settings(settings) => Self::Settings {
                settings: settings.try_into()?,
            },
            rt::QueryResult::Candidates(candidates) => Self::Candidates {
                candidates: candidates.into_iter().map(Into::into).collect(),
            },
            rt::QueryResult::SubscriptionAccounts(accounts) => Self::SubscriptionAccounts {
                accounts: accounts.into_iter().map(Into::into).collect(),
            },
            rt::QueryResult::Models(models) => Self::Models {
                models: models.into_iter().map(Into::into).collect(),
            },
            rt::QueryResult::ObsScenes(scenes) => Self::ObsScenes { scenes },
            rt::QueryResult::ObsInputs(inputs) => Self::ObsInputs { inputs },
            rt::QueryResult::Sessions(sessions) => Self::Sessions {
                sessions: sessions.into_iter().map(Into::into).collect(),
            },
            rt::QueryResult::Session(session) => Self::Session {
                session: session.into(),
            },
        })
    }
}
