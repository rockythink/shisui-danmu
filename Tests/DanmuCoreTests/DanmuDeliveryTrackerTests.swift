import Foundation
import Testing
@testable import DanmuCore

@Suite("Danmu delivery tracker")
struct DanmuDeliveryTrackerTests {
    @Test func onlyMatchingBroadcasterEchoConfirmsOutgoingDanmu() {
        let submittedAt = Date(timeIntervalSince1970: 100)
        var tracker = DanmuDeliveryTracker()
        tracker.beginAwaitingEcho(
            message: "大家晚上好",
            broadcasterNickname: "石头",
            at: submittedAt
        )

        let sameTextFromViewer = DanmuEvent(
            id: "viewer-event",
            kind: .danmu,
            timestamp: submittedAt.addingTimeInterval(1),
            username: "观众",
            content: "大家晚上好"
        )
        let viewerDidConfirm = tracker.receive(sameTextFromViewer)
        #expect(!viewerDidConfirm)

        let differentTextFromBroadcaster = DanmuEvent(
            id: "other-event",
            kind: .danmu,
            timestamp: submittedAt.addingTimeInterval(2),
            username: "石头",
            content: "另一条弹幕"
        )
        let differentTextDidConfirm = tracker.receive(differentTextFromBroadcaster)
        #expect(!differentTextDidConfirm)

        let matchingEcho = DanmuEvent(
            id: "confirmed-event",
            kind: .danmu,
            timestamp: submittedAt.addingTimeInterval(3),
            username: "石头",
            content: "大家晚上好"
        )
        let matchingEchoDidConfirm = tracker.receive(matchingEcho)
        #expect(matchingEchoDidConfirm)
        #expect(tracker.state == .confirmed(eventID: "confirmed-event"))
    }

    @Test func missingEchoTimesOutAfterConfiguredInterval() {
        let submittedAt = Date(timeIntervalSince1970: 100)
        var tracker = DanmuDeliveryTracker()
        tracker.beginAwaitingEcho(
            message: "等待回流",
            broadcasterNickname: "石头",
            at: submittedAt
        )

        let earlyTimeout = tracker.expireIfNeeded(
            at: submittedAt.addingTimeInterval(14),
            timeout: 15
        )
        #expect(!earlyTimeout)

        let didTimeout = tracker.expireIfNeeded(
            at: submittedAt.addingTimeInterval(15),
            timeout: 15
        )
        #expect(didTimeout)
        #expect(tracker.state == .timedOut(message: "等待回流"))
    }
}
