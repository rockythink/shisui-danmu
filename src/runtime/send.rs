use super::*;
use crate::{
    bilibili::DanmuResponse,
    delivery::{Cause, Delivery},
};

pub(crate) struct RuntimeTransport {
    pub account: accounts::SendIdentity,
    pub client: BilibiliClient,
    pub room: String,
    pub tx: mpsc::Sender<CoreEvent>,
    pub job: Option<bridge::Reply>,
    pub bridge: bridge::Bridge,
    pub queue: SendQueue,
    pub manual_permit: Option<crate::delivery::Permit>,
}
impl Transport for RuntimeTransport {
    async fn send_confirm(&self, text: &str, reply_to: Option<&str>) -> Delivery {
        if !self.account.valid()
            || self
                .manual_permit
                .as_ref()
                .is_some_and(|permit| !permit.valid())
        {
            return Outcome::Cancelled.into();
        }
        let generation = self.queue.generation();
        let (name, user_id) =
            match tokio::time::timeout(Duration::from_secs(5), self.account.status()).await {
                Ok(Ok(AccountStatus::SignedIn {
                    display_name,
                    user_id,
                })) => (display_name, user_id),
                _ => {
                    self.stop_unavailable_account().await;
                    let message = "当前发送身份登录态不可用；未发送，不回退主账号";
                    let _ = self
                        .tx
                        .send(CoreEvent::DeliveryRejected {
                            content: text.into(),
                            message: message.into(),
                        })
                        .await;
                    return Delivery::new(Outcome::Rejected, Cause::OtherRejected, message);
                }
            };
        let mut delivery = PendingDelivery::new(text.into(), name, user_id, Utc::now());
        delivery.room_id = Some(self.room.clone());
        let id = delivery.id.clone();
        let (confirmation, mut rx) = oneshot::channel();
        let (registered, registered_rx) = oneshot::channel();
        if self
            .tx
            .send(CoreEvent::DeliveryStarted {
                delivery,
                confirmation,
                registered,
            })
            .await
            .is_err()
        {
            return Outcome::Cancelled.into();
        }
        // A live echo can beat the POST response. Install its matcher before starting the POST,
        // otherwise a genuine echo may be ingested as an ordinary message and later go missing.
        if registered_rx.await.is_err() {
            return Outcome::Cancelled.into();
        }
        // Login checks and the UI channel can yield before the POST begins. Recheck authorization here.
        if !self.account.valid()
            || self.queue.is_paused()
            || self
                .manual_permit
                .as_ref()
                .is_some_and(|permit| !permit.valid())
            || self.queue.generation() != generation
            || self
                .job
                .as_ref()
                .is_some_and(|job| !self.bridge.job_authorized(job))
        {
            let _ = self
                .tx
                .send(CoreEvent::DeliveryExpired { delivery_id: id })
                .await;
            return Outcome::Cancelled.into();
        }
        if let Some(job) = &self.job
            && let Some(word) = self.bridge.blocked_word(job, text)
        {
            let _ = self
                .tx
                .send(CoreEvent::DeliveryExpired { delivery_id: id })
                .await;
            return Delivery::new(
                Outcome::Rejected,
                Cause::KnownWord,
                format!("命中用户配置已知词「{word}」；未向平台发送"),
            );
        }
        let Some(account) = self.account.account() else {
            return Outcome::Cancelled.into();
        };
        let response = tokio::time::timeout(
            Duration::from_secs(8),
            account.send_danmu(text, &self.room, reply_to, || {
                self.account.valid()
                    && !self.queue.is_paused()
                    && self
                        .manual_permit
                        .as_ref()
                        .is_none_or(crate::delivery::Permit::valid)
                    && self.queue.generation() == generation
                    && self
                        .job
                        .as_ref()
                        .is_none_or(|job| self.bridge.job_authorized(job))
            }),
        )
        .await;
        let (proof, detail) = match response {
            Ok(Ok(None)) => {
                let _ = self
                    .tx
                    .send(CoreEvent::DeliveryExpired { delivery_id: id })
                    .await;
                return Outcome::Cancelled.into();
            }
            Ok(Ok(Some(DanmuResponse::Responded { proof, detail }))) => (proof, detail),
            Ok(Ok(Some(DanmuResponse::UncertainResponse {
                detail,
                authentication_failed,
            }))) => {
                if authentication_failed {
                    self.stop_unavailable_account().await;
                }
                let _ = self
                    .tx
                    .send(CoreEvent::DeliveryTimedOut {
                        delivery_ids: vec![id],
                    })
                    .await;
                let mut result =
                    Delivery::new(Outcome::Uncertain, Cause::TransportUncertain, detail);
                result.diagnosis.response_received = true;
                return result;
            }
            Ok(Ok(Some(response))) => {
                let (cause, code, message) = match response {
                    DanmuResponse::ContentRejected { code, message } => {
                        (Cause::ContentRejected, code, message)
                    }
                    DanmuResponse::Rejected { code, message } => {
                        (Cause::OtherRejected, code, message)
                    }
                    DanmuResponse::Responded { .. } | DanmuResponse::UncertainResponse { .. } => {
                        unreachable!()
                    }
                };
                if matches!(code, -101 | -102 | -111) {
                    self.stop_unavailable_account().await;
                }
                let _ = self
                    .tx
                    .send(CoreEvent::DeliveryExpired { delivery_id: id })
                    .await;
                let _ = self
                    .tx
                    .send(CoreEvent::DeliveryRejected {
                        content: text.into(),
                        message: message.clone(),
                    })
                    .await;
                let mut result = Delivery::new(Outcome::Rejected, cause, message);
                result.diagnosis.response_received = true;
                result.diagnosis.platform_code = Some(code);
                return result;
            }
            error => {
                let detail = match error {
                    Ok(Err(error)) => {
                        format!("POST传输或响应解析失败：{error}；结果未知，不认定屏蔽，不自动重发")
                    }
                    Err(_) => {
                        "POST本身8秒超时；结果未知，可能已发送；不认定屏蔽，不自动重发".into()
                    }
                    _ => unreachable!(),
                };
                let _ = self
                    .tx
                    .send(CoreEvent::DeliveryTimedOut {
                        delivery_ids: vec![id],
                    })
                    .await;
                return Delivery::new(Outcome::Uncertain, Cause::TransportUncertain, detail);
            }
        };
        // Even code=0 without mode_info/extra must wait for a real matching echo.
        let _ = self.tx.send(CoreEvent::DeliveryAccepted).await;
        let wait = async {
            for attempt in 0..8 {
                tokio::select! {
                    echo = &mut rx => return echo.is_ok(),
                    _ = tokio::time::sleep(if attempt == 0 { Duration::from_millis(350) } else { Duration::from_secs(2) }) => {}
                }
                tokio::select! {
                    echo = &mut rx => return echo.is_ok(),
                    history = self.client.history(&self.room) => {
                        if let Ok(events) = history {
                            let _ = self.tx.send(CoreEvent::DeliveryHistory { events, room_id: self.room.clone() }).await;
                        }
                    }
                }
            }
            tokio::select! {
                echo = &mut rx => echo.is_ok(),
                _ = tokio::time::sleep(Duration::from_millis(250)) => false,
            }
        };
        let confirmed = matches!(
            tokio::time::timeout(Duration::from_secs(16), wait).await,
            Ok(true)
        );
        if confirmed {
            let mut result = Delivery::new(
                Outcome::Confirmed,
                Cause::EchoConfirmed,
                format!("{detail}；真实回显确认"),
            );
            result.diagnosis.response_received = true;
            result.diagnosis.acceptance_proof = proof;
            result.diagnosis.platform_code = Some(0);
            return result;
        }
        let _ = self
            .tx
            .send(CoreEvent::DeliveryEchoMissing {
                delivery_ids: vec![id.clone()],
            })
            .await;
        let tx = self.tx.clone();
        let bridge = self.bridge.clone();
        let job = self.job.clone();
        let _ = self
            .tx
            .send(CoreEvent::Background(Box::pin(async move {
                if matches!(
                    tokio::time::timeout(Duration::from_secs(60), rx).await,
                    Ok(Ok(()))
                ) {
                    if let Some(job) = job {
                        bridge.late_echo(&job);
                    }
                    let _ = tx.send(CoreEvent::DeliveryCompleted).await;
                    let _ = tx
                        .send(CoreEvent::DeliveryNotice(
                            "原段迟到回显已确认；不自动重发".into(),
                        ))
                        .await;
                }
                let _ = tx
                    .send(CoreEvent::DeliveryExpired { delivery_id: id })
                    .await;
            })))
            .await;
        let mut result = Delivery::new(
            Outcome::Uncertain,
            Cause::EchoMissing,
            format!("{detail}；接口已返回，但未确认真实回显，保留结果且不自动重发"),
        );
        result.diagnosis.response_received = true;
        result.diagnosis.acceptance_proof = proof;
        result.diagnosis.platform_code = Some(0);
        result
    }
}

impl RuntimeTransport {
    pub(crate) async fn stop_unavailable_account(&self) {
        if let Some(generation) = self.account.invalidate() {
            self.bridge.identity_changed();
            let _ = self
                .tx
                .send(CoreEvent::AssistantAccount(
                    accounts::AccountEvent::Unavailable { generation },
                ))
                .await;
        }
    }
}
