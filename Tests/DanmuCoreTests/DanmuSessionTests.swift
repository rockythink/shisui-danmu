import Foundation
import Testing
@testable import DanmuCore

@Suite("Danmu session")
struct DanmuSessionTests {
    @Test func ingestDeduplicatesEventsAndCreatesPendingQuestion() {
        var session = DanmuSession(roomID: "123")
        let question = makeEvent(id: "q1", kind: .danmu, content: "为什么？")

        let firstIngest = session.ingest(question)
        let duplicateIngest = session.ingest(question)

        #expect(firstIngest)
        #expect(!duplicateIngest)
        #expect(session.recentEvents == [question])
        #expect(session.pendingQuestions == [question])
        #expect(session.disposition(for: question.id) == .pending)
        #expect(session.metrics.totalEventCount == 1)
    }

    @Test func questionDispositionIsMutuallyExclusiveAndClearsFeaturedEvent() {
        var session = DanmuSession(roomID: "123")
        let question = makeEvent(id: "q1", kind: .danmu, content: "为什么？")
        session.ingest(question)
        session.feature(eventID: question.id)

        session.markAnswered(eventID: question.id)

        #expect(session.disposition(for: question.id) == .answered)
        #expect(session.featuredEvent == nil)

        session.skipQuestion(eventID: question.id)
        #expect(session.disposition(for: question.id) == .skipped)
    }

    @Test func recentEventLimitDoesNotRemoveQuestionFeaturedOrMetrics() {
        var session = DanmuSession(roomID: "123", eventLimit: 1)
        let oldQuestion = makeEvent(id: "q1", kind: .danmu, content: "这个功能怎么使用？")
        session.ingest(oldQuestion)
        session.feature(eventID: oldQuestion.id)

        let latestDanmu = makeEvent(id: "d1", kind: .danmu)
        session.ingest(latestDanmu)

        #expect(session.recentEvents == [latestDanmu])
        #expect(session.disposition(for: oldQuestion.id) == .pending)
        #expect(session.featuredEvent == oldQuestion)
        #expect(session.metrics.totalEventCount == 2)
    }

    @Test func endedSessionRejectsNewEventsAndRecordsEndTime() {
        let startedAt = Date(timeIntervalSince1970: 1_700_000_000)
        let endedAt = startedAt.addingTimeInterval(600)
        var session = DanmuSession(roomID: "123", startedAt: startedAt)

        #expect(session.status == .active)
        session.end(at: endedAt)
        let acceptedLateEvent = session.ingest(makeEvent(id: "late", kind: .danmu))

        #expect(session.status == .ended)
        #expect(session.endedAt == endedAt)
        #expect(!acceptedLateEvent)
        #expect(session.recentEvents.isEmpty)
    }

    @Test func queueUsesHighPriorityThenFIFOAndPostponeMovesToTail() {
        var session = DanmuSession(roomID: "123")
        let first = makeEvent(id: "first", kind: .danmu, content: "第一个问题？")
        let second = makeEvent(id: "second", kind: .danmu, content: "第二个问题？")
        let superchat = makeEvent(id: "sc", kind: .superchat, content: "SC 问题？")
        session.ingest(first)
        session.ingest(second)
        session.ingest(superchat)

        #expect(session.pendingQuestions.map(\.id) == ["sc", "first", "second"])
        session.postpone(eventID: first.id)
        #expect(session.pendingQuestions.map(\.id) == ["sc", "second", "first"])
        session.setHighPriority(true, eventID: second.id)
        #expect(session.pendingQuestions.map(\.id) == ["second", "sc", "first"])
        session.setHighPriority(false, eventID: superchat.id)
        #expect(session.pendingQuestions.map(\.id) == ["second", "sc", "first"])
    }

    @Test func answerAndAdvanceChangesCurrentQuestionAtomically() {
        var session = DanmuSession(roomID: "123")
        let first = makeEvent(id: "first", kind: .danmu, content: "第一个问题？")
        let second = makeEvent(id: "second", kind: .danmu, content: "第二个问题？")
        session.ingest(first)
        session.ingest(second)
        session.startAnswering(eventID: first.id)

        let advanced = session.answerAndAdvance(eventID: first.id)

        #expect(session.disposition(for: first.id) == .answered)
        #expect(session.disposition(for: second.id) == .answering)
        #expect(advanced == second)
        #expect(session.featuredEvent == second)
    }

    @Test func moreThanTwoHundredFortyEventsKeepOldQuestionAndCumulativeCount() {
        var session = DanmuSession(roomID: "123")
        let oldQuestion = makeEvent(id: "q0", kind: .danmu, content: "最早的问题？")
        session.ingest(oldQuestion)
        session.startAnswering(eventID: oldQuestion.id)

        for index in 1...241 {
            session.ingest(makeEvent(id: "d\(index)", kind: .danmu, content: "普通互动 \(index)"))
        }

        #expect(session.recentEvents.count == 240)
        #expect(!session.recentEvents.contains(oldQuestion))
        #expect(session.disposition(for: oldQuestion.id) == .answering)
        #expect(session.featuredEvent == oldQuestion)
        #expect(session.metrics.totalEventCount == 242)
    }

    private func makeEvent(
        id: String,
        kind: DanmuEventKind,
        content: String = "这是一条测试互动"
    ) -> DanmuEvent {
        DanmuEvent(
            id: id,
            kind: kind,
            timestamp: Date(timeIntervalSince1970: 1_700_000_000),
            username: "测试观众",
            content: content
        )
    }
}
