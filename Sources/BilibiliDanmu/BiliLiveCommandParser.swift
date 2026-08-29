import DanmuCore
import Foundation

enum BiliLiveCommandParser {
    static func parse(
        jsonData: Data,
        timestamp: Date = .now,
        id: String? = nil
    ) -> DanmuEvent? {
        guard
            let object = try? JSONSerialization.jsonObject(with: jsonData),
            let raw = object as? [String: Any]
        else {
            return nil
        }
        return parse(rawCommand: raw, timestamp: timestamp, id: id)
    }

    static func parse(
        rawCommand raw: [String: Any],
        timestamp: Date = .now,
        id: String? = nil
    ) -> DanmuEvent? {
        let sourceID = sourceIdentifier(for: raw)
        let eventID = id ?? stableIdentifier(for: raw)
        let rawCommand = stringValue(raw["cmd"]) ?? ""
        let command = rawCommand.split(separator: ":").first.map(String.init) ?? rawCommand
        let data = raw["data"] as? [String: Any] ?? [:]

        switch command {
        case "DANMU_MSG":
            guard let info = raw["info"] as? [Any] else {
                return nil
            }
            let emotes = danmuEmotes(from: info)
            let parsedContent = danmuContent(from: info)
            let content = parsedContent.isEmpty ? emotes.map(\.text).joined() : parsedContent
            let user = safeValue(in: info, at: 2) as? [Any] ?? []
            return DanmuEvent(
                id: eventID,
                kind: .danmu,
                timestamp: timestamp,
                username: stringValue(safeValue(in: user, at: 1)) ?? "观众",
                authorID: stringValue(safeValue(in: user, at: 0)),
                content: content,
                platformEventID: sourceID,
                emotes: emotes
            )
        case "SEND_GIFT", "COMBO_SEND":
            let username = stringValue(data["uname"]) ?? stringValue(data["user_name"]) ?? "观众"
            let giftName = stringValue(data["giftName"]) ?? stringValue(data["gift_name"]) ?? "礼物"
            let count = intValue(data["num"]) ?? intValue(data["total_num"]) ?? 1
            return DanmuEvent(
                id: eventID,
                kind: .gift,
                timestamp: timestamp,
                username: username,
                authorID: stringValue(data["uid"]) ?? stringValue(data["sender_uid"]),
                content: "\(username) 送出 \(giftName) x\(count)"
            )
        case "SUPER_CHAT_MESSAGE", "SUPER_CHAT_MESSAGE_JP", "SUPER_CHAT_MESSAGE_JPN":
            let username = usernameFromUserInfo(data["user_info"]) ?? "观众"
            return DanmuEvent(
                id: eventID,
                kind: .superchat,
                timestamp: timestamp,
                username: username,
                content: stringValue(data["message"]) ?? "醒目留言"
            )
        case "GUARD_BUY", "USER_TOAST_MSG":
            let username = stringValue(data["username"]) ?? stringValue(data["uname"]) ?? "观众"
            let content = stringValue(data["toast_msg"])
                ?? "\(username) 开通 \(stringValue(data["gift_name"]) ?? "舰长")"
            return DanmuEvent(
                id: eventID,
                kind: .guardEvent,
                timestamp: timestamp,
                username: username,
                content: content
            )
        case "INTERACT_WORD":
            let username = entryUsername(from: data) ?? "观众"
            switch intValue(data["msg_type"]) ?? 1 {
            case 2:
                return DanmuEvent(
                    id: eventID,
                    kind: .follow,
                    timestamp: timestamp,
                    username: username,
                    content: "\(username) 关注了主播"
                )
            case 4:
                return DanmuEvent(
                    id: eventID,
                    kind: .follow,
                    timestamp: timestamp,
                    username: username,
                    content: "\(username) 特别关注了主播"
                )
            case 5:
                return DanmuEvent(
                    id: eventID,
                    kind: .follow,
                    timestamp: timestamp,
                    username: username,
                    content: "\(username) 与主播互相关注"
                )
            case 3:
                return DanmuEvent(
                    id: eventID,
                    kind: .share,
                    timestamp: timestamp,
                    username: username,
                    content: "\(username) 分享了直播间"
                )
            default:
                return DanmuEvent(
                    id: eventID,
                    kind: .enter,
                    timestamp: timestamp,
                    username: username,
                    content: "\(username) 进入直播间"
                )
            }
        case "ENTRY_EFFECT":
            let username = entryUsername(from: data) ?? "观众"
            return DanmuEvent(
                id: eventID,
                kind: .enter,
                timestamp: timestamp,
                username: username,
                content: "\(username) 进入直播间"
            )
        case "LIKE_INFO_V3_CLICK":
            let username = stringValue(data["uname"]) ?? "观众"
            return DanmuEvent(
                id: eventID,
                kind: .like,
                timestamp: timestamp,
                username: username,
                content: "\(username) 点赞了直播间"
            )
        case "ROOM_ADMIN_ENTRANCE", "ROOM_ADMIN_REVOKE", "ROOM_BLOCK_MSG",
             "ROOM_SILENT_ON", "ROOM_SILENT_OFF":
            let username = stringValue(data["uname"])
                ?? stringValue(data["username"])
                ?? stringValue(data["name"])
            return DanmuEvent(
                id: eventID,
                kind: .moderation,
                timestamp: timestamp,
                username: username,
                content: moderationContent(for: command, username: username)
            )
        case "LIVE", "PREPARING":
            return DanmuEvent(
                id: eventID,
                kind: .roomStatus,
                timestamp: timestamp,
                content: command == "LIVE" ? "直播已开始" : "直播已结束"
            )
        case "NOTICE_MSG":
            // B 站用 NOTICE_MSG 发送高能/跨房间礼物广播。
            // 这类推广不是观众的直接互动，且正文含平台私有的 <%...%> 模板标记。
            return nil
        case "COMMON_NOTICE_DANMAKU", "WARNING", "CUT_OFF":
            return DanmuEvent(
                id: eventID,
                kind: .system,
                timestamp: timestamp,
                content: systemNoticeContent(raw: raw, data: data)
            )
        default:
            if command.hasPrefix("PK_BATTLE_") {
                return DanmuEvent(
                    id: eventID,
                    kind: .pk,
                    timestamp: timestamp,
                    content: pkContent(for: command)
                )
            }
            if isLotteryCommand(command) {
                return DanmuEvent(
                    id: eventID,
                    kind: .lottery,
                    timestamp: timestamp,
                    content: lotteryContent(for: command, data: data)
                )
            }
            return nil
        }
    }

