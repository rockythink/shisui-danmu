import Foundation

public struct DanmuEchoConfirmationPoller: Sendable {
    public typealias RecentEventsFetcher = @Sendable () async -> [DanmuEvent]
    public typealias Sleeper = @Sendable (TimeInterval) async -> Void

    private let maximumAttempts: Int
    private let pollInterval: TimeInterval
    private let fetchRecentEvents: RecentEventsFetcher
    private let sleeper: Sleeper

    public init(
        maximumAttempts: Int = 8,
        pollInterval: TimeInterval = 2,
        fetchRecentEvents: @escaping RecentEventsFetcher,
        sleeper: @escaping Sleeper = { delay in
            guard delay > 0 else { return }
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
        }
    ) {
        self.maximumAttempts = max(1, maximumAttempts)
        self.pollInterval = max(0, pollInterval)
        self.fetchRecentEvents = fetchRecentEvents
        self.sleeper = sleeper
    }

    public func waitForEcho(
        message: String,
        broadcasterNickname: String,
        broadcasterAuthorID: String? = nil,
        submittedAt: Date
    ) async -> DanmuEvent? {
        var tracker = DanmuDeliveryTracker()
        tracker.beginAwaitingEcho(
            message: message,
            broadcasterNickname: broadcasterNickname,
            at: submittedAt
        )

        for attempt in 0..<maximumAttempts {
            guard !Task.isCancelled else { return nil }
            let recentEvents = await fetchRecentEvents()
            guard !Task.isCancelled else { return nil }
            for event in recentEvents where event.timestamp >= submittedAt.addingTimeInterval(-2) {
                if let broadcasterAuthorID = broadcasterAuthorID?.nonEmptyTrimmed {
                    guard event.authorID?.nonEmptyTrimmed == broadcasterAuthorID else { continue }
                }
                if tracker.receive(event) {
                    return event
                }
            }
            if attempt < maximumAttempts - 1 {
                await sleeper(pollInterval)
            }
        }
        return nil
    }
}

private extension String {
    var nonEmptyTrimmed: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
