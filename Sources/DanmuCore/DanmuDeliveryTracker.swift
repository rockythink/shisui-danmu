import Foundation

public struct DanmuDeliveryTracker: Sendable {
    public enum State: Equatable, Sendable {
        case idle
        case awaitingEcho(message: String, broadcasterNickname: String, submittedAt: Date)
        case confirmed(eventID: String)
        case timedOut(message: String)
    }

    public private(set) var state: State

    public init() {
        self.state = .idle
    }

    public mutating func beginAwaitingEcho(
        message: String,
        broadcasterNickname: String,
        at date: Date = .now
    ) {
        state = .awaitingEcho(
            message: message.trimmingCharacters(in: .whitespacesAndNewlines),
            broadcasterNickname: broadcasterNickname.trimmingCharacters(in: .whitespacesAndNewlines),
            submittedAt: date
        )
    }

    @discardableResult
    public mutating func receive(_ event: DanmuEvent) -> Bool {
        guard case .awaitingEcho(let message, let broadcasterNickname, _) = state,
              Self.isBroadcasterMessage(event, nickname: broadcasterNickname),
              normalized(event.content) == normalized(message) else {
            return false
        }
        state = .confirmed(eventID: event.id)
        return true
    }

    @discardableResult
    public mutating func expireIfNeeded(at date: Date = .now, timeout: TimeInterval) -> Bool {
        guard case .awaitingEcho(let message, _, let submittedAt) = state,
              date.timeIntervalSince(submittedAt) >= max(0, timeout) else {
            return false
        }
        state = .timedOut(message: message)
        return true
    }

    public static func isBroadcasterMessage(_ event: DanmuEvent, nickname: String?) -> Bool {
        guard event.kind == .danmu,
              let nickname,
              let username = event.username else {
            return false
        }
        return normalized(username).caseInsensitiveCompare(normalized(nickname)) == .orderedSame
    }

    private static func normalized(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func normalized(_ value: String) -> String {
        Self.normalized(value)
    }
}