    private static func pkContent(for command: String) -> String {
        if command.contains("PRE") {
            return "PK 即将开始"
        }
        if command.contains("PUNISH_BEGIN") {
            return "PK 惩罚阶段开始"
        }
        if command.contains("START") || command.contains("BEGIN") {
            return "PK 已开始"
        }
        if command.contains("PUNISH_END") {
            return "PK 惩罚阶段结束"
        }
        if command.contains("SETTLE") {
            return "PK 结算完成"
        }
        if command.contains("END") || command.contains("TIMEOUT") {
            return "PK 已结束"
        }
        return "PK 状态更新"
    }

    private static func isLotteryCommand(_ command: String) -> Bool {
        command.hasPrefix("ANCHOR_LOT_")
            || command.hasPrefix("POPULARITY_RED_POCKET_")
            || command.hasPrefix("RAFFLE_")
            || command == "LOTTERY_START"
            || command == "LOTTERY_END"
    }

    private static func moderationContent(for command: String, username: String?) -> String {
        let name = username ?? "该用户"
        switch command {
        case "ROOM_ADMIN_ENTRANCE":
            return "\(name) 成为房管"
        case "ROOM_ADMIN_REVOKE":
            return "\(name) 被取消房管"
        case "ROOM_BLOCK_MSG":
            return "\(name) 被禁言"
        case "ROOM_SILENT_ON":
            return "直播间已开启全员禁言"
        case "ROOM_SILENT_OFF":
            return "直播间已关闭全员禁言"
        default:
            return "直播间管理状态更新"
        }
    }

