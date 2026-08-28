import Foundation
import Testing
@testable import DanmuCore

@Suite("Broadcaster identity")
struct BroadcasterIdentityTests {
    @Test func authorIDRecognizesAndCanonicalizesMaskedLiveNickname() throws {
        let identity = try #require(BroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237"))
        let event = DanmuEvent(
            id: "live",
            kind: .danmu,
            timestamp: .now,
            username: "停***",
            authorID: "7604237",
            content: "测试一下"
        )

        #expect(identity.matches(event))
        #expect(identity.canonicalizing(event).username == "停车拾穗")
    }

    @Test func maskedNicknameWithPlaceholderAuthorIDIsNotTrustedByItself() throws {
        let identity = try #require(BroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237"))
        let event = DanmuEvent(
            id: "privacy-masked-live",
            kind: .danmu,
            timestamp: .now,
            username: "停***",
            authorID: "0",
            content: "从其他客户端发送"
        )

        #expect(!identity.matches(event))
        #expect(identity.canonicalizing(event) == event)
    }

    @Test func maskedNicknameCannotOverrideAConflictingUsableAuthorID() throws {
        let identity = try #require(BroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237"))
        let event = DanmuEvent(
            id: "masked-other-user",
            kind: .danmu,
            timestamp: .now,
            username: "停***",
            authorID: "other-user",
            content: "其他用户"
        )

        #expect(!identity.matches(event))
        #expect(identity.canonicalizing(event) == event)
    }

    @Test func completeNicknameIsFallbackWhenEventHasNoAuthorID() throws {
        let identity = try #require(BroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237"))
        let event = DanmuEvent(
            id: "history",
            kind: .danmu,
            timestamp: .now,
            username: "停车拾穗",
            content: "历史弹幕"
        )

        #expect(identity.matches(event))
    }

    @Test func conflictingAuthorIDWinsOverSameNickname() throws {
        let identity = try #require(BroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237"))
        let event = DanmuEvent(
            id: "impostor",
            kind: .danmu,
            timestamp: .now,
            username: "停车拾穗",
            authorID: "other-user",
            content: "同名用户"
        )

        #expect(!identity.matches(event))
        #expect(identity.canonicalizing(event) == event)
    }

    @Test func nonDanmuEventsAreNeverBroadcasterMessages() throws {
        let identity = try #require(BroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237"))
        let gift = DanmuEvent(
            id: "gift",
            kind: .gift,
            timestamp: .now,
            username: "停车拾穗",
            authorID: "7604237",
            content: "赠送礼物"
        )

        #expect(!identity.matches(gift))
    }
}
