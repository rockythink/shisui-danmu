import Foundation
import Testing
@testable import DanmuCore

@Suite("Explainable question recognition")
struct QuestionClassifierTests {
    @Test func recognizesDanmuAndSuperchatWithoutChangingPlatformKind() {
        let classifier = QuestionClassifier()
        let danmu = event(id: "d", kind: .danmu, content: "这个功能怎么使用")
        let superchat = event(id: "sc", kind: .superchat, content: "是否支持历史搜索")

        #expect(classifier.classify(danmu)?.source == .builtInRule)
        #expect(classifier.classify(superchat)?.source == .builtInRule)
        #expect(danmu.kind == .danmu)
        #expect(superchat.kind == .superchat)
    }

    @Test func rulePrecedenceIsIgnoreThenUserKeywordThenBuiltIn() {
        let classifier = QuestionClassifier(rules: QuestionRules(
            includedKeywords: ["求教程"],
            ignoredPhrases: ["反问示例"]
        ))

        #expect(classifier.classify(event(id: "ignored", content: "反问示例：为什么要这样？")) == nil)
        #expect(classifier.evaluate(event(id: "ignored", content: "反问示例：为什么要这样？")).reason == "命中忽略短语：反问示例")
        #expect(classifier.classify(event(id: "custom", content: "求教程"))?.source == .userKeyword)
        #expect(classifier.classify(event(id: "normal", content: "今天讲缓存吗"))?.source == .builtInRule)
        #expect(classifier.classify(event(id: "false-positive", content: "这个方案可以落地")) == nil)
    }

    @Test func manualCorrectionOverridesAutomaticRecognition() {
        var session = DanmuSession(roomID: "1")
        let automatic = event(id: "auto", content: "为什么这样？")
        let manual = event(id: "manual", content: "求展开")
        session.ingest(automatic)
        session.ingest(manual)

        session.markNotQuestion(eventID: automatic.id)
        session.markAsQuestion(eventID: manual.id)

        #expect(session.question(for: automatic.id) == nil)
        #expect(session.question(for: manual.id)?.recognition.source == .manual)
    }

    private func event(
        id: String,
        kind: DanmuEventKind = .danmu,
        content: String
    ) -> DanmuEvent {
        DanmuEvent(
            id: id,
            kind: kind,
            timestamp: Date(timeIntervalSince1970: 100),
            username: "观众",
            content: content
        )
    }
}