    private static func systemNoticeContent(raw: [String: Any], data: [String: Any]) -> String {
        let direct = stringValue(data["message"])
            ?? stringValue(data["msg"])
            ?? stringValue(data["msg_common"])
            ?? stringValue(data["msg_self"])
            ?? stringValue(data["content"])
            ?? stringValue(raw["message"])
            ?? stringValue(raw["msg"])
            ?? stringValue(raw["msg_common"])

        if let direct = direct?.nonEmptyTrimmed {
            return direct
        }
        if let segments = data["content_segments"] as? [[String: Any]] {
            let content = segments.compactMap { stringValue($0["text"]) }.joined()
            if let content = content.nonEmptyTrimmed {
                return content
            }
        }
        return "B 站直播间通知"
    }

    private static func lotteryContent(for command: String, data: [String: Any]) -> String {
        let isRedPocket = command.contains("RED_POCKET")
        let isResult = command.contains("AWARD")
            || command.contains("WINNER")
            || command.hasSuffix("_END")

        if isRedPocket {
            return isResult ? "红包结果已公布" : "红包活动开始"
        }
        if isResult {
            return "抽奖结果已公布"
        }
        let awardName = stringValue(data["award_name"])
            ?? stringValue(data["prize_name"])
        return awardName.map { "抽奖开始：\($0)" } ?? "抽奖开始"
    }

    private static func danmuContent(from info: [Any]) -> String {
        let content = stringValue(safeValue(in: info, at: 1)) ?? ""
        guard
            let modeInfo = danmuModeInfo(from: info),
            let replyUsername = replyUsername(from: modeInfo)?.nonEmptyTrimmed
        else {
            return content
        }
        let replyContent = content.trimmingCharacters(in: .whitespacesAndNewlines)
        return "回复 @\(replyUsername): \(replyContent)"
    }

    private static func danmuEmotes(from info: [Any]) -> [DanmuEmote] {
        guard let modeInfo = danmuModeInfo(from: info) else { return [] }
        let extra = decodedExtra(from: modeInfo)
        let value = extra?["emots"] ?? modeInfo["emots"]
        guard let rawEmotes = value as? [String: Any] else { return [] }

        return rawEmotes.compactMap { fallbackText, value in
            guard
                let descriptor = value as? [String: Any],
                let rawURL = stringValue(descriptor["url"]),
                let imageURL = secureImageURL(rawURL)
            else {
                return nil
            }
            let text = stringValue(descriptor["emoji"])
                ?? stringValue(descriptor["text"])
                ?? stringValue(descriptor["descript"])
                ?? fallbackText
            guard !text.isEmpty else { return nil }
            return DanmuEmote(
                text: text,
                imageURL: imageURL,
                width: positiveIntValue(descriptor["width"]),
                height: positiveIntValue(descriptor["height"]),
                isAnimated: intValue(descriptor["is_dynamic"]) == 1
            )
        }
        .sorted { $0.text < $1.text }
    }

    private static func danmuModeInfo(from info: [Any]) -> [String: Any]? {
        guard let metadata = safeValue(in: info, at: 0) as? [Any] else { return nil }
        return safeValue(in: metadata, at: 15) as? [String: Any]
    }

