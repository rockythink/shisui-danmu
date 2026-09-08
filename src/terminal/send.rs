use super::*;

pub(super) struct TerminalTransport {
    pub account: AccountClient,
    pub client: BilibiliClient,
    pub room: String,
    pub tx: mpsc::Sender<UiEvent>,
    pub reply_key: Option<String>,
}
impl Transport for TerminalTransport {
    async fn send_confirm(&self, text: &str) -> Outcome {
        let (name, user_id) =
            match tokio::time::timeout(Duration::from_secs(5), self.account.status()).await {
                Ok(Ok(AccountStatus::SignedIn {
                    display_name,
                    user_id,
                })) => (display_name, user_id),
                _ => {
                    let _ = self
                        .tx
                        .send(UiEvent::DeliveryRejected {
                            content: text.into(),
                            message: "登录态不可用；发送队列安全暂停".into(),
                        })
                        .await;
                    return Outcome::Rejected;
                }
            };
        let delivery = PendingDelivery::new(text.into(), name, user_id, Utc::now());
        let id = delivery.id.clone();
        let (confirmation, rx) = oneshot::channel();
        if self
            .tx
            .send(UiEvent::DeliveryStarted {
                delivery,
                confirmation,
            })
            .await
            .is_err()
        {
            return Outcome::Cancelled;
        }
        match tokio::time::timeout(
            Duration::from_secs(8),
            self.account.send_danmu(text, &self.room, None),
        )
        .await
        {
            Ok(Ok(())) => {
                let _ = self.tx.send(UiEvent::DeliveryAccepted).await;
                if let Some(key) = &self.reply_key {
                    let _ = self
                        .tx
                        .send(UiEvent::ReplyDelivery {
                            key: key.clone(),
                            state: ReplyState::Accepted,
                        })
                        .await;
                }
            }
            _ => {
                // A failed POST response does not prove the platform did not accept it.
                let _ = self
                    .tx
                    .send(UiEvent::DeliveryTimedOut {
                        delivery_ids: vec![id],
                    })
                    .await;
                return Outcome::Uncertain;
            }
        }
        let unresolved = tokio::time::timeout(
            Duration::from_secs(16),
            wait_for_delivery_confirmations(
                &self.client,
                &self.room,
                &self.tx,
                vec![(id.clone(), rx)],
            ),
        )
        .await;
        if matches!(&unresolved, Ok(pending) if pending.is_empty()) {
            Outcome::Confirmed
        } else {
            let _ = self
                .tx
                .send(UiEvent::DeliveryTimedOut {
                    delivery_ids: vec![id],
                })
                .await;
            Outcome::Uncertain
        }
    }
}
