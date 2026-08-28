import Testing
@testable import BilibiliDanmu

@Suite("Bilibili danmu message segmenter")
struct BilibiliDanmuMessageSegmenterTests {
    @Test func trimsAndSplitsLongChineseDraftByCharacters() {
        let message = "  一二三四五六七八九十一二三四五六七八九十一二三  "

        let segments = BilibiliDanmuMessageSegmenter.segments(for: message, limit: 10)

        #expect(segments == ["一二三四五六七八九十", "一二三四五六七八九十", "一二三"])
    }

    @Test func preservesExtendedGraphemeClusters() {
        let segments = BilibiliDanmuMessageSegmenter.segments(
            for: "👨‍👩‍👧‍👦数据直播间",
            limit: 2
        )

        #expect(segments == ["👨‍👩‍👧‍👦数", "据直", "播间"])
    }

    @Test func emptyDraftAndInvalidLimitProduceNoSegments() {
        #expect(BilibiliDanmuMessageSegmenter.segments(for: " \n ").isEmpty)
        #expect(BilibiliDanmuMessageSegmenter.segments(for: "内容", limit: 0).isEmpty)
    }
}
