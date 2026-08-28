import Foundation

public struct BroadcasterIdentity: Equatable, Sendable {
    public let nickname: String
    public let authorID: String?

    public init?(nickname: String?, authorID: String?) {
        guard let nickname = nickname?.trimmedNonEmpty else { return nil }
        self.nickname = nickname
        self.authorID = authorID?.trimmedNonEmpty
    }

    public func matches(_ event: DanmuEvent) -> Bool {
        guard event.kind == .danmu else { return false }

        if let authorID, let eventAuthorID = event.authorID?.usableAuthorID {
            return eventAuthorID == authorID
        }
        guard let username = event.username?.trimmedNonEmpty else { return false }
        if username.caseInsensitiveCompare(nickname) == .orderedSame {
            return true
        }

        // 脱敏昵称（例如“停***”）不是稳定身份：多个用户可能拥有相同前缀，
        // 不能仅凭它把一条普通实时弹幕认成主播。发送回流可在调用方结合
        // 内容与时间窗口单独确认，普通展示这里只接受 uid 或完整昵称。
        return false
    }

    public func canonicalizing(_ event: DanmuEvent) -> DanmuEvent {
        guard matches(event), event.username != nickname else { return event }
        return DanmuEvent(
            id: event.id,
            kind: event.kind,
            timestamp: event.timestamp,
            username: nickname,
            authorID: event.authorID ?? authorID,
            content: event.content,
            origin: event.origin,
            platformEventID: event.platformEventID
        )
    }
}

private extension String {
    var trimmedNonEmpty: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }

    var usableAuthorID: String? {
        guard let value = trimmedNonEmpty, value != "0" else { return nil }
        return value
    }
}
