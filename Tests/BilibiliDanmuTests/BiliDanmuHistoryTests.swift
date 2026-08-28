import DanmuCore
import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili danmu history")
struct BilibiliDanmuHistoryTests {
    @Test func recentHistoryBecomesStableStandardEvents() throws {
        let payload = #"""
        {
          "code": 0,
          "data": {
            "room": [
              {
                "text": "大家晚上好",
                "uid": 1,
                "nickname": "观众甲",
                "timeline": "2026-07-20 21:00:00",
                "id_str": "history-1"
              },
              {
                "text": "这个功能怎么使用？",
                "uid": 2,
                "nickname": "观众乙",
                "timeline": "2026-07-20 21:00:01",
                "id_str": "history-2"
              }
            ]
          }
        }
        """#.data(using: .utf8)!

        let events = try BiliDanmuHistoryParser.parse(data: payload)

        #expect(events.map(\.id) == ["bili-history-1", "bili-history-2"])
        #expect(events.map(\.kind) == [.danmu, .danmu])
        #expect(events.allSatisfy { $0.origin == .history })
        #expect(events.map(\.username) == ["观众甲", "观众乙"])
        #expect(events.map(\.authorID) == ["1", "2"])
        #expect(events.map(\.content) == ["大家晚上好", "这个功能怎么使用？"])
        #expect(events[0].timestamp < events[1].timestamp)
    }
}
