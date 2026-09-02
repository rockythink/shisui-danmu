use crate::domain::{DanmuEmote, DanmuEvent, DanmuEventKind};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{TimeZone, Utc};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

pub(crate) fn command_name(value: &Value) -> Option<&str> {
    value.get("cmd")?.as_str()?.split(':').next()
}

pub fn parse_command(value: &Value) -> Option<DanmuEvent> {
    let command = command_name(value)?;
    let data = value.get("data").unwrap_or(&Value::Null);
    match command {
        "DANMU_MSG" => parse_danmu(value),
        "SEND_GIFT" | "COMBO_SEND" => event(
            DanmuEventKind::Gift,
            username(data),
            author_id(data),
            gift_content(data),
            event_id(data),
        ),
        "SUPER_CHAT_MESSAGE" | "SUPER_CHAT_MESSAGE_JPN" | "SUPER_CHAT_MESSAGE_JP" => event(
            DanmuEventKind::Superchat,
            username(data),
            author_id(data),
            text(data, &["message", "content"]),
            event_id(data),
        ),
        "GUARD_BUY" | "USER_TOAST_MSG" => event(
            DanmuEventKind::GuardEvent,
            username(data),
            author_id(data),
            guard_content(data),
            event_id(data),
        ),
        "INTERACT_WORD" => parse_interaction(data),
        "INTERACT_WORD_V2" => parse_interaction_v2(data),
        "ENTRY_EFFECT" => event(
            DanmuEventKind::Enter,
            username(data),
            author_id(data),
            text(data, &["copy_writing", "uname"]).or_else(|| Some("进入直播间".into())),
            event_id(data),
        ),
        "LIKE_INFO_V3_CLICK" => event(
            DanmuEventKind::Like,
            username(data),
            author_id(data),
            Some(format!(
                "{} 点赞了直播间",
                username(data).unwrap_or_else(|| "观众".into())
            )),
            event_id(data),
        ),
        "ROOM_ADMIN_ENTRANCE"
        | "room_admin_entrance"
        | "ROOM_ADMIN_REVOKE"
        | "ROOM_BLOCK_MSG"
        | "ROOM_SILENT_ON"
        | "ROOM_SILENT_OFF" => event(
            DanmuEventKind::Moderation,
            username(data),
            author_id(data),
            text(data, &["msg", "uname", "content"]).or_else(|| Some(command.replace('_', " "))),
            event_id(data),
        ),
        "LIVE" => event(
            DanmuEventKind::RoomStatus,
            None,
            None,
            Some("直播开始".into()),
            event_id(data),
        ),
        "PREPARING" => event(
            DanmuEventKind::RoomStatus,
            None,
            None,
            Some("直播结束".into()),
            event_id(data),
        ),
        command if command.starts_with("PK_BATTLE_") => event(
            DanmuEventKind::Pk,
            username(data),
            author_id(data),
            text(data, &["pk_status", "msg"]).or_else(|| Some(command.replace('_', " "))),
            event_id(data),
        ),
        command
            if command.contains("LOTTERY")
                || command.contains("RAFFLE")
                || command.starts_with("POPULARITY_RED_POCKET")
                || command.starts_with("ANCHOR_LOT_") =>
        {
            event(
                DanmuEventKind::Lottery,
                username(data),
                author_id(data),
                text(data, &["lottery_name", "name", "msg", "title"])
                    .or_else(|| Some("直播间抽奖事件".into())),
                event_id(data),
            )
        }
        "COMMON_NOTICE_DANMAKU" | "WARNING" | "CUT_OFF" => event(
            DanmuEventKind::System,
            None,
            None,
            text(data, &["content", "msg", "message"]).or_else(|| text(value, &["msg"])),
            event_id(data),
        ),
        "NOTICE_MSG" => None,
        _ => None,
    }
}

