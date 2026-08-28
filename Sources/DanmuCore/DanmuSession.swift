import Foundation

public enum DanmuSessionStatus: String, Codable, Sendable {
    case active
    case interrupted
    case ended
}

public enum DanmuSessionEndReason: String, Codable, Sendable {
    case completed
    case replaced
}

public struct DanmuSession: Equatable, Sendable {
    public let id: String
    public let roomID: String
    public let startedAt: Date
    public private(set) var endedAt: Date?
    public private(set) var status: DanmuSessionStatus
    public private(set) var endReason: DanmuSessionEndReason?
    public private(set) var recentEvents: [DanmuEvent]
    public private(set) var metrics: SessionMetrics
    public private(set) var featuredEvent: DanmuEvent?
    public private(set) var questionRecords: [QuestionRecord]
    public let eventLimit: Int
    public var questionClassifier: QuestionClassifier

    private var seenEventIDs: Set<String>
    private var nextQueueSequence: Int

    public init(
        id: String = UUID().uuidString.lowercased(),
        roomID: String,
        eventLimit: Int = 240,
        startedAt: Date = .now,
        questionClassifier: QuestionClassifier = QuestionClassifier()
    ) {
        self.id = id
        self.roomID = roomID
        self.startedAt = startedAt
        self.endedAt = nil
        self.status = .active
        self.endReason = nil
        self.recentEvents = []
        self.metrics = SessionMetrics()
        self.featuredEvent = nil
        self.questionRecords = []
        self.eventLimit = max(1, eventLimit)
        self.questionClassifier = questionClassifier
        self.seenEventIDs = []
        self.nextQueueSequence = 0
    }

    @discardableResult
    public mutating func ingest(_ event: DanmuEvent) -> Bool {
        ingest(event, recognition: questionClassifier.classify(event))
    }

    @discardableResult
    public mutating func ingest(_ event: DanmuEvent, recognition: QuestionRecognition?) -> Bool {
        guard status == .active, seenEventIDs.insert(event.id).inserted else { return false }

        recentEvents.insert(event, at: 0)
        if recentEvents.count > eventLimit {
            recentEvents.removeLast(recentEvents.count - eventLimit)
        }
        metrics.record(event)

        if let recognition {
            addQuestion(event: event, recognition: recognition)
        }
        return true
    }

    public mutating func interrupt() {
        guard status == .active else { return }
        status = .interrupted
    }

    public mutating func resume() {
        guard status == .interrupted else { return }
        status = .active
    }

    public mutating func end(
        at date: Date = .now,
        reason: DanmuSessionEndReason = .completed
    ) {
        guard status != .ended else { return }
        status = .ended
        endedAt = max(date, startedAt)
        endReason = reason
    }

    public var questions: [DanmuEvent] {
        orderedQuestionRecords.map(\.event)
    }

    public var pendingQuestions: [DanmuEvent] {
        orderedQuestionRecords.filter { $0.status == .pending }.map(\.event)
    }

    public var answeringQuestion: DanmuEvent? {
        questionRecords.first(where: { $0.status == .answering })?.event
    }

    public func question(for eventID: String) -> QuestionRecord? {
        questionRecords.first { $0.eventID == eventID }
    }

    public func disposition(for eventID: String) -> QuestionStatus? {
        question(for: eventID)?.status
    }

    public mutating func markAsQuestion(eventID: String, reason: String = "主播手动标记") {
        guard question(for: eventID) == nil, let event = event(for: eventID) else { return }
        markAsQuestion(event, reason: reason)
    }

    public mutating func markAsQuestion(_ event: DanmuEvent, reason: String = "主播手动标记") {
        guard question(for: event.id) == nil else { return }
        addQuestion(event: event, recognition: QuestionRecognition(source: .manual, reason: reason))
    }

    public mutating func classifyQuestion(
        eventID: String,
        recognition: QuestionRecognition
    ) {
        guard question(for: eventID) == nil, let event = event(for: eventID) else { return }
        addQuestion(event: event, recognition: recognition)
    }

    public mutating func restoreQuestionRecord(_ record: QuestionRecord) {
        if let index = questionRecords.firstIndex(where: { $0.eventID == record.eventID }) {
            questionRecords[index] = record
        } else {
            questionRecords.append(record)
        }
        nextQueueSequence = max(nextQueueSequence, record.queueSequence + 1)
    }

