import Foundation

public struct QuestionRules: Codable, Equatable, Sendable {
    public var includedKeywords: [String]
    public var ignoredPhrases: [String]

    public init(includedKeywords: [String] = [], ignoredPhrases: [String] = []) {
        self.includedKeywords = includedKeywords
        self.ignoredPhrases = ignoredPhrases
    }
}

public struct QuestionClassifier: Codable, Equatable, Sendable {
    public var rules: QuestionRules

    public init(rules: QuestionRules = QuestionRules()) {
        self.rules = rules
    }

    public func classify(_ event: DanmuEvent) -> QuestionRecognition? {
        evaluate(event).recognition
    }

    public func evaluate(_ event: DanmuEvent) -> QuestionClassification {
        guard event.kind == .danmu || event.kind == .superchat else {
            return QuestionClassification(isQuestion: false, reason: "非提问候选事件")
        }
        let content = Self.normalized(event.content)
        guard !content.isEmpty else {
            return QuestionClassification(isQuestion: false, reason: "内容为空")
        }

        if let phrase = rules.ignoredPhrases.first(where: { keyword in
            let normalizedKeyword = Self.normalized(keyword)
            return !normalizedKeyword.isEmpty && content.contains(normalizedKeyword)
        }) {
            return QuestionClassification(isQuestion: false, reason: "命中忽略短语：\(phrase)")
        }

        if let keyword = rules.includedKeywords.first(where: { keyword in
            let normalizedKeyword = Self.normalized(keyword)
            return !normalizedKeyword.isEmpty && content.contains(normalizedKeyword)
        }) {
            return QuestionClassification(recognition: QuestionRecognition(
                source: .userKeyword,
                reason: "包含自定义词：\(keyword)"
            ))
        }

        if content.contains("?") || content.contains("？") {
            return QuestionClassification(recognition: QuestionRecognition(
                source: .builtInRule,
                reason: "包含问号"
            ))
        }

        let interrogatives = [
            "为什么", "为何", "怎么", "如何", "请问", "能不能", "可不可以",
            "有没有", "是否", "哪里", "哪个", "多少", "什么",
        ]
        if let marker = interrogatives.first(where: { content.contains($0) }) {
            return QuestionClassification(recognition: QuestionRecognition(
                source: .builtInRule,
                reason: "包含疑问句式：\(marker)"
            ))
        }

        if let suffix = ["吗", "么", "呢"].first(where: { content.hasSuffix($0) }) {
            return QuestionClassification(recognition: QuestionRecognition(
                source: .builtInRule,
                reason: "疑问语气结尾：\(suffix)"
            ))
        }
        return QuestionClassification(isQuestion: false, reason: "未命中提问规则")
    }

    public static func normalized(_ value: String) -> String {
        value
            .precomposedStringWithCompatibilityMapping
            .folding(options: [.caseInsensitive, .widthInsensitive], locale: Locale(identifier: "zh_CN"))
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

public struct QuestionClassification: Codable, Equatable, Sendable {
    public let isQuestion: Bool
    public let recognition: QuestionRecognition?
    public let reason: String

    public init(isQuestion: Bool, recognition: QuestionRecognition? = nil, reason: String) {
        self.isQuestion = isQuestion
        self.recognition = recognition
        self.reason = reason
    }

    public init(recognition: QuestionRecognition) {
        self.isQuestion = true
        self.recognition = recognition
        self.reason = recognition.reason
    }
}
