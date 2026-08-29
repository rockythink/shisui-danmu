import Foundation
import Testing
@testable import DanmuCore

@Suite("Danmu event coding")
struct DanmuEventCodableTests {
    @Test func legacyEventWithoutEmotesStillDecodes() throws {
        let data = Data(
            #"{"id":"legacy","kind":"danmu","timestamp":0,"content":"旧归档弹幕","origin":"live"}"#.utf8
        )

        let event = try JSONDecoder().decode(DanmuEvent.self, from: data)

        #expect(event.id == "legacy")
        #expect(event.content == "旧归档弹幕")
        #expect(event.emotes.isEmpty)
    }

    @Test func emotesSurviveEventRoundTrip() throws {
        let emote = DanmuEmote(
            text: "[热]",
            imageURL: try #require(URL(string: "https://i0.hdslb.com/bfs/live/heat.png")),
            width: 20,
            height: 20
        )
        let event = DanmuEvent(
            id: "emote",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 1),
            username: "表情观众",
            content: "[热]",
            emotes: [emote]
        )

        let encoded = try JSONEncoder().encode(event)
        let decoded = try JSONDecoder().decode(DanmuEvent.self, from: encoded)

        #expect(decoded == event)
    }
}