    public mutating func markNotQuestion(eventID: String) {
        guard let index = questionRecords.firstIndex(where: { $0.eventID == eventID }) else { return }
        questionRecords.remove(at: index)
        if featuredEvent?.id == eventID { featuredEvent = nil }
    }

    public mutating func setHighPriority(_ isHigh: Bool, eventID: String) {
        guard let index = questionRecords.firstIndex(where: { $0.eventID == eventID }) else { return }
        questionRecords[index].priority = isHigh ? .high : .normal
    }

    public mutating func startAnswering(eventID: String) {
        guard let index = questionRecords.firstIndex(where: { $0.eventID == eventID }) else { return }
        moveCurrentAnswerToQueue(except: eventID)
        questionRecords[index].status = .answering
        featuredEvent = questionRecords[index].event
    }

    public mutating func markAnswered(eventID: String) {
        updateQuestion(eventID: eventID, to: .answered)
    }

    @discardableResult
    public mutating func answerAndAdvance(eventID: String) -> DanmuEvent? {
        updateQuestion(eventID: eventID, to: .answered)
        guard let next = orderedQuestionRecords.first(where: { $0.status == .pending }) else {
            return nil
        }
        startAnswering(eventID: next.eventID)
        return next.event
    }

    public mutating func postpone(eventID: String) {
        guard let index = questionRecords.firstIndex(where: { $0.eventID == eventID }) else { return }
        questionRecords[index].status = .pending
        questionRecords[index].queueSequence = takeNextSequence()
        if featuredEvent?.id == eventID { featuredEvent = nil }
    }

    public mutating func skipQuestion(eventID: String) {
        updateQuestion(eventID: eventID, to: .skipped)
    }

    public mutating func resetQuestion(eventID: String) {
        restoreQuestion(eventID: eventID)
    }

    public mutating func restoreQuestion(eventID: String) {
        guard let index = questionRecords.firstIndex(where: { $0.eventID == eventID }) else { return }
        questionRecords[index].status = .pending
        questionRecords[index].queueSequence = takeNextSequence()
    }

    public mutating func feature(eventID: String?) {
        guard let eventID else {
            featuredEvent = nil
            return
        }
        guard let event = event(for: eventID) else { return }
        featuredEvent = event
    }

    public mutating func feature(_ event: DanmuEvent) {
        featuredEvent = event
    }

    private var orderedQuestionRecords: [QuestionRecord] {
        questionRecords.sorted { lhs, rhs in
            if lhs.priority != rhs.priority { return lhs.priority == .high }
            return lhs.queueSequence < rhs.queueSequence
        }
    }

    private mutating func addQuestion(event: DanmuEvent, recognition: QuestionRecognition) {
        guard question(for: event.id) == nil else { return }
        questionRecords.append(QuestionRecord(
            event: event,
            recognition: recognition,
            priority: defaultPriority(for: event),
            queueSequence: takeNextSequence()
        ))
    }

    private func defaultPriority(for event: DanmuEvent) -> QuestionPriority {
        event.kind == .superchat ? .high : .normal
    }

    private func event(for eventID: String) -> DanmuEvent? {
        recentEvents.first(where: { $0.id == eventID })
            ?? questionRecords.first(where: { $0.eventID == eventID })?.event
            ?? (featuredEvent?.id == eventID ? featuredEvent : nil)
    }

    private mutating func moveCurrentAnswerToQueue(except eventID: String) {
        guard let index = questionRecords.firstIndex(where: {
            $0.status == .answering && $0.eventID != eventID
        }) else { return }
        questionRecords[index].status = .pending
        questionRecords[index].queueSequence = takeNextSequence()
    }

    private mutating func updateQuestion(eventID: String, to status: QuestionStatus) {
        guard let index = questionRecords.firstIndex(where: { $0.eventID == eventID }) else { return }
        questionRecords[index].status = status
        if status != .answering, featuredEvent?.id == eventID { featuredEvent = nil }
    }

    private mutating func takeNextSequence() -> Int {
        defer { nextQueueSequence += 1 }
        return nextQueueSequence
    }
}
