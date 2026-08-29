import Foundation

public enum DanmuEventKind: String, Codable, CaseIterable, Sendable {
    case danmu
    case gift
    case guardEvent = "guard"
    case superchat
    case enter
    case like
    case follow
    case share
    case pk
    case lottery
    case moderation
    case roomStatus
    case system
}

public enum DanmuEventOrigin: String, Codable, Sendable {
    case live
    case history
}

public struct DanmuEmote: Codable, Equatable, Hashable, Sendable {
    public let text: String
    public let imageURL: URL
    public let width: Int?
    public let height: Int?
    public let isAnimated: Bool

    public init(
        text: String,
        imageURL: URL,
        width: Int? = nil,
        height: Int? = nil,
        isAnimated: Bool = false
    ) {
        self.text = text
        self.imageURL = imageURL
        self.width = width
        self.height = height
        self.isAnimated = isAnimated
    }
}

public struct DanmuEvent: Identifiable, Codable, Equatable, Sendable {
    public let id: String
    public let kind: DanmuEventKind
    public let timestamp: Date
    public let username: String?
    public let authorID: String?
    public let content: String
    public let origin: DanmuEventOrigin
    public let platformEventID: String?
    public let emotes: [DanmuEmote]

    public init(
        id: String,
        kind: DanmuEventKind,
        timestamp: Date,
        username: String? = nil,
        authorID: String? = nil,
        content: String,
        origin: DanmuEventOrigin = .live,
        platformEventID: String? = nil,
        emotes: [DanmuEmote] = []
    ) {
        self.id = id
        self.kind = kind
        self.timestamp = timestamp
        self.username = username
        self.authorID = authorID
        self.content = content
        self.origin = origin
        self.platformEventID = platformEventID
        self.emotes = emotes
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case kind
        case timestamp
        case username
        case authorID
        case content
        case origin
        case platformEventID
        case emotes
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        kind = try container.decode(DanmuEventKind.self, forKey: .kind)
        timestamp = try container.decode(Date.self, forKey: .timestamp)
        username = try container.decodeIfPresent(String.self, forKey: .username)
        authorID = try container.decodeIfPresent(String.self, forKey: .authorID)
        content = try container.decode(String.self, forKey: .content)
        origin = try container.decodeIfPresent(DanmuEventOrigin.self, forKey: .origin) ?? .live
        platformEventID = try container.decodeIfPresent(String.self, forKey: .platformEventID)
        emotes = try container.decodeIfPresent([DanmuEmote].self, forKey: .emotes) ?? []
    }
}
public enum QuestionStatus: String, Codable, CaseIterable, Sendable {
    case pending
    case answering
    case answered
    case skipped
}

public typealias QuestionDisposition = QuestionStatus

public enum QuestionPriority: String, Codable, Sendable {
    case normal
    case high
}

public enum QuestionRecognitionSource: String, Codable, Sendable {
    case builtInRule
    case userKeyword
    case manual
}

public struct QuestionRecognition: Codable, Equatable, Sendable {
    public let source: QuestionRecognitionSource
    public let reason: String

    public init(source: QuestionRecognitionSource, reason: String) {
        self.source = source
        self.reason = reason
    }
}

public struct QuestionRecord: Identifiable, Codable, Equatable, Sendable {
    public var id: String { event.id }
    public var eventID: String { event.id }
    public let event: DanmuEvent
    public var recognition: QuestionRecognition
    public var priority: QuestionPriority
    public var queueSequence: Int
    public var status: QuestionStatus

    public init(
        event: DanmuEvent,
        recognition: QuestionRecognition,
        priority: QuestionPriority,
        queueSequence: Int,
        status: QuestionStatus = .pending
    ) {
        self.event = event
        self.recognition = recognition
        self.priority = priority
        self.queueSequence = queueSequence
        self.status = status
    }
}

public struct SessionMetrics: Codable, Equatable, Sendable {
    public private(set) var totalEventCount: Int
    public private(set) var eventCounts: [DanmuEventKind: Int]

    public init(totalEventCount: Int = 0, eventCounts: [DanmuEventKind: Int] = [:]) {
        self.totalEventCount = totalEventCount
        self.eventCounts = eventCounts
    }

    public func count(for kind: DanmuEventKind) -> Int {
        eventCounts[kind, default: 0]
    }

    mutating func record(_ event: DanmuEvent) {
        totalEventCount += 1
        eventCounts[event.kind, default: 0] += 1
    }
}
