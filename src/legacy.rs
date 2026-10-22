use crate::domain::DanmuEvent;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, value::RawValue};

/// Decode the event envelope written by the original Swift journal. Keep the
/// original bytes untouched: retired question metadata is not runtime authority.
pub(crate) fn decode_event(payload: &RawValue) -> Result<DanmuEvent> {
    #[derive(Deserialize)]
    struct Payload {
        event: Value,
    }
    let mut event = serde_json::from_str::<Payload>(payload.get())?.event;
    let fields = event.as_object_mut().context("旧弹幕记录不是对象")?;
    normalize_field(fields, "authorID", "authorId")?;
    normalize_field(fields, "author_id", "authorId")?;
    normalize_field(fields, "platformEventID", "platformEventId")?;
    normalize_field(fields, "platform_event_id", "platformEventId")?;
    normalize_field(fields, "reply_to", "replyTo")?;
    if let Some(emotes) = fields.get_mut("emotes").and_then(Value::as_array_mut) {
        for emote in emotes {
            let fields = emote.as_object_mut().context("旧弹幕表情不是对象")?;
            normalize_field(fields, "imageURL", "imageUrl")?;
        }
    }
    serde_json::from_value(event).context("旧弹幕记录格式错误")
}

fn normalize_field(fields: &mut Map<String, Value>, old: &str, current: &str) -> Result<()> {
    if let Some(value) = fields.remove(old) {
        if let Some(existing) = fields.get(current) {
            ensure!(existing == &value, "旧弹幕字段冲突：{current}");
        } else {
            fields.insert(current.into(), value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DanmuEventOrigin, GiftDetails};
    use serde_json::json;

    #[test]
    fn swift_acronyms_and_extended_event_fields_survive_decoding() {
        let payload = serde_json::value::to_raw_value(&json!({"event": {
            "id": "legacy", "kind": "gift", "timestamp": "2026-09-09T00:00:00Z",
            "username": "观众", "authorID": "42", "content": "礼物",
            "origin": "history", "platformEventID": "platform-42", "reply_to": "主播",
            "emotes": [{"text": "[好]", "imageURL": "https://example.invalid/emote.png",
                "width": 24, "height": 24, "isAnimated": true}],
            "gift": {"name": "花", "action": "投喂", "blindGift": null,
                "receipts": {"transaction": 2}, "reportedQuantity": 2},
            "questionStatus": "answered", "priority": "high"
        }}))
        .unwrap();
        let event = decode_event(&payload).unwrap();
        assert_eq!(event.author_id.as_deref(), Some("42"));
        assert_eq!(event.platform_event_id.as_deref(), Some("platform-42"));
        assert_eq!(event.reply_to.as_deref(), Some("主播"));
        assert_eq!(event.origin, DanmuEventOrigin::History);
        assert_eq!(
            event.emotes[0].image_url.as_str(),
            "https://example.invalid/emote.png"
        );
        assert!(event.emotes[0].is_animated);
        assert_eq!(
            event.gift.as_deref(),
            Some(&GiftDetails {
                name: "花".into(),
                action: "投喂".into(),
                blind_gift: None,
                receipts: [("transaction".into(), 2)].into(),
                reported_quantity: 2,
            })
        );
    }

    #[test]
    fn conflicting_uid_aliases_are_corruption_not_silently_reassigned_identity() {
        let payload = serde_json::value::to_raw_value(&json!({"event": {
            "id": "legacy", "kind": "danmu", "timestamp": "2026-09-09T00:00:00Z",
            "authorID": "42", "authorId": "43", "content": "不能合并身份"
        }}))
        .unwrap();
        assert!(decode_event(&payload).is_err());
    }
}
