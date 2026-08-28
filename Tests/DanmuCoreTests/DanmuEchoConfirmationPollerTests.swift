import Foundation
import Testing
@testable import DanmuCore

@Suite("Danmu echo confirmation polling")
struct DanmuEchoConfirmationPollerTests {
    @Test func confirmsFromRecentHistoryWhenLiveEchoWasMissed() async {
        let responses = EventResponseQueue([
            [],
            [DanmuEvent(
                id: "history-confirmed",
                kind: .danmu,
                timestamp: Date(timeIntervalSince1970: 101),
                username: "停车拾穗",
                content: "测试回流"
            )],
        ])
        let poller = DanmuEchoConfirmationPoller(
            maximumAttempts: 2,
            fetchRecentEvents: { await responses.next() },
            sleeper: { _ in }
        )

        let event = await poller.waitForEcho(
            message: "测试回流",
            broadcasterNickname: "停车拾穗",
            submittedAt: Date(timeIntervalSince1970: 100)
        )

        #expect(event?.id == "history-confirmed")
        #expect(await responses.fetchCount == 2)
    }

    @Test func neverConfirmsMatchingTextFromAnotherViewer() async {
        let responses = EventResponseQueue([[DanmuEvent(
            id: "viewer-message",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 101),
            username: "其他观众",
            content: "测试回流"
        )]])
        let poller = DanmuEchoConfirmationPoller(
            maximumAttempts: 1,
            fetchRecentEvents: { await responses.next() },
            sleeper: { _ in }
        )

        let event = await poller.waitForEcho(
            message: "测试回流",
            broadcasterNickname: "停车拾穗",
            submittedAt: Date(timeIntervalSince1970: 100)
        )

        #expect(event == nil)
    }

    @Test func ignoresAnIdenticalMessageFromBeforeThisSend() async {
        let responses = EventResponseQueue([[DanmuEvent(
            id: "old-message",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 90),
            username: "停车拾穗",
            content: "重复内容"
        )]])
        let poller = DanmuEchoConfirmationPoller(
            maximumAttempts: 1,
            fetchRecentEvents: { await responses.next() },
            sleeper: { _ in }
        )

        let event = await poller.waitForEcho(
            message: "重复内容",
            broadcasterNickname: "停车拾穗",
            submittedAt: Date(timeIntervalSince1970: 100)
        )

        #expect(event == nil)
    }

    @Test func usesStableAuthorIDWhenAnotherAccountHasTheSameNicknameAndContent() async {
        let submittedAt = Date(timeIntervalSince1970: 100)
        let responses = EventResponseQueue([
            [DanmuEvent(
                id: "same-name-viewer",
                kind: .danmu,
                timestamp: submittedAt.addingTimeInterval(1),
                username: "停车拾穗",
                authorID: "999999",
                content: "测试回流"
            )],
            [DanmuEvent(
                id: "broadcaster",
                kind: .danmu,
                timestamp: submittedAt.addingTimeInterval(2),
                username: "停车拾穗",
                authorID: "7604237",
                content: "测试回流"
            )],
        ])
        let poller = DanmuEchoConfirmationPoller(
            maximumAttempts: 2,
            fetchRecentEvents: { await responses.next() },
            sleeper: { _ in }
        )

        let event = await poller.waitForEcho(
            message: "测试回流",
            broadcasterNickname: "停车拾穗",
            broadcasterAuthorID: "7604237",
            submittedAt: submittedAt
        )

        #expect(event?.id == "broadcaster")
        #expect(await responses.fetchCount == 2)
    }
}

private actor EventResponseQueue {
    private var responses: [[DanmuEvent]]
    private(set) var fetchCount = 0

    init(_ responses: [[DanmuEvent]]) {
        self.responses = responses
    }

    func next() -> [DanmuEvent] {
        fetchCount += 1
        guard !responses.isEmpty else { return [] }
        return responses.removeFirst()
    }
}
