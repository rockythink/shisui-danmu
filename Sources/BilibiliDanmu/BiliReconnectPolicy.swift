import Foundation

struct BiliReconnectPolicy: Equatable, Sendable {
    let delays: [TimeInterval]

    init(delays: [TimeInterval] = [1, 2, 4, 8, 15, 30]) {
        self.delays = delays.isEmpty ? [1] : delays.map { max(0, $0) }
    }

    func delay(forAttempt attempt: Int) -> TimeInterval {
        delays[min(max(1, attempt) - 1, delays.count - 1)]
    }
}

typealias BiliReconnectSleeper = @Sendable (TimeInterval) async -> Void
