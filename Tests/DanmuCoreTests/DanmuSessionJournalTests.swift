import Foundation
import Testing
@testable import DanmuCore

@Suite("Danmu session journal")
struct DanmuSessionJournalTests {
    @Test func jsonlUsesIncreasingSequenceAndRepairsCorruptTail() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("shisui-journal-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = LocalJSONLDanmuSessionJournal(sessionsDirectory: directory)
        let sessionID = "session"
        try journal.append(
            sessionID: sessionID,
            kind: .sessionStarted,
            payload: DanmuJournalPayload(roomID: "123"),
            at: Date(timeIntervalSince1970: 1)
        )
        try journal.append(
            sessionID: sessionID,
            kind: .eventReceived,
            payload: DanmuJournalPayload(event: event(id: "e1", content: "第一条")),
            at: Date(timeIntervalSince1970: 2)
        )

        let journalURL = directory
            .appendingPathComponent(sessionID, isDirectory: true)
            .appendingPathComponent("journal.jsonl")
        let handle = try FileHandle(forWritingTo: journalURL)
        try handle.seekToEnd()
        try handle.write(contentsOf: Data(#"{"schemaVersion":1"#.utf8))
        try handle.close()

        #expect(try journal.records(sessionID: sessionID).map(\.sequence) == [1, 2])
        let appended = try journal.append(
            sessionID: sessionID,
            kind: .sessionInterrupted,
            payload: DanmuJournalPayload(),
            at: Date(timeIntervalSince1970: 3)
        )
        #expect(appended.sequence == 3)
        #expect(try journal.records(sessionID: sessionID).count == 3)
    }

    @Test func replayIsIdempotentAndRecoveryStaysDisconnected() throws {
        let journal = InMemoryDanmuSessionJournal()
        let start = try journal.append(
            sessionID: "s1",
            kind: .sessionStarted,
            payload: DanmuJournalPayload(roomID: "123"),
            at: Date(timeIntervalSince1970: 1)
        )
        journal.insertForReplayTesting(start)
        try journal.append(
            sessionID: "s1",
            kind: .eventReceived,
            payload: DanmuJournalPayload(event: event(id: "q", content: "怎么使用？")),
            at: Date(timeIntervalSince1970: 2)
        )
        var live = DanmuSession(id: "s1", roomID: "123", startedAt: Date(timeIntervalSince1970: 1))
        live.ingest(event(id: "q", content: "怎么使用？"))
        try journal.append(
            sessionID: "s1",
            kind: .questionClassified,
            payload: DanmuJournalPayload(question: live.question(for: "q"), isQuestion: true),
            at: Date(timeIntervalSince1970: 2)
        )

        let archive = DanmuSessionArchive(journal: journal)
        let recovered = try archive.recoverLatestInterrupted(at: Date(timeIntervalSince1970: 3))

        #expect(recovered?.status == .interrupted)
        #expect(recovered?.metrics.totalEventCount == 1)
        #expect(recovered?.pendingQuestions.map(\.id) == ["q"])
        #expect(try journal.records(sessionID: "s1").last?.kind == .sessionInterrupted)
    }

    @Test func searchAndExportsRebuildFromJournalWithoutSensitiveFields() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("shisui-archive-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = LocalJSONLDanmuSessionJournal(sessionsDirectory: directory)
        let archive = DanmuSessionArchive(journal: journal)
        let sessionID = "s1"
        let questionEvent = event(id: "q", content: "Ｆｌｉｎｋ 怎么学习？", username: "观众Ａ")
        var session = DanmuSession(id: sessionID, roomID: "123", startedAt: Date(timeIntervalSince1970: 1))
        session.ingest(questionEvent)

        try journal.append(
            sessionID: sessionID,
            kind: .sessionStarted,
            payload: DanmuJournalPayload(roomID: "123"),
            at: Date(timeIntervalSince1970: 1)
        )
        try journal.append(
            sessionID: sessionID,
            kind: .eventReceived,
            payload: DanmuJournalPayload(event: questionEvent),
            at: Date(timeIntervalSince1970: 2)
        )
        try journal.append(
            sessionID: sessionID,
            kind: .questionClassified,
            payload: DanmuJournalPayload(question: session.question(for: "q"), isQuestion: true),
            at: Date(timeIntervalSince1970: 2)
        )
        session.markAnswered(eventID: "q")
        try journal.append(
            sessionID: sessionID,
            kind: .questionStateChanged,
            payload: DanmuJournalPayload(question: session.question(for: "q"), isQuestion: true),
            at: Date(timeIntervalSince1970: 3)
        )
        try journal.append(
            sessionID: sessionID,
            kind: .sessionEnded,
            payload: DanmuJournalPayload(endReason: .completed),
            at: Date(timeIntervalSince1970: 4)
        )

        let results = try archive.search(DanmuArchiveSearchFilter(
            query: "flink",
            roomID: "123",
            questionStatus: .answered
        ))
        let summaryURL = try archive.writeSummary(sessionID: sessionID)
        let snapshotURL = try archive.writeJSONSnapshot(sessionID: sessionID)
        let summary = try String(contentsOf: #require(summaryURL), encoding: .utf8)
        let snapshot = try String(contentsOf: #require(snapshotURL), encoding: .utf8)

        #expect(results.map(\.event.id) == ["q"])
        #expect(summary.contains("## 已回答"))
        #expect(summary.contains("Ｆｌｉｎｋ 怎么学习？"))
        #expect(!snapshot.localizedCaseInsensitiveContains("cookie"))
        #expect(!snapshot.localizedCaseInsensitiveContains("csrf"))
        #expect(!snapshot.localizedCaseInsensitiveContains("token"))
    }

    @Test func manualQuestionCorrectionSurvivesReplay() throws {
        let journal = InMemoryDanmuSessionJournal()
        let automatic = event(id: "automatic", content: "为什么？")
        let manual = event(id: "manual", content: "求展开")
        var session = DanmuSession(id: "s1", roomID: "123")
        session.ingest(automatic)
        session.ingest(manual)

        try journal.append(
            sessionID: "s1",
            kind: .sessionStarted,
            payload: DanmuJournalPayload(roomID: "123")
        )
        for event in [automatic, manual] {
            try journal.append(
                sessionID: "s1",
                kind: .eventReceived,
                payload: DanmuJournalPayload(event: event)
            )
        }
        try journal.append(
            sessionID: "s1",
            kind: .questionClassified,
            payload: DanmuJournalPayload(
                question: session.question(for: automatic.id),
                isQuestion: true,
                eventID: automatic.id
            )
        )
        session.markNotQuestion(eventID: automatic.id)
        try journal.append(
            sessionID: "s1",
            kind: .questionCorrected,
            payload: DanmuJournalPayload(isQuestion: false, eventID: automatic.id)
        )
        session.markAsQuestion(eventID: manual.id)
        try journal.append(
            sessionID: "s1",
            kind: .questionCorrected,
            payload: DanmuJournalPayload(
                question: session.question(for: manual.id),
                isQuestion: true,
                eventID: manual.id
            )
        )

        let replayed = DanmuSessionReplayer.replay(try journal.records(sessionID: "s1"))
        #expect(replayed?.question(for: automatic.id) == nil)
        #expect(replayed?.question(for: manual.id)?.recognition.source == .manual)
    }

    @Test func replayRestoresFeaturedEventThatFellOutOfRecentProjection() throws {
        let journal = InMemoryDanmuSessionJournal()
        let featured = event(id: "old", content: "需要长期保留的重点")
        try journal.append(
            sessionID: "s1",
            kind: .sessionStarted,
            payload: DanmuJournalPayload(roomID: "123")
        )
        try journal.append(
            sessionID: "s1",
            kind: .eventReceived,
            payload: DanmuJournalPayload(event: featured)
        )
        for index in 1...241 {
            try journal.append(
                sessionID: "s1",
                kind: .eventReceived,
                payload: DanmuJournalPayload(event: event(id: "e\(index)", content: "互动 \(index)"))
            )
        }
        try journal.append(
            sessionID: "s1",
            kind: .featuredChanged,
            payload: DanmuJournalPayload(event: featured, featuredEventID: featured.id)
        )

        let replayed = DanmuSessionReplayer.replay(try journal.records(sessionID: "s1"))
        #expect(replayed?.recentEvents.count == 240)
        #expect(replayed?.featuredEvent == featured)
        #expect(replayed?.metrics.totalEventCount == 242)
    }

    private func event(
        id: String,
        content: String,
        username: String = "观众"
    ) -> DanmuEvent {
        DanmuEvent(
            id: id,
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 2),
            username: username,
            content: content
        )
    }
}
