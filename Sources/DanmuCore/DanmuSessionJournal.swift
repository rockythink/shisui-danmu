import Foundation

public enum DanmuJournalRecordKind: String, Codable, Sendable {
    case sessionStarted
    case sessionInterrupted
    case sessionResumed
    case sessionEnded
    case eventReceived
    case questionClassified
    case questionCorrected
    case questionStateChanged
    case questionPriorityChanged
    case featuredChanged
}

public struct DanmuJournalPayload: Codable, Equatable, Sendable {
    public var roomID: String?
    public var event: DanmuEvent?
    public var question: QuestionRecord?
    public var isQuestion: Bool?
    public var eventID: String?
    public var featuredEventID: String?
    public var endReason: DanmuSessionEndReason?
    public var classificationReason: String?

    public init(
        roomID: String? = nil,
        event: DanmuEvent? = nil,
        question: QuestionRecord? = nil,
        isQuestion: Bool? = nil,
        eventID: String? = nil,
        featuredEventID: String? = nil,
        endReason: DanmuSessionEndReason? = nil,
        classificationReason: String? = nil
    ) {
        self.roomID = roomID
        self.event = event
        self.question = question
        self.isQuestion = isQuestion
        self.eventID = eventID
        self.featuredEventID = featuredEventID
        self.endReason = endReason
        self.classificationReason = classificationReason
    }
}

public struct DanmuJournalRecord: Codable, Equatable, Identifiable, Sendable {
    public let schemaVersion: Int
    public let recordID: String
    public var id: String { recordID }
    public let sessionID: String
    public let sequence: Int
    public let recordedAt: Date
    public let kind: DanmuJournalRecordKind
    public let payload: DanmuJournalPayload

    public init(
        recordID: String = UUID().uuidString.lowercased(),
        sessionID: String,
        sequence: Int,
        recordedAt: Date = .now,
        kind: DanmuJournalRecordKind,
        payload: DanmuJournalPayload = DanmuJournalPayload()
    ) {
        self.schemaVersion = 1
        self.recordID = recordID
        self.sessionID = sessionID
        self.sequence = sequence
        self.recordedAt = recordedAt
        self.kind = kind
        self.payload = payload
    }
}

public protocol DanmuSessionJournal: Sendable {
    @discardableResult
    func append(
        sessionID: String,
        kind: DanmuJournalRecordKind,
        payload: DanmuJournalPayload,
        at date: Date
    ) throws -> DanmuJournalRecord
    func records(sessionID: String) throws -> [DanmuJournalRecord]
    func sessionIDs() throws -> [String]
    func artifactDirectory(sessionID: String) throws -> URL?
}

public extension DanmuSessionJournal {
    @discardableResult
    func append(
        sessionID: String,
        kind: DanmuJournalRecordKind,
        payload: DanmuJournalPayload = DanmuJournalPayload(),
        at date: Date = .now
    ) throws -> DanmuJournalRecord {
        try append(sessionID: sessionID, kind: kind, payload: payload, at: date)
    }
}

public final class InMemoryDanmuSessionJournal: DanmuSessionJournal, @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [String: [DanmuJournalRecord]] = [:]

    public init() {}

    @discardableResult
    public func append(
        sessionID: String,
        kind: DanmuJournalRecordKind,
        payload: DanmuJournalPayload,
        at date: Date
    ) throws -> DanmuJournalRecord {
        lock.lock()
        defer { lock.unlock() }
        let sequence = (storage[sessionID]?.map(\.sequence).max() ?? 0) + 1
        let record = DanmuJournalRecord(
            sessionID: sessionID,
            sequence: sequence,
            recordedAt: date,
            kind: kind,
            payload: payload
        )
        storage[sessionID, default: []].append(record)
        return record
    }

    public func records(sessionID: String) throws -> [DanmuJournalRecord] {
        lock.lock()
        defer { lock.unlock() }
        return storage[sessionID] ?? []
    }

    public func sessionIDs() throws -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return storage.keys.sorted()
    }

    public func artifactDirectory(sessionID: String) throws -> URL? { nil }

    public func insertForReplayTesting(_ record: DanmuJournalRecord) {
        lock.lock()
        defer { lock.unlock() }
        storage[record.sessionID, default: []].append(record)
    }
}

public enum LocalDanmuSessionJournalError: Error {
    case corruptRecord
}

public final class LocalJSONLDanmuSessionJournal: DanmuSessionJournal, @unchecked Sendable {
    public let sessionsDirectory: URL
    private let fileManager: FileManager
    private let lock = NSLock()
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder
    private var lastSequenceBySession: [String: Int] = [:]