fn parse_danmu(value: &Value) -> Option<DanmuEvent> {
    let info = value.get("info")?.as_array()?;
    let content = info.get(1)?.as_str()?.trim().to_string();
    if content.is_empty() {
        return None;
    }
    let user = info.get(2).and_then(Value::as_array);
    let author_id = user.and_then(|items| items.first()).and_then(value_string);
    let username = user.and_then(|items| items.get(1)).and_then(value_string);
    let metadata = info.first().and_then(Value::as_array);
    let timestamp = metadata
        .and_then(|items| items.get(4))
        .and_then(value_i64)
        .filter(|value| *value > 0)
        .and_then(|value| {
            if value >= 100_000_000_000 {
                Utc.timestamp_millis_opt(value).single()
            } else {
                Utc.timestamp_opt(value, 0).single()
            }
        })
        .unwrap_or_else(Utc::now);
    let platform_id = live_source_identifier(value);
    let emotes = metadata
        .and_then(|items| items.get(13))
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    let url = value
                        .get("url")
                        .and_then(Value::as_str)
                        .and_then(|value| Url::parse(value).ok())?;
                    Some(DanmuEmote {
                        text: key.clone(),
                        fallback: bilibili_emote_fallback(key).into(),
                        image_url: url,
                        width: value
                            .get("width")
                            .and_then(Value::as_u64)
                            .map(|value| value as u32),
                        height: value
                            .get("height")
                            .and_then(Value::as_u64)
                            .map(|value| value as u32),
                        is_animated: value.get("is_dynamic").and_then(Value::as_i64) == Some(1),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(DanmuEvent {
        id: stable_live_identifier(value, platform_id.as_deref()),
        kind: DanmuEventKind::Danmu,
        timestamp,
        username,
        author_id,
        content,
        origin: Default::default(),
        platform_event_id: platform_id,
        emotes,
    })
}

fn live_source_identifier(value: &Value) -> Option<String> {
    let data = value.get("data").unwrap_or(&Value::Null);
    ["id_str", "message_id", "transaction_id"]
        .iter()
        .find_map(|key| data.get(*key).and_then(value_string))
        .or_else(|| value.get("id").and_then(value_string))
}

fn stable_live_identifier(value: &Value, source_identifier: Option<&str>) -> String {
    if let Some(source_identifier) = source_identifier {
        return format!("bili-{source_identifier}");
    }

    let canonical = serde_json::to_vec(value).unwrap_or_default();
    let hash = canonical
        .into_iter()
        .fold(14_695_981_039_346_656_037_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
    format!("bili-{hash:x}")
}

fn bilibili_emote_fallback(label: &str) -> &'static str {
    let label = label.trim_matches(['[', ']']).to_lowercase();
    let mappings: &[(&[&str], &str)] = &[
        (&["笑哭", "喜极而泣"], "😂"),
        (&["大笑", "妙啊", "开心", "呲牙", "滑稽"], "😄"),
        (&["大哭", "哭", "泪"], "😭"),
        (&["捂脸"], "🤦"),
        (&["吃瓜"], "🍉"),
        (&["鼓掌"], "👏"),
        (&["赞", "支持", "打call"], "👍"),
        (&["爱", "喜欢", "心"], "❤️"),
        (&["星星眼"], "🤩"),
        (&["脸红", "害羞"], "😊"),
        (&["惊", "哦呼"], "😮"),
        (&["疑问", "问号"], "🤔"),
        (&["无语"], "😑"),
        (&["汗"], "😅"),
        (&["疼"], "😣"),
        (&["怒", "生气"], "😡"),
        (&["阴险"], "😏"),
        (&["保佑", "祈祷"], "🙏"),
        (&["加油"], "💪"),
        (&["猫"], "🐱"),
        (&["dog", "狗"], "🐶"),
    ];
    mappings
        .iter()
        .find_map(|(keywords, emoji)| {
            keywords
                .iter()
                .any(|keyword| label.contains(keyword))
                .then_some(*emoji)
        })
        .unwrap_or("🙂")
}

fn parse_interaction(data: &Value) -> Option<DanmuEvent> {
    interaction_event(
        data.get("msg_type")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        username(data),
        author_id(data),
        event_id(data),
    )
}

fn parse_interaction_v2(data: &Value) -> Option<DanmuEvent> {
    let encoded = data.get("pb")?.as_str()?;
    let bytes = STANDARD.decode(encoded).ok()?;
    let interaction = decode_interaction_v2(&bytes)?;
    interaction_event(
        interaction.msg_type,
        interaction.username,
        interaction.author_id,
        None,
    )
}

fn interaction_event(
    msg_type: i64,
    name: Option<String>,
    author_id: Option<String>,
    event_id: Option<String>,
) -> Option<DanmuEvent> {
    let kind = match msg_type {
        2 | 4 | 5 => DanmuEventKind::Follow,
        3 => DanmuEventKind::Share,
        _ => DanmuEventKind::Enter,
    };
    let content = match kind {
        DanmuEventKind::Follow => format!("{} 关注了直播间", name.as_deref().unwrap_or("观众")),
        DanmuEventKind::Share => format!("{} 分享了直播间", name.as_deref().unwrap_or("观众")),
        _ => format!("{} 进入直播间", name.as_deref().unwrap_or("观众")),
    };
    event(kind, name, author_id, Some(content), event_id)
}

struct InteractionV2 {
    msg_type: i64,
    username: Option<String>,
    author_id: Option<String>,
}

fn decode_interaction_v2(bytes: &[u8]) -> Option<InteractionV2> {
    let mut cursor = 0usize;
    let mut msg_type = None;
    let mut username = None;
    let mut author_id = None;
    while cursor < bytes.len() {
        let tag = read_protobuf_varint(bytes, &mut cursor)?;
        let field = tag >> 3;
        let wire_type = (tag & 0x07) as u8;
        match (field, wire_type) {
            (1, 0) => author_id = Some(read_protobuf_varint(bytes, &mut cursor)?.to_string()),
            (2, 2) => {
                username = Some(
                    std::str::from_utf8(read_protobuf_bytes(bytes, &mut cursor)?)
                        .ok()?
                        .to_owned(),
                );
            }
            (5, 0) => msg_type = Some(read_protobuf_varint(bytes, &mut cursor)? as i64),
            _ => skip_protobuf_field(bytes, &mut cursor, wire_type)?,
        }
    }
    Some(InteractionV2 {
        msg_type: msg_type?,
        username,
        author_id,
    })
}

fn read_protobuf_varint(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn read_protobuf_bytes<'a>(bytes: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    let length = usize::try_from(read_protobuf_varint(bytes, cursor)?).ok()?;
    let end = cursor.checked_add(length)?;
    let value = bytes.get(*cursor..end)?;
    *cursor = end;
    Some(value)
}

fn skip_protobuf_field(bytes: &[u8], cursor: &mut usize, wire_type: u8) -> Option<()> {
    match wire_type {
        0 => {
            read_protobuf_varint(bytes, cursor)?;
        }
        1 => *cursor = cursor.checked_add(8)?,
        2 => {
            read_protobuf_bytes(bytes, cursor)?;
        }
        5 => *cursor = cursor.checked_add(4)?,
        _ => return None,
    }
    (*cursor <= bytes.len()).then_some(())
}

fn event(
    kind: DanmuEventKind,
    username: Option<String>,
    author_id: Option<String>,
    content: Option<String>,
    platform_event_id: Option<String>,
) -> Option<DanmuEvent> {
    let content = content?.trim().to_string();
    if content.is_empty() {
        return None;
    }
    Some(DanmuEvent {
        id: platform_event_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        kind,
        timestamp: Utc::now(),
        username,
        author_id,
        content,
        origin: Default::default(),
        platform_event_id,
        emotes: Vec::new(),
    })
}

fn username(data: &Value) -> Option<String> {
    text(data, &["uname", "username", "user_name", "name"])
        .or_else(|| data.pointer("/user_info/uname").and_then(value_string))
        .or_else(|| {
            data.pointer("/sender_uinfo/base/name")
                .and_then(value_string)
        })
}

fn author_id(data: &Value) -> Option<String> {
    ["uid", "sender_uid", "user_id", "mid"]
        .iter()
        .find_map(|key| data.get(*key).and_then(value_string))
        .or_else(|| data.pointer("/user_info/uid").and_then(value_string))
        .or_else(|| data.pointer("/sender_uinfo/uid").and_then(value_string))
}

fn event_id(data: &Value) -> Option<String> {
    ["id_str", "id", "dmid", "message_id"]
        .iter()
        .find_map(|key| data.get(*key).and_then(value_string))
}

fn text(data: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| data.get(*key).and_then(value_string))
}

fn value_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn gift_content(data: &Value) -> Option<String> {
    let gift = text(data, &["giftName", "gift_name"]).unwrap_or_else(|| "礼物".into());
    let count = ["num", "gift_num", "total_num", "combo_num"]
        .iter()
        .find_map(|key| data.get(*key).and_then(value_i64))
        .unwrap_or(1);
    Some(format!("赠送 {gift} ×{count}"))
}

fn guard_content(data: &Value) -> Option<String> {
    text(data, &["toast_msg", "msg"]).or_else(|| {
        let name = text(data, &["username", "uname"]).unwrap_or_else(|| "观众".into());
        let count = data.get("num").and_then(Value::as_i64).unwrap_or(1);
        Some(format!("{name} 开通大航海 ×{count}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_danmu() {
        let event = parse_command(
            &json!({"cmd":"DANMU_MSG","info":[[0,0,0,0,1700000000000_i64],"如何学习？",[42,"石头"]]}),
        )
        .unwrap();
        assert_eq!(event.kind, DanmuEventKind::Danmu);
        assert_eq!(event.author_id.as_deref(), Some("42"));
        assert_eq!(event.username.as_deref(), Some("石头"));
    }
    #[test]
    fn parses_live_danmu_timestamps_in_seconds() {
        let event = parse_command(
            &json!({"cmd":"DANMU_MSG","info":[[0,0,0,0,1700000000_i64,0],"实时问题",[42,"石头"]]}),
        )
        .unwrap();

        assert_eq!(event.timestamp.timestamp(), 1_700_000_000);
    }

    #[test]
    fn does_not_use_empty_dm_v2_or_legacy_metadata_as_the_event_id() {
        let first = parse_command(
            &json!({"cmd":"DANMU_MSG","dm_v2":"","info":[[0,0,0,0,1700000000_i64,0],"第一个问题",[42,"石头"]]}),
        )
        .unwrap();
        let second = parse_command(
            &json!({"cmd":"DANMU_MSG","dm_v2":"","info":[[0,0,0,0,1700000001_i64,0],"第二个问题",[42,"石头"]]}),
        )
        .unwrap();

        assert_ne!(first.id, second.id);
        assert_ne!(first.id, "0");
        assert_ne!(first.id, "");
        assert!(first.platform_event_id.is_none());
    }

    #[test]
    fn assigns_unicode_fallbacks_to_bilibili_image_emotes() {
        let mut metadata = vec![Value::Null; 14];
        metadata[13] = json!({
            "[妙啊]": {
                "url": "https://example.com/emote.png",
                "width": 48,
                "height": 48
            }
        });
        let event = parse_command(&json!({
            "cmd": "DANMU_MSG",
            "info": [metadata, "[妙啊]", [42, "石头"]]
        }))
        .unwrap();

        assert_eq!(event.emotes[0].fallback, "😄");
    }

    #[test]
    fn ignores_notice_noise() {
        assert!(parse_command(&json!({"cmd":"NOTICE_MSG","data":{"msg":"广告"}})).is_none());
    }

    #[test]
    fn parses_real_interact_word_v2_payload() {
        let event = parse_command(&json!({
            "cmd": "INTERACT_WORD_V2",
            "data": {
                "dmscore": 3,
                "pb": "CJTwwNEBEgpTdGFyU2VhMjQ2IgIDASgBMNWgITispaTDBkDUubHe/jJKLAiv8CkQEhoG55Sf5oCBIKS6ngYopLqeBjCkup4GOKS6ngZAAWDVoCFo9JQRYgB4gZ/v1tmc1qcYmgEAsgHPAQiU8MDRARJYCgpTdGFyU2VhMjQ2EkpodHRwczovL2kwLmhkc2xiLmNvbS9iZnMvZmFjZS8xMDliNzg3YzVmMTEzYzRhM2M3NDE1YmI5YmY2YjgyYmMzM2JjNGUyLmpwZxpnCgbnlJ/mgIEQEhikup4GIKS6ngYopLqeBjCkup4GOP/hAUgBUK/wKWD0lBF6CSNEQzZCNkI5OYIBCSNEQzZCNkI5OYoBCSNEQzZCNkI5OZIBCSNGRkZGRkZGRpoBCSM4MTAwMUY5OSICCAkyALoBAA=="
            }
        }))
        .unwrap();

        assert_eq!(event.kind, DanmuEventKind::Enter);
        assert_eq!(event.username.as_deref(), Some("StarSea246"));
        assert_eq!(event.author_id.as_deref(), Some("439367700"));
    }

    #[test]
    fn maps_supported_live_commands_to_domain_kinds() {
        let cases = [
            (
                json!({"cmd":"SEND_GIFT","data":{"uname":"甲","giftName":"花","num":2}}),
                DanmuEventKind::Gift,
            ),
            (
                json!({"cmd":"SUPER_CHAT_MESSAGE","data":{"uname":"甲","message":"问题"}}),
                DanmuEventKind::Superchat,
            ),
            (
                json!({"cmd":"GUARD_BUY","data":{"username":"甲","num":1}}),
                DanmuEventKind::GuardEvent,
            ),
            (
                json!({"cmd":"INTERACT_WORD","data":{"uname":"甲","msg_type":2}}),
                DanmuEventKind::Follow,
            ),
            (
                json!({"cmd":"room_admin_entrance","data":{"uname":"甲"}}),
                DanmuEventKind::Moderation,
            ),
            (
                json!({"cmd":"ANCHOR_LOT_START","data":{"name":"抽奖"}}),
                DanmuEventKind::Lottery,
            ),
            (
                json!({"cmd":"LIKE_INFO_V3_CLICK","data":{"uname":"甲"}}),
                DanmuEventKind::Like,
            ),
            (
                json!({"cmd":"ROOM_BLOCK_MSG","data":{"uname":"甲"}}),
                DanmuEventKind::Moderation,
            ),
            (json!({"cmd":"LIVE","data":{}}), DanmuEventKind::RoomStatus),
            (
                json!({"cmd":"PK_BATTLE_START","data":{}}),
                DanmuEventKind::Pk,
            ),
            (
                json!({"cmd":"ANCHOR_LOTTERY_START","data":{"name":"抽奖"}}),
                DanmuEventKind::Lottery,
            ),
            (
                json!({"cmd":"WARNING","data":{"msg":"警告"}}),
                DanmuEventKind::System,
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(
                parse_command(&value).map(|event| event.kind),
                Some(expected)
            );
        }
    }
}
