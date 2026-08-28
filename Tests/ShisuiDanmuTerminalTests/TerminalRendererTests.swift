import BilibiliDanmu
import DanmuCore
import Foundation
import Testing
@testable import ShisuiDanmuTerminal

@Suite("Terminal renderer")
struct TerminalRendererTests {
    @Test(arguments: TerminalTheme.allCases)
    func everyThemeRendersWithinSmallAndLargeTerminals(theme: TerminalTheme) throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        configuration.theme = theme
        let event = DanmuEvent(
            id: "1",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 0),
            username: "中文用户",
            content: "这个问题应该如何处理？"
        )
        let room = BilibiliRoomSnapshot(
            roomID: "1",
            broadcasterID: "2",
            title: "数据直播间",
            parentAreaName: "知识",
            areaName: "科技",
            onlineCount: 1_234,
            followerCount: 5_678,
            liveStartedAt: Date(timeIntervalSince1970: 0),
            onlineRankUsers: [BilibiliOnlineRankUser(name: "榜一", score: 100, rank: 1)]
        )
        var session = DanmuSession(roomID: "1")
        _ = session.ingest(event)
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: room,
            events: session.recentEvents,
            questions: session.questionRecords,
            featuredEvent: session.featuredEvent,
            broadcasterNickname: "主播本人",
            broadcasterAuthorID: "anchor-id",
            editor: TerminalEditorSnapshot(text: "准备发送", cursor: 4),
            notice: nil,
            totalEventCount: 1,
            revision: 1
        )

        let renderer = TerminalRenderer()
        let compact = renderer.render(state, size: TerminalSize(columns: 60, rows: 18))
        let large = renderer.render(state, size: TerminalSize(columns: 140, rows: 40))

        #expect(compact.contains("拾穗弹幕台"))
        #expect(compact.contains("信息流"))
        #expect(compact.contains("准备发送"))
        #expect(
            large.contains("数据直播间")
                || [.pure, .shisui, .paper, .porcelain, .sage, .blush].contains(theme)
        )
    }

    @Test func slashSuggestionsRenderWithDescriptionsWithoutHidingInput() throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [],
            questions: [],
            featuredEvent: nil,
            slashSuggestions: [
                TerminalSlashSuggestion(
                    completion: "/obs status",
                    title: "/obs status",
                    description: "刷新 OBS 状态"
                ),
                TerminalSlashSuggestion(
                    completion: "/obs start",
                    title: "/obs start",
                    description: "开始 OBS 推流"
                )
            ],
            selectedSlashSuggestion: 1,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "/obs st", cursor: 7),
            notice: nil,
            totalEventCount: 0,
            revision: 1
        )

        let output = TerminalRenderer().render(state, size: TerminalSize(columns: 90, rows: 18))
        #expect(output.contains("Slash Commands"))
        #expect(output.contains("开始 OBS 推流"))
        #expect(output.contains("/obs st"))
        #expect(output.contains("Tab 补全"))
        #expect(output.contains("未登录 B 站 · /login"))
    }

    @Test func broadcasterNicknameGetsDistinctTerminalIdentityColor() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "pure"])
        configuration.chatLayout = true
        let event = DanmuEvent(
            id: "broadcaster",
            kind: .danmu,
            timestamp: .now,
            username: "主播本人",
            content: "这是主播发送的弹幕"
        )
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [event],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: "主播本人",
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: 1,
            revision: 1
        )

        let output = TerminalRenderer().render(state, size: TerminalSize(columns: 80, rows: 16))

        let broadcasterLine = terminalContentLines(output).first { $0.contains("这是主播发送的弹幕") }

        #expect(!output.contains("主播·"))
        #expect(output.contains("主播本人"))
        #expect(output.contains("38;2;121;226;182m主播本人"))
        #expect(output.contains("38;2;231;229;242m这是主播发送的弹幕"))
        #expect(output.contains("48;2;23;21;34m"))
        #expect(broadcasterLine?.hasPrefix("│ ") == true)
        #expect(broadcasterLine?.hasSuffix("这是主播发送的弹幕│") == true)
    }

    @Test func standardMessageFlowKeepsBroadcasterMessagesLeftAligned() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        configuration.chatLayout = false
        let event = DanmuEvent(
            id: "broadcaster-standard",
            kind: .danmu,
            timestamp: .now,
            username: "主播本人",
            content: "普通消息流"
        )
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [event],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: "主播本人",
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: 1,
            revision: 1
        )

        let output = TerminalRenderer().render(state, size: TerminalSize(columns: 80, rows: 16))
        let line = terminalContentLines(output).first { $0.contains("普通消息流") }

        #expect(line?.hasPrefix("│") == true)
        #expect(line?.hasPrefix("│ ") == false)
    }

    @Test func chatLayoutSeparatesMetadataContentAndMessages() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        configuration.chatLayout = true
        let events = [
            DanmuEvent(id: "new", kind: .danmu, timestamp: .now, username: "新观众", content: "第二条消息"),
            DanmuEvent(id: "old", kind: .danmu, timestamp: .now, username: "老观众", content: "第一条消息"),
        ]
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: events,
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: 2,
            revision: 1
        )

        let lines = terminalContentLines(
            TerminalRenderer().render(state, size: TerminalSize(columns: 80, rows: 16))
        )
        let firstContentIndex = try #require(lines.firstIndex { $0.contains("第一条消息") })
        let secondMetadataIndex = try #require(lines.firstIndex { $0.contains("新观众") })

        #expect(lines[firstContentIndex - 1].contains("老观众"))
        #expect(lines[firstContentIndex + 1] == "│                                                                              │")
        #expect(secondMetadataIndex == firstContentIndex + 2)
    }
    @Test func longMessageWrapsWithoutLosingContentInSingleLineLayout() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "pure"])
        configuration.chatLayout = false
        configuration.singleLine = true
        configuration.showTime = false
        let event = DanmuEvent(
            id: "long-message",
            kind: .danmu,
            timestamp: .now,
            username: "长消息观众",
            content: "这是第一段需要自动换行的消息这是第二段必须完整显示的结尾"
        )
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [event],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: 1,
            revision: 1
        )

        let lines = terminalContentLines(
            TerminalRenderer().render(state, size: TerminalSize(columns: 40, rows: 16))
        )
        let firstLineIndex = try #require(lines.firstIndex { $0.contains("长消息观众  这是第一段") })
        let continuationIndex = try #require(lines.firstIndex { $0.contains("必须完整显示的结尾") })

        #expect(continuationIndex > firstLineIndex)
        #expect(terminalDisplayWidth(lines[continuationIndex]) == 40)
    }
    @Test func fullwidthPunctuationWrapsBeforeTerminalBoundary() throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let event = DanmuEvent(
            id: "fullwidth-punctuation",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 0),
            username: "谈谈小恐龙",
            content: "让它做个放烟花的效果，然后再做个流星雨的效果[dog]这个够奔放了"
        )
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [event],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: 1,
            revision: 1
        )

        let lines = terminalContentLines(
            TerminalRenderer().render(state, size: TerminalSize(columns: 41, rows: 16))
        )
        let firstLineIndex = try #require(lines.firstIndex { $0.contains("让它做个放烟花的效果") })

        #expect(!lines[firstLineIndex].contains("，"))
        #expect(lines[firstLineIndex + 1].contains("，然后"))
        #expect(lines.allSatisfy { terminalDisplayWidth($0) <= 41 })
    }

    @Test func replyDanmuKeepsMentionTargetVisible() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "pure"])
        configuration.chatLayout = false
        let event = DanmuEvent(
            id: "reply-danmu",
            kind: .danmu,
            timestamp: .now,
            username: "观众乙",
            content: "回复 @观众甲: 这个方案更适合批处理"
        )
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [event],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: 1,
            revision: 1
        )

        let output = TerminalRenderer().render(state, size: TerminalSize(columns: 80, rows: 16))

        #expect(output.contains("回复 @观众甲: 这个方案更适合批处理"))
    }

    @Test func hiddenUsernameModeAlsoRemovesNamesEmbeddedInEventSummaries() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "pure"])
        configuration.showUsername = false
        let event = DanmuEvent(
            id: "gift",
            kind: .gift,
            timestamp: .now,
            username: "不应显示的用户",
            content: "不应显示的用户 送出 人气票 x1"
        )
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [event],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: 1,
            revision: 1
        )

        let output = TerminalRenderer().render(state, size: TerminalSize(columns: 80, rows: 16))

        #expect(!output.contains("不应显示的用户"))
        #expect(output.contains("送出 人气票 x1"))
    }

    @Test func inputUsesInsertionBarInsteadOfCoveringBlockCursor() throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "输入文字", cursor: 2),
            notice: nil,
            totalEventCount: 0,
            revision: 1
        )

        let output = TerminalRenderer().render(state, size: TerminalSize(columns: 80, rows: 16))

        #expect(!output.contains("/obs 控制 OBS"))
        #expect(output.contains("\u{001B}[6 q\u{001B}[?7h\u{001B}[?25h"))
        // Cursor placement starts at the editor and replays its prefix, so the
        // terminal—not our Unicode width table—advances over CJK/IME text.
        #expect(output.contains("\u{001B}[15;5H"))
        #expect(output.contains("输入"))
    }

    @Test(arguments: TerminalTheme.allCases)
    func adaptiveLayoutNeverExceedsTerminalBounds(theme: TerminalTheme) throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        configuration.theme = theme
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: [],
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "一段很长的中文输入内容", cursor: 10),
            notice: nil,
            totalEventCount: 0,
            revision: 1
        )
        let renderer = TerminalRenderer()
        let sizes = [
            TerminalSize(columns: 12, rows: 3),
            TerminalSize(columns: 20, rows: 6),
            TerminalSize(columns: 31, rows: 7),
            TerminalSize(columns: 32, rows: 7),
            TerminalSize(columns: 45, rows: 9),
            TerminalSize(columns: 71, rows: 10),
            TerminalSize(columns: 72, rows: 14),
            TerminalSize(columns: 86, rows: 18),
            TerminalSize(columns: 120, rows: 32),
        ]

        for size in sizes {
            let output = renderer.render(state, size: size)
            let lines = terminalContentLines(output)
            #expect(lines.count == size.rows, "\(theme) at \(size.columns)x\(size.rows)")
            #expect(
                lines.allSatisfy { terminalDisplayWidth($0) <= size.columns },
                "\(theme) exceeded width at \(size.columns)x\(size.rows)"
            )
            #expect(output.contains("\u{001B}[?7l"))
            #expect(output.contains("\u{001B}[?7h"))
        }
    }
    @Test func selectionPresentsOnlyChangedTerminalRows() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "pure"])
        configuration.chatLayout = false
        configuration.singleLine = true
        configuration.showTime = false
        let events = (0..<40).map { index in
            DanmuEvent(id: "event-\(index)", kind: .danmu, timestamp: .now, username: "观众\(index)", content: "消息\(index)")
        }
        let state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: events,
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: events.count,
            revision: 1
        )
        var selectedState = state
        selectedState.selectedEventID = "event-20"
        let renderer = TerminalRenderer()
        let size = TerminalSize(columns: 120, rows: 40)
        let initialFrame = renderer.renderInteractive(state, size: size)
        let selectedFrame = renderer.renderInteractive(selectedState, size: size)
        var presenter = TerminalFramePresenter()

        _ = presenter.present(initialFrame)
        let update = presenter.present(selectedFrame)

        #expect(!update.contains("\u{001B}[2J"))
        #expect(update.contains("\u{001B}[7m"))
        #expect(update.utf8.count < selectedFrame.output.utf8.count)
    }

    @Test func selectedWrappedMessageStaysVisibleAndHighlighted() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "pure"])
        configuration.chatLayout = false
        configuration.singleLine = true
        configuration.showTime = false
        let events = (0..<20).map { index in
            DanmuEvent(
                id: "wrapped-\(index)",
                kind: .danmu,
                timestamp: .now,
                username: "观众\(index)",
                content: "前缀\(index)-" + String(repeating: "长", count: 20) + "-结尾\(index)"
            )
        }
        var state = TerminalViewState(
            configuration: configuration,
            connectionState: .connected(roomID: "1"),
            room: nil,
            events: events,
            questions: [],
            featuredEvent: nil,
            broadcasterNickname: nil,
            broadcasterAuthorID: nil,
            editor: TerminalEditorSnapshot(text: "", cursor: 0),
            notice: nil,
            totalEventCount: events.count,
            revision: 1
        )
        state.selectedEventID = "wrapped-19"
        let output = TerminalRenderer().render(state, size: TerminalSize(columns: 40, rows: 16))

        #expect(output.contains("前缀19"))
        #expect(output.contains("\u{001B}[7m"))
    }
}