    public init(sessionsDirectory: URL, fileManager: FileManager = .default) {
        self.sessionsDirectory = sessionsDirectory
        self.fileManager = fileManager
        encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys]
        decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
    }

    public convenience init(bundleIdentifier: String) throws {
        let root = try FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        self.init(sessionsDirectory: root
            .appendingPathComponent(bundleIdentifier, isDirectory: true)
            .appendingPathComponent("Sessions", isDirectory: true))
    }

    @discardableResult
    public func append(
        sessionID: String,
        kind: DanmuJournalRecordKind,
        payload: DanmuJournalPayload,
        at date: Date
    ) throws -> DanmuJournalRecord {
        lock.lock()
        defer { lock.unlock() }
        try ensureSessionDirectory(sessionID)
        let lastSequence: Int
        let needsLeadingNewline: Bool
        if let cached = lastSequenceBySession[sessionID] {
            lastSequence = cached
            needsLeadingNewline = false
        } else {
            let existing = try readRecordsUnlocked(sessionID: sessionID, repairTail: true)
            lastSequence = existing.map(\.sequence).max() ?? 0
            let existingData = try? Data(contentsOf: journalURL(sessionID))
            needsLeadingNewline = existingData?.isEmpty == false && existingData?.last != 0x0A
        }
        let sequence = lastSequence + 1
        let record = DanmuJournalRecord(
            sessionID: sessionID,
            sequence: sequence,
            recordedAt: date,
            kind: kind,
            payload: payload
        )
        var line = try encoder.encode(record)
        line.append(0x0A)
        if needsLeadingNewline {
            line.insert(0x0A, at: 0)
        }
        let url = journalURL(sessionID)
        if !fileManager.fileExists(atPath: url.path) {
            fileManager.createFile(atPath: url.path, contents: nil)
        }
        let handle = try FileHandle(forWritingTo: url)
        defer { try? handle.close() }
        try handle.seekToEnd()
        try handle.write(contentsOf: line)
        if kind != .eventReceived {
            try handle.synchronize()
        }
        lastSequenceBySession[sessionID] = sequence
        return record
    }

    public func records(sessionID: String) throws -> [DanmuJournalRecord] {
        lock.lock()
        defer { lock.unlock() }
        let records = try readRecordsUnlocked(sessionID: sessionID, repairTail: true)
        lastSequenceBySession[sessionID] = records.map(\.sequence).max() ?? 0
        return records
    }

    public func sessionIDs() throws -> [String] {
        lock.lock()
        defer { lock.unlock() }
        guard fileManager.fileExists(atPath: sessionsDirectory.path) else { return [] }
        return try fileManager.contentsOfDirectory(
            at: sessionsDirectory,
            includingPropertiesForKeys: [.isDirectoryKey],
            options: [.skipsHiddenFiles]
        ).filter { url in
            (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true
                && fileManager.fileExists(atPath: url.appendingPathComponent("journal.jsonl").path)
        }.map(\.lastPathComponent).sorted()
    }

    public func artifactDirectory(sessionID: String) throws -> URL? {
        lock.lock()
        defer { lock.unlock() }
        try ensureSessionDirectory(sessionID)
        return sessionDirectory(sessionID)
    }

    private func readRecordsUnlocked(
        sessionID: String,
        repairTail: Bool
    ) throws -> [DanmuJournalRecord] {
        let url = journalURL(sessionID)
        guard fileManager.fileExists(atPath: url.path) else { return [] }
        let data = try Data(contentsOf: url)
        var records: [DanmuJournalRecord] = []
        var lineStart = 0
        var lastCompleteOffset = 0

        for index in data.indices where data[index] == 0x0A {
            let line = data.subdata(in: lineStart..<index)
            if !line.isEmpty {
                guard let record = try? decoder.decode(DanmuJournalRecord.self, from: line) else {
                    throw LocalDanmuSessionJournalError.corruptRecord
                }
                records.append(record)
            }
            lineStart = index + 1
            lastCompleteOffset = lineStart
        }

        if lineStart < data.count {
            let tail = data.subdata(in: lineStart..<data.count)
            if let record = try? decoder.decode(DanmuJournalRecord.self, from: tail) {
                records.append(record)
            } else if repairTail {
                try data.prefix(lastCompleteOffset).write(to: url, options: .atomic)
            } else {
                throw LocalDanmuSessionJournalError.corruptRecord
            }
        }
        return records
    }

    private func ensureSessionDirectory(_ sessionID: String) throws {
        try fileManager.createDirectory(
            at: sessionDirectory(sessionID),
            withIntermediateDirectories: true
        )
    }

    private func sessionDirectory(_ sessionID: String) -> URL {
        sessionsDirectory.appendingPathComponent(sessionID, isDirectory: true)
    }

    private func journalURL(_ sessionID: String) -> URL {
        sessionDirectory(sessionID).appendingPathComponent("journal.jsonl")
    }
}

