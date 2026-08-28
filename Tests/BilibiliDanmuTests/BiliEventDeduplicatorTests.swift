import DanmuCore
import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili history and live deduplication")
struct BiliEventDeduplicatorTests {
    @Test func mergesMatchingHistoryAndLiveWithinThreeSeconds() {
        var deduplicator = BiliEventDeduplicator()
        let history = event(id: "history", origin: .history, at: 100)
        let live = event(id: "live", origin: .live, at: 102)

        let emittedHistory = deduplicator.shouldEmit(history)
        let emittedLive = deduplicator.shouldEmit(live)
        #expect(emittedHistory)
        #expect(!emittedLive)
    }

    @Test func doesNotMergeRepeatedLiveMessagesOrDistantHistory() {
        var deduplicator = BiliEventDeduplicator()
        let first = deduplicator.shouldEmit(event(id: "live-1", origin: .live, at: 100))
        let second = deduplicator.shouldEmit(event(id: "live-2", origin: .live, at: 101))
        let distant = deduplicator.shouldEmit(event(id: "history", origin: .history, at: 105))
        #expect(first)
        #expect(second)
        #expect(distant)
    }

    @Test func stableAuthorIDMergesMaskedLiveAndFullHistoryNames() {
        var deduplicator = BiliEventDeduplicator()
        let history = DanmuEvent(
            id: "history",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 100),
            username: "停车拾穗",
            authorID: "7604237",
            content: "测试一下弹幕",
            origin: .history
        )
        let live = DanmuEvent(
            id: "live",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 101),
            username: "停***",
            authorID: "7604237",
            content: "测试一下弹幕",
            origin: .live
        )

        let emittedHistory = deduplicator.shouldEmit(history)
        let emittedLive = deduplicator.shouldEmit(live)
        #expect(emittedHistory)
        #expect(!emittedLive)
    }

    @Test func placeholderLiveAuthorIDStillMergesWithMatchingFullHistoryIdentity() {
        var deduplicator = BiliEventDeduplicator()
        let history = DanmuEvent(
            id: "history-placeholder",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 100),
            username: "Darkkk丶",
            authorID: "153499714",
            content: "测试",
            origin: .history
        )
        let live = DanmuEvent(
            id: "live-placeholder",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 101),
            username: "D***",
            authorID: "0",
            content: "测试",
            origin: .live
        )

        let emittedHistory = deduplicator.shouldEmit(history)
        let emittedLive = deduplicator.shouldEmit(live)
        #expect(emittedHistory)
        #expect(!emittedLive)
    }

    private func event(id: String, origin: DanmuEventOrigin, at seconds: TimeInterval) -> DanmuEvent {
        DanmuEvent(
            id: id,
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: seconds),
            username: "观众Ａ",
            content: "同一条弹幕",
            origin: origin
        )
    }
}