    private static func decodedExtra(from modeInfo: [String: Any]) -> [String: Any]? {
        let extra = modeInfo["extra"]
        if let extra = extra as? [String: Any] {
            return extra
        }
        guard
            let encoded = stringValue(extra),
            let data = encoded.data(using: .utf8)
        else {
            return nil
        }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    private static func secureImageURL(_ value: String) -> URL? {
        guard var components = URLComponents(string: value) else { return nil }
        switch components.scheme?.lowercased() {
        case "http":
            components.scheme = "https"
        case "https":
            break
        default:
            return nil
        }
        return components.url
    }

    private static func positiveIntValue(_ value: Any?) -> Int? {
        guard let value = intValue(value), value > 0 else { return nil }
        return value
    }

    private static func replyUsername(from modeInfo: [String: Any]) -> String? {
        if let direct = stringValue(modeInfo["reply_uname"])?.nonEmptyTrimmed {
            return direct
        }
        return stringValue(decodedExtra(from: modeInfo)?["reply_uname"])
    }

    private static func safeValue(in array: [Any], at index: Int) -> Any? {
        array.indices.contains(index) ? array[index] : nil
    }

    private static func stringValue(_ value: Any?) -> String? {
        if let value = value as? String {
            return value
        }
        if let value = value as? NSNumber {
            return value.stringValue
        }
        return nil
    }

    private static func intValue(_ value: Any?) -> Int? {
        if let value = value as? Int {
            return value
        }
        if let value = value as? NSNumber {
            return value.intValue
        }
        if let value = value as? String {
            return Int(value)
        }
        return nil
    }

    private static func usernameFromUserInfo(_ value: Any?) -> String? {
        guard let info = value as? [String: Any] else {
            return stringValue(value)
        }
        return stringValue(info["uname"])
    }

    private static func entryUsername(from data: [String: Any]) -> String? {
        let directName = stringValue(data["uname"])
            ?? stringValue(data["user_name"])
            ?? stringValue(data["username"])
            ?? nestedStringValue(data, path: ["uinfo", "base", "name"])
            ?? nestedStringValue(data, path: ["user", "base", "name"])
            ?? nestedStringValue(data, path: ["user_info", "uname"])

        if let name = directName?.nonEmptyTrimmed {
            return name
        }
        return entryUsernameFromCopywriting(data["copy_writing"])
            ?? entryUsernameFromCopywriting(data["copy_writing_v2"])
    }

    private static func nestedStringValue(_ value: Any?, path: [String]) -> String? {
        guard let key = path.first else {
            return stringValue(value)
        }
        guard let dictionary = value as? [String: Any] else {
            return nil
        }
        return nestedStringValue(dictionary[key], path: Array(path.dropFirst()))
    }

    private static func entryUsernameFromCopywriting(_ value: Any?) -> String? {
        guard let raw = stringValue(value)?.nonEmptyTrimmed else {
            return nil
        }
        if let start = raw.range(of: "<%"),
           let end = raw.range(of: "%>", range: start.upperBound..<raw.endIndex) {
            return String(raw[start.upperBound..<end.lowerBound]).nonEmptyTrimmed
        }
        return raw
            .replacingOccurrences(of: "进入直播间", with: "")
            .replacingOccurrences(of: "来了", with: "")
            .replacingOccurrences(of: "欢迎", with: "")
            .nonEmptyTrimmed
    }

    private static func sourceIdentifier(for raw: [String: Any]) -> String? {
        let data = raw["data"] as? [String: Any]
        return stringValue(data?["id_str"])
            ?? stringValue(data?["message_id"])
            ?? stringValue(data?["transaction_id"])
            ?? stringValue(raw["id"])
    }

    private static func stableIdentifier(for raw: [String: Any]) -> String {
        let sourceID = sourceIdentifier(for: raw)
        if let sourceID, !sourceID.isEmpty {
            return "bili-\(sourceID)"
        }

        let canonical = (try? JSONSerialization.data(withJSONObject: raw, options: [.sortedKeys])) ?? Data()
        var hash: UInt64 = 14_695_981_039_346_656_037
        for byte in canonical {
            hash ^= UInt64(byte)
            hash &*= 1_099_511_628_211
        }
        return "bili-\(String(hash, radix: 16))"
    }
}

private extension String {
    var nonEmptyTrimmed: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