public enum DanmuSessionReplayer {
    public static func replay(_ records: [DanmuJournalRecord]) -> DanmuSession? {
        var session: DanmuSession?
        var appliedRecordIDs: Set<String> = []

        for record in records.sorted(by: recordOrder) {
            guard record.schemaVersion == 1, appliedRecordIDs.insert(record.recordID).inserted else {
                continue
            }
            switch record.kind {
            case .sessionStarted:
                guard session == nil, let roomID = record.payload.roomID else { continue }
                session = DanmuSession(
                    id: record.sessionID,
                    roomID: roomID,
                    startedAt: record.recordedAt
                )
            case .sessionInterrupted:
                session?.interrupt()
            case .sessionResumed:
                session?.resume()
            case .sessionEnded:
                session?.end(at: record.recordedAt, reason: record.payload.endReason ?? .completed)
            case .eventReceived:
                if let event = record.payload.event {
                    _ = session?.ingest(event, recognition: nil)
                }
            case .questionClassified, .questionCorrected,
                 .questionStateChanged, .questionPriorityChanged:
                if record.payload.isQuestion == false, let eventID = record.payload.eventID {
                    session?.markNotQuestion(eventID: eventID)
                } else if let question = record.payload.question {
                    session?.restoreQuestionRecord(question)
                }
            case .featuredChanged:
                if let event = record.payload.event {
                    session?.feature(event)
                } else {
                    session?.feature(eventID: record.payload.featuredEventID)
                }
            }
        }
        return session
    }

    private static func recordOrder(_ lhs: DanmuJournalRecord, _ rhs: DanmuJournalRecord) -> Bool {
        if lhs.sequence != rhs.sequence { return lhs.sequence < rhs.sequence }
        return lhs.recordedAt < rhs.recordedAt
    }
}

public struct DanmuArchiveSearchFilter: Sendable {
    public var query: String
    public var roomID: String?
    public var dateRange: ClosedRange<Date>?
    public var questionStatus: QuestionStatus?

    public init(
        query: String = "",
        roomID: String? = nil,
        dateRange: ClosedRange<Date>? = nil,
        questionStatus: QuestionStatus? = nil
    ) {
        self.query = query
        self.roomID = roomID
        self.dateRange = dateRange
        self.questionStatus = questionStatus
    }
}

public struct DanmuArchiveSearchResult: Identifiable, Equatable, Sendable {
    public var id: String { "\(sessionID):\(event.id)" }
    public let sessionID: String
    public let roomID: String
    public let event: DanmuEvent
    public let questionStatus: QuestionStatus?
    public let isCurrentSession: Bool
}

public struct DanmuSessionJSONSnapshot: Codable, Equatable, Sendable {
    public let sessionID: String
    public let roomID: String
    public let startedAt: Date
    public let endedAt: Date?
    public let status: DanmuSessionStatus
    public let metrics: SessionMetrics
    public let events: [DanmuEvent]
    public let questions: [QuestionRecord]
    public let featuredEvent: DanmuEvent?
}

public final class DanmuSessionArchive: @unchecked Sendable {
    public let journal: any DanmuSessionJournal

    public init(journal: any DanmuSessionJournal) {
        self.journal = journal
    }

    public func recoverLatestInterrupted(at date: Date = .now) throws -> DanmuSession? {
        let candidates = try journal.sessionIDs().compactMap { sessionID -> (DanmuSession, Date)? in
            let records = try journal.records(sessionID: sessionID)
            guard let session = DanmuSessionReplayer.replay(records), session.status != .ended else {
                return nil
            }
            return (session, records.last?.recordedAt ?? session.startedAt)
        }
        guard var recovered = candidates.max(by: { $0.1 < $1.1 })?.0 else { return nil }
        if recovered.status == .active {
            try journal.append(sessionID: recovered.id, kind: .sessionInterrupted, at: date)
            recovered.interrupt()
        }
        return recovered
    }