private func terminalContentLines(_ output: String) -> [String] {
    output
        // The renderer replays the editor prefix after positioning the hardware
        // cursor. It updates existing cells and is not an additional content line.
        .replacingOccurrences(
            of: "\u{001B}\\[[0-9]+;[0-9]+H.*?\u{001B}\\[0m(?=\u{001B}\\[6 q)",
            with: "",
            options: .regularExpression
        )
        .replacingOccurrences(
            of: "\u{001B}\\][^\u{0007}]*\u{0007}",
            with: "",
            options: .regularExpression
        )
        .replacingOccurrences(
            of: "\u{001B}\\[[0-?]*[ -/]*[@-~]",
            with: "",
            options: .regularExpression
        )
        .components(separatedBy: "\r\n")
}

private func terminalDisplayWidth(_ value: String) -> Int {
    value.unicodeScalars.reduce(0) { width, scalar in
        let code = scalar.value
        if code == 0 || code < 32 || (0x7F...0x9F).contains(code) { return width }
        if (0x0300...0x036F).contains(code) || (0xFE00...0xFE0F).contains(code) || code == 0x200D {
            return width
        }
        let isWide = (0x1100...0x115F).contains(code)
            || (0x2E80...0xA4CF).contains(code)
            || (0xAC00...0xD7A3).contains(code)
            || (0xF900...0xFAFF).contains(code)
            || (0xFE10...0xFE19).contains(code)
            || (0xFE30...0xFE6F).contains(code)
            || (0xFF01...0xFF60).contains(code)
            || (0xFFE0...0xFFE6).contains(code)
            || (0x1F300...0x1FAFF).contains(code)
            || (0x20000...0x3FFFD).contains(code)
        return width + (isWide ? 2 : 1)
    }
}