    public func search(
        _ filter: DanmuArchiveSearchFilter,
        currentSessionID: String? = nil
    ) throws -> [DanmuArchiveSearchResult] {
        let query = QuestionClassifier.normalized(filter.query)
        var results: [DanmuArchiveSearchResult] = []
        for sessionID in try journal.sessionIDs() {
            let records = try journal.records(sessionID: sessionID)
            guard let session = DanmuSessionReplayer.replay(records) else { continue }
            guard filter.roomID.map({ $0 == session.roomID }) ?? true else { continue }
            let questionStatuses = Dictionary(
                uniqueKeysWithValues: session.questionRecords.map { ($0.eventID, $0.status) }
            )
            for event in allEvents(records) {
                let status = questionStatuses[event.id]
                guard filter.questionStatus.map({ $0 == status }) ?? true else { continue }
                guard filter.dateRange.map({ $0.contains(event.timestamp) }) ?? true else { continue }
                let haystack = QuestionClassifier.normalized("\(event.username ?? "") \(event.content)")
                guard query.isEmpty || haystack.contains(query) else { continue }
                results.append(DanmuArchiveSearchResult(
                    sessionID: sessionID,
                    roomID: session.roomID,
                    event: event,
                    questionStatus: status,
                    isCurrentSession: sessionID == currentSessionID
                ))
            }
        }
        return results.sorted { $0.event.timestamp > $1.event.timestamp }
    }

    @discardableResult
    public func writeSummary(sessionID: String) throws -> URL? {
        let records = try journal.records(sessionID: sessionID)
        let markdown = try markdownSummary(records: records)
        guard let directory = try journal.artifactDirectory(sessionID: sessionID) else { return nil }
        let url = directory.appendingPathComponent("summary.md")
        try Data(markdown.utf8).write(to: url, options: .atomic)
        return url
    }

    public func markdownSummary(records: [DanmuJournalRecord]) throws -> String {
        guard let session = DanmuSessionReplayer.replay(records) else { return "" }
        let groups: [(String, [QuestionRecord])] = [
            ("当前回答", session.questionRecords.filter { $0.status == .answering }),
            ("重点问题", session.questionRecords.filter { $0.priority == .high }),
            ("已回答", session.questionRecords.filter { $0.status == .answered }),
            ("待回答", session.questionRecords.filter { $0.status == .pending }),
            ("已跳过", session.questionRecords.filter { $0.status == .skipped }),
        ]
        var lines = [
            "# 直播复盘 · 房间 \(session.roomID)",
            "",
            "- 会话 ID：`\(session.id)`",
            "- 本场事件：\(session.metrics.totalEventCount)",
            "- 状态：\(session.status.rawValue)",
            "",
        ]
        if let featured = session.featuredEvent {
            lines += ["## 当前重点", "", "- \(render(featured))", ""]
        }
        for (title, questions) in groups {
            lines += ["## \(title)", ""]
            lines += questions.isEmpty ? ["- 无"] : questions.map { "- \(render($0.event))" }
            lines.append("")
        }
        let eventsByID = Dictionary(uniqueKeysWithValues: allEvents(records).map { ($0.id, $0) })
        let featuredHistory = records.compactMap { record -> DanmuEvent? in
            guard record.kind == .featuredChanged else { return nil }
            return record.payload.featuredEventID.flatMap { eventsByID[$0] }
        }
        lines += ["## 重点上屏记录", ""]
        lines += featuredHistory.isEmpty ? ["- 无"] : featuredHistory.map { "- \(render($0))" }
        lines.append("")
        return lines.joined(separator: "\n")
    }

    @discardableResult
    public func writeJSONSnapshot(sessionID: String, to destination: URL? = nil) throws -> URL? {
        let records = try journal.records(sessionID: sessionID)
        guard let snapshot = snapshot(records: records) else { return nil }
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let target: URL
        if let destination {
            target = destination
        } else if let directory = try journal.artifactDirectory(sessionID: sessionID) {
            target = directory.appendingPathComponent("snapshot.json")
        } else {
            return nil
        }
        try encoder.encode(snapshot).write(to: target, options: .atomic)
        return target
    }

    public func snapshot(records: [DanmuJournalRecord]) -> DanmuSessionJSONSnapshot? {
        guard let session = DanmuSessionReplayer.replay(records) else { return nil }
        return DanmuSessionJSONSnapshot(
            sessionID: session.id,
            roomID: session.roomID,
            startedAt: session.startedAt,
            endedAt: session.endedAt,
            status: session.status,
            metrics: session.metrics,
            events: allEvents(records),
            questions: session.questionRecords,
            featuredEvent: session.featuredEvent
        )
    }

    private func allEvents(_ records: [DanmuJournalRecord]) -> [DanmuEvent] {
        var seen: Set<String> = []
        return records.sorted { $0.sequence < $1.sequence }.compactMap { record in
            guard record.kind == .eventReceived,
                  let event = record.payload.event,
                  seen.insert(event.id).inserted else { return nil }
            return event
        }
    }

    private func render(_ event: DanmuEvent) -> String {
        "\(event.username ?? "系统")：\(event.content)"
    }
}
