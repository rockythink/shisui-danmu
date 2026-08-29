import BilibiliDanmu
import DanmuCore
import Foundation
import OBSControl

struct TerminalSize: Equatable, Sendable {
    let columns: Int
    let rows: Int
}
struct TerminalScreenFrame: Equatable, Sendable {
    let lines: [String]
    let cursorRow: Int
    let cursorStartColumn: Int
    let cursorPrefix: String
    let title: String
    let background: String
}

struct TerminalRenderResult: Equatable, Sendable {
    let output: String
    let screen: TerminalScreenFrame
}

struct TerminalFramePresenter {
    private var previousScreen: TerminalScreenFrame?

    mutating func reset() {
        previousScreen = nil
    }

    mutating func present(_ result: TerminalRenderResult) -> String {
        defer { previousScreen = result.screen }
        guard let previousScreen,
              previousScreen.lines.count == result.screen.lines.count else {
            return result.output
        }

        let current = result.screen
        var output = "\u{001B}[?2026h\u{001B}[?7l\(current.title)\(current.background)"
        for index in current.lines.indices where current.lines[index] != previousScreen.lines[index] {
            output += "\u{001B}[\(index + 1);1H\(current.lines[index])"
        }
        output += "\u{001B}[\(current.cursorRow);\(max(1, current.cursorStartColumn))H"
            + current.cursorPrefix
            + "\u{001B}[0m\u{001B}[6 q\u{001B}[?7h\u{001B}[?25h\u{001B}[?2026l"
        return output
    }
}
struct TerminalRenderer {
    private let styleReset = "\u{001B}[22;39m"
    private let fullReset = "\u{001B}[0m"

    func render(_ state: TerminalViewState, size: TerminalSize, now: Date = .now) -> String {
        renderInteractive(state, size: size, now: now).output
    }

    func renderInteractive(_ state: TerminalViewState, size: TerminalSize, now: Date = .now) -> TerminalRenderResult {
        let width = max(1, size.columns)
        let height = max(1, size.rows)
        // Framed panels cost four rows before showing any messages. Narrow and
        // short terminals stay linear so resizing down preserves information.
        if width < 72 || height < 10 {
            return compactRender(state, width: width, height: height, now: now)
        }

        let palette = state.configuration.palette
        let status = statusLine(state, width: width, now: now)
        let footerHeight = 3
        let bodyHeight = max(2, height - footerHeight - 1)
        var body: [String]

        switch state.configuration.theme {
        case .chatroom where width >= 72 && bodyHeight >= 10:
            body = chatroom(state, width: width, height: bodyHeight, now: now)
        case .info where width >= 72 && bodyHeight >= 10:
            body = infoTheme(state, width: width, height: bodyHeight, now: now)
        case .simple where bodyHeight >= 5:
            let info = roomSummary(state.room, now: now)
            body = [color(info.fitted(to: width), palette.info)] + box(
                title: "Messages",
                width: width,
                height: bodyHeight - 1,
                body: eventRows(state.events, state: state, width: width - 2),
                frame: palette.frame
            )
        case .shisui, .paper, .porcelain, .sage, .blush:
            body = shisui(state, width: width, height: bodyHeight, now: now)
        case .chatroom, .pure, .simple, .info:
            body = box(
                title: "Messages",
                width: width,
                height: bodyHeight,
                body: eventRows(state.events, state: state, width: width - 2),
                frame: palette.frame
            )
        }
        if !state.slashSuggestions.isEmpty {
            let paletteRows = slashPalette(state, width: width, maxHeight: bodyHeight)
            body = Array(body.prefix(max(0, bodyHeight - paletteRows.count))) + paletteRows
        }
        let normalizedBody = Array(body.prefix(bodyHeight))
            + Array(repeating: String(repeating: " ", count: width), count: max(0, bodyHeight - body.count))
        let prompt = " ❯ "
        let availableEditorWidth = max(1, width - 2 - prompt.displayWidth)
        let viewport = editorViewport(visibleEditor(in: state), width: availableEditorWidth)
        let draft = prompt + viewport.text
        let inputTitle: String
        if state.obsPasswordEntryActive {
            inputTitle = "OBS WebSocket 新密码（安全输入，Esc 取消）"
        } else if state.obsStopConfirmationPending {
            inputTitle = "/obs confirm 或 /obs cancel"
        } else if state.broadcasterNickname == nil {
            inputTitle = "未登录 B 站 · /login"
        } else {
            inputTitle = ""
        }
        let input = box(
            title: inputTitle,
            width: width,
            height: footerHeight,
            body: [StyledLine(color(draft, palette.content), columns: min(width - 2, draft.displayWidth))],
            frame: palette.inputFrame
        )
        let background = backgroundSequence(palette.background)
        let title = "\u{001B}]2;拾穗终端舞台 · \(state.configuration.roomID)\u{0007}"
        let lines = Array(([status] + normalizedBody + input).prefix(height))
        let cursorRow = max(1, height - 1)
        return renderFrame(
            lines: lines,
            cursorRow: cursorRow,
            cursorStartColumn: min(width - 1, 2 + prompt.displayWidth),
            cursorPrefix: color(viewport.beforeCursor, palette.content),
            title: title,
            background: background
        )
    }

    private func compactRender(
        _ state: TerminalViewState,
        width: Int,
        height: Int,
        now: Date
    ) -> TerminalRenderResult {
        let prompt = "❯ "
        let editorWidth = max(1, width - prompt.displayWidth)
        let viewport = editorViewport(visibleEditor(in: state), width: editorWidth)
        let input = color((prompt + viewport.text).fitted(to: width), state.configuration.palette.content)
        var lines: [String] = []

        if height >= 2 {
            lines.append(statusLine(state, width: width, now: now))
        }
        let eventCapacity = max(0, height - lines.count - 1)
        if !state.slashSuggestions.isEmpty {
            let suggestions = visibleSlashSuggestions(state, limit: eventCapacity)
            lines += suggestions.map { suggestion in
                let marker = suggestion.index == state.selectedSlashSuggestion ? "› " : "  "
                return (marker + suggestion.value.title).fitted(to: width)
            }
            while lines.count < height - 1 {
                lines.append(String(repeating: " ", count: width))
            }
            lines.append(input)
            return renderFrame(
                lines: Array(lines.suffix(height)),
                cursorRow: height,
                cursorStartColumn: min(width, 1 + prompt.displayWidth),
                cursorPrefix: color(viewport.beforeCursor, state.configuration.palette.content),
                title: "\u{001B}]2;拾穗终端舞台 · Slash Commands\u{0007}",
                background: backgroundSequence(state.configuration.palette.background)
            )
        }
        let eventLines = eventRows(state.events, state: state, width: width)
        let visibleRows: ArraySlice<StyledLine>
        if let selectedIndex = eventLines.lastIndex(where: \.isSelected) {
            let end = min(eventLines.count, max(eventCapacity, selectedIndex + 1))
            visibleRows = eventLines[max(0, end - eventCapacity)..<end]
        } else {
            visibleRows = eventLines.suffix(eventCapacity)
        }
        lines += visibleRows.map { line in
            let padding = String(repeating: " ", count: max(0, width - line.columns))
            return line.isSelected
                ? "\u{001B}[7m" + line.rendered + padding + "\u{001B}[27m"
                : line.rendered + padding
        }
        while lines.count < height - 1 {
            lines.append(String(repeating: " ", count: width))
        }
        lines.append(input)
        lines = Array(lines.suffix(height))

        let cursorRow = height
        return renderFrame(
            lines: lines,
            cursorRow: cursorRow,
            cursorStartColumn: min(width, 1 + prompt.displayWidth),
            cursorPrefix: color(viewport.beforeCursor, state.configuration.palette.content),
            title: "\u{001B}]2;拾穗终端舞台 · \(state.configuration.roomID)\u{0007}",
            background: backgroundSequence(state.configuration.palette.background)
        )
    }

    private func renderFrame(
        lines: [String],
        cursorRow: Int,
        cursorStartColumn: Int,
        cursorPrefix: String,
        title: String,
        background: String
    ) -> TerminalRenderResult {
        let output = "\u{001B}[?2026h\u{001B}[?7l\u{001B}[H\u{001B}[2J\(title)\(background)"
            + lines.joined(separator: "\r\n")
            + "\u{001B}[\(cursorRow);\(max(1, cursorStartColumn))H"
            + cursorPrefix
            + fullReset
            + "\u{001B}[6 q\u{001B}[?7h\u{001B}[?25h\u{001B}[?2026l"
        let screen = TerminalScreenFrame(
            lines: lines,
            cursorRow: cursorRow,
            cursorStartColumn: cursorStartColumn,
            cursorPrefix: cursorPrefix,
            title: title,
            background: background
        )
        return TerminalRenderResult(output: output, screen: screen)
    }

    private func visibleEditor(in state: TerminalViewState) -> TerminalEditorSnapshot {
        guard state.obsPasswordEntryActive else { return state.editor }
        return TerminalEditorSnapshot(
            text: String(repeating: "•", count: state.editor.text.count),
            cursor: state.editor.cursor
        )
    }

    private func editorViewport(_ editor: TerminalEditorSnapshot, width: Int) -> (text: String, beforeCursor: String) {
        let characters = Array(editor.text)
        let cursor = min(max(0, editor.cursor), characters.count)
        var start = 0
        while start < cursor {
            let beforeCursor = String(characters[start..<cursor])
            if beforeCursor.displayWidth < width { break }
            start += 1
        }

        var visible: [Character] = []
        var used = 0
        for character in characters[start...] {
            let characterWidth = String(character).displayWidth
            guard used + characterWidth <= width else { break }
            visible.append(character)
            used += characterWidth
        }
        let beforeCursor = String(characters[start..<cursor])
        let text = String(visible).fitted(to: width)
        return (text, beforeCursor)
    }

    private func slashPalette(_ state: TerminalViewState, width: Int, maxHeight: Int) -> [String] {
        let height = min(maxHeight, min(7, state.slashSuggestions.count + 2))
        guard height >= 3 else { return [] }
        let visible = visibleSlashSuggestions(state, limit: height - 2)
        let rows = visible.map { suggestion -> StyledLine in
            let selected = suggestion.index == state.selectedSlashSuggestion
            let marker = selected ? "› " : "  "
            let text = marker + suggestion.value.title + " — " + suggestion.value.description
            return StyledLine(
                color(
                    text.fittedWithoutPadding(to: max(1, width - 2)),
                    selected ? state.configuration.palette.info : state.configuration.palette.content,
                    bold: selected
                ),
                columns: min(max(1, width - 2), text.displayWidth)
            )
        }
        return box(
            title: "Slash Commands · ↑↓ 选择 · Tab 补全 · Enter 执行 · Esc 关闭",
            width: width,
            height: height,
            body: rows,
            frame: state.configuration.palette.inputFrame
        )
    }

    private func visibleSlashSuggestions(
        _ state: TerminalViewState,
        limit: Int
    ) -> [(index: Int, value: TerminalSlashSuggestion)] {
        guard limit > 0, !state.slashSuggestions.isEmpty else { return [] }
        let selected = min(max(0, state.selectedSlashSuggestion), state.slashSuggestions.count - 1)
        let start = min(
            max(0, selected - limit + 1),
            max(0, state.slashSuggestions.count - limit)
        )
        return state.slashSuggestions.enumerated().dropFirst(start).prefix(limit).map {
            (index: $0.offset, value: $0.element)
        }
    }

    private func statusLine(_ state: TerminalViewState, width: Int, now: Date) -> String {
        let palette = state.configuration.palette
        let compact = width < 40
        let connection = danmuConnectionSegment(state.connectionState, compact: compact, palette: palette)
        let obsConnection = obsConnectionSegment(state.obsStatus.connection, compact: compact, palette: palette)
        let stream = streamSegment(state.obsStatus.stream, compact: compact, palette: palette)
        let microphone = microphoneSegment(state.obsStatus.microphone, compact: compact, palette: palette)
        let pending = state.questions.filter { $0.status == .pending || $0.status == .answering }.count
        let online = state.room.map { "在线 \($0.onlineCount.formatted())" } ?? "在线 --"
        let layout = state.configuration.chatLayout ? "聊天" : "信息流"
        let scene = state.obsStatus.currentScene ?? "--"
        let segments: [TerminalStatusSegment]

        if width < 40 {
            segments = [
                TerminalStatusSegment(" ", tint: palette.content),
                obsConnection,
                TerminalStatusSegment(" ", tint: palette.content),
                stream,
                TerminalStatusSegment(" ", tint: palette.content),
                microphone,
                TerminalStatusSegment(" ", tint: palette.content),
                connection,
                TerminalStatusSegment("  房间 \(state.configuration.roomID) ", tint: palette.content),
            ]
        } else if width < 56 {
            segments = [
                TerminalStatusSegment(" ", tint: palette.content),
                obsConnection,
                TerminalStatusSegment("  ", tint: palette.content),
                stream,
                TerminalStatusSegment("  ", tint: palette.content),
                microphone,
                TerminalStatusSegment("  ", tint: palette.content),
                connection,
                TerminalStatusSegment("  房间 \(state.configuration.roomID) ", tint: palette.content),
            ]
        } else if width < 72 {
            segments = [
                TerminalStatusSegment(" 拾穗弹幕台  ", tint: palette.content, bold: true),
                TerminalStatusSegment("\(layout)  ", tint: palette.time),
                obsConnection,
                TerminalStatusSegment(" ", tint: palette.content),
                stream,
                TerminalStatusSegment(" ", tint: palette.content),
                microphone,
                TerminalStatusSegment(" ", tint: palette.content),
                connection,
                TerminalStatusSegment(" ", tint: palette.content),
            ]
        } else {
            segments = [
                TerminalStatusSegment(" ", tint: palette.content),
                obsConnection,
                TerminalStatusSegment(" ", tint: palette.content),
                stream,
                TerminalStatusSegment("  场景 \(scene)  ", tint: palette.content),
                microphone,
                TerminalStatusSegment(" │ 拾穗弹幕台  \(layout)  ", tint: palette.content, bold: true),
                connection,
                TerminalStatusSegment(
                    "  房间 \(state.configuration.roomID)  \(online)  互动 \(state.totalEventCount)  待回答 \(pending) ",
                    tint: palette.time
                ),
            ]
        }

        return styledStatusLine(segments, width: width)
    }

    private func styledStatusLine(_ segments: [TerminalStatusSegment], width: Int) -> String {
        var rendered = ""
        var remaining = width
        for segment in segments where remaining > 0 {
            let text = segment.text.fittedWithoutPadding(to: remaining)
            rendered += color(text, segment.tint, bold: segment.bold)
            remaining -= text.displayWidth
            guard text.displayWidth == segment.text.displayWidth else { break }
        }
        return rendered + String(repeating: " ", count: max(0, remaining))
    }

    private func danmuConnectionSegment(
        _ state: BilibiliConnectionState,
        compact: Bool,
        palette: TerminalPalette
    ) -> TerminalStatusSegment {
        switch state {
        case .disconnected:
            TerminalStatusSegment(compact ? "弹○" : "弹幕○", tint: palette.warning, bold: true)
        case .connecting:
            TerminalStatusSegment(compact ? "弹…" : "弹幕连接中", tint: palette.rank, bold: true)
        case .connected:
            TerminalStatusSegment(compact ? "弹●" : "弹幕●", tint: palette.info, bold: true)
        case .reconnecting(let attempt, _):
            TerminalStatusSegment(compact ? "重\(attempt)" : "弹幕重连 \(attempt)", tint: palette.rank, bold: true)
        case .error:
            TerminalStatusSegment(compact ? "弹×" : "弹幕错误", tint: palette.warning, bold: true)
        }
    }

    private func obsConnectionSegment(
        _ state: OBSConnectionState,
        compact: Bool,
        palette: TerminalPalette
    ) -> TerminalStatusSegment {
        switch state {
        case .unknown:
            TerminalStatusSegment(compact ? "OBS?" : "OBS --", tint: palette.time)
        case .connected:
            TerminalStatusSegment("OBS●", tint: palette.info, bold: true)
        case .unavailable:
            TerminalStatusSegment("OBS×", tint: palette.warning, bold: true)
        }
    }

    private func streamSegment(
        _ state: OBSStreamState,
        compact: Bool,
        palette: TerminalPalette
    ) -> TerminalStatusSegment {
        switch state {
        case .unknown:
            TerminalStatusSegment(compact ? "播?" : "直播 --", tint: palette.time)
        case .stopped:
            TerminalStatusSegment(compact ? "播○" : "未直播", tint: palette.time)
        case .starting:
            TerminalStatusSegment(compact ? "播…" : "开播中…", tint: palette.rank, bold: true)
        case .live:
            TerminalStatusSegment(compact ? "播●" : "● 直播中", tint: palette.warning, bold: true)
        case .stopping:
            TerminalStatusSegment(compact ? "停…" : "停播中…", tint: palette.rank, bold: true)
        }
    }

    private func microphoneSegment(
        _ state: OBSMicrophoneState,
        compact: Bool,
        palette: TerminalPalette
    ) -> TerminalStatusSegment {
        switch state {
        case .unknown:
            TerminalStatusSegment(compact ? "麦?" : "MIC --", tint: palette.time)
        case .unmuted:
            TerminalStatusSegment(compact ? "麦●" : "MIC 开启", tint: palette.info, bold: true)
        case .muted:
            TerminalStatusSegment(compact ? "麦×" : "MIC 静音", tint: palette.warning, bold: true)
        }
    }

    private func chatroom(_ state: TerminalViewState, width: Int, height: Int, now: Date) -> [String] {
        let sidebarWidth = min(28, max(20, width / 4))
        let messageWidth = max(20, width - sidebarWidth)
        let infoHeight = min(9, max(6, height / 3))
        let rankHeight = max(3, height - infoHeight)
        let info = box(
            title: "RoomInfo",
            width: sidebarWidth,
            height: infoHeight,
            body: roomInfoRows(state.room, now: now).map { StyledLine.plain($0) },
            frame: state.configuration.palette.frame,
            bodyColor: state.configuration.palette.info
        )
        let rank = box(
            title: "Rank(\(state.room?.onlineRankUsers.count ?? 0))",
            width: sidebarWidth,
            height: rankHeight,
            body: rankRows(state.room, showUsername: state.configuration.showUsername).map { StyledLine.plain($0) },
            frame: state.configuration.palette.frame,
            bodyColor: state.configuration.palette.rank
        )
        let messages = box(
            title: "Messages",
            width: messageWidth,
            height: height,
            body: eventRows(state.events, state: state, width: messageWidth - 2),
            frame: state.configuration.palette.frame
        )
        return zipColumns(info + rank, messages)
    }

    private func infoTheme(_ state: TerminalViewState, width: Int, height: Int, now: Date) -> [String] {
        let sidebarWidth = min(24, max(18, width / 5))
        let mainWidth = width - sidebarWidth
        let sidebar = box(
            title: "RoomInfo",
            width: sidebarWidth,
            height: max(6, height / 2),
            body: roomInfoRows(state.room, now: now).map { StyledLine.plain($0) },
            frame: state.configuration.palette.frame,
            bodyColor: state.configuration.palette.info
        ) + box(
            title: "Rank",
            width: sidebarWidth,
            height: height - max(6, height / 2),
            body: rankRows(state.room, showUsername: state.configuration.showUsername).map { StyledLine.plain($0) },
            frame: state.configuration.palette.frame,
            bodyColor: state.configuration.palette.rank
        )

        let messages = state.events.filter { ![.enter, .follow, .share, .gift, .guardEvent, .superchat].contains($0.kind) }
        let access = state.events.filter { [.enter, .follow, .share].contains($0.kind) }
        let gifts = state.events.filter { [.gift, .guardEvent, .superchat].contains($0.kind) }
        let main: [String]
        if mainWidth >= 78 {
            let first = mainWidth / 2
            let second = (mainWidth - first) / 2
            let third = mainWidth - first - second
            main = zipColumns(
                box(title: "Messages", width: first, height: height, body: eventRows(messages, state: state, width: first - 2), frame: state.configuration.palette.frame),
                box(title: "Access", width: second, height: height, body: eventRows(access, state: state, width: second - 2), frame: state.configuration.palette.frame),
                box(title: "Gift", width: third, height: height, body: eventRows(gifts, state: state, width: third - 2), frame: state.configuration.palette.frame)
            )
        } else {
            let messageWidth = max(20, mainWidth * 2 / 3)
            let sideWidth = mainWidth - messageWidth
            let half = height / 2
            let side = box(title: "Access", width: sideWidth, height: half, body: eventRows(access, state: state, width: sideWidth - 2), frame: state.configuration.palette.frame)
                + box(title: "Gift", width: sideWidth, height: height - half, body: eventRows(gifts, state: state, width: sideWidth - 2), frame: state.configuration.palette.frame)
            main = zipColumns(
                box(title: "Messages", width: messageWidth, height: height, body: eventRows(messages, state: state, width: messageWidth - 2), frame: state.configuration.palette.frame),
                side
            )
        }
        return zipColumns(sidebar, main)
    }

    private func shisui(_ state: TerminalViewState, width: Int, height: Int, now: Date) -> [String] {
        guard width >= 86 else {
            return box(
                title: "Ghost Stage · Ctrl+N/P 选择问题 · Ctrl+A 回答/完成 · Ctrl+F 重点",
                width: width,
                height: height,
                body: eventRows(state.events, state: state, width: width - 2),
                frame: state.configuration.palette.frame
            )
        }
        let queueWidth = min(38, width / 3)
        let messagesWidth = width - queueWidth
        let messages = box(
            title: state.featuredEvent.map {
                state.configuration.showUsername
                    ? "NOW · \($0.username ?? "观众")：\($0.terminalFallbackContent.replacingKnownBilibiliEmotes())"
                    : "NOW · \($0.terminalFallbackContent.replacingKnownBilibiliEmotes())"
            } ?? "Ghost Stage",
            width: messagesWidth,
            height: height,
            body: eventRows(state.events, state: state, width: messagesWidth - 2),
            frame: state.configuration.palette.frame
        )
        let active = state.questions.filter { $0.status == .pending || $0.status == .answering }
        let queueRows = active.map { question -> StyledLine in
            let priority = question.priority == .high ? "▲" : " "
            let answering = question.status == .answering ? "NOW" : ""
            let username = state.configuration.showUsername ? "\(question.event.username ?? "观众") " : ""
            let text = "\(priority) \(username)\(answering) \(question.event.terminalFallbackContent.replacingKnownBilibiliEmotes())"
            let tint = question.status == .answering
                ? state.configuration.palette.info
                : (question.priority == .high ? state.configuration.palette.warning : state.configuration.palette.content)
            return StyledLine(color(text.fitted(to: queueWidth - 2), tint), columns: min(queueWidth - 2, text.displayWidth))
        }
        let queue = box(
            title: "Questions · \(active.count)",
            width: queueWidth,
            height: height,
            body: queueRows,
            frame: state.configuration.palette.frame
        )
        return zipColumns(messages, queue)
    }

    private func eventRows(_ events: [DanmuEvent], state: TerminalViewState, width: Int) -> [StyledLine] {
        var rows: [StyledLine] = []
        for event in events.reversed() {
            let time = state.configuration.showTime ? DateFormatter.terminalTime.string(from: event.timestamp) + " " : ""
            let isBroadcaster = isBroadcasterMessage(event, state: state)
            let username = state.configuration.showUsername ? (event.username ?? kindName(event.kind)) : ""
            let usernameTint = isBroadcaster ? state.configuration.palette.broadcaster : state.configuration.palette.name
            let contentTint = tint(event.kind, palette: state.configuration.palette)
            let eventContent = visibleContent(event, state: state)
            let isSelected = event.id == state.selectedEventID
            if state.configuration.chatLayout {
                if !rows.isEmpty { rows.append(.plain("")) }
                let visibleUsername = username.fittedWithoutPadding(to: max(1, width - time.displayWidth))
                let metadataColumns = min(width, time.displayWidth + visibleUsername.displayWidth)
                let metadata = color(time, state.configuration.palette.time)
                    + color(visibleUsername, usernameTint, bold: isBroadcaster)
                if !time.isEmpty || !visibleUsername.isEmpty {
                    rows.append(alignedLine(
                        metadata,
                        columns: metadataColumns,
                        width: width,
                        trailing: isBroadcaster,
                        isSelected: isSelected
                    ))
                }
                for content in eventContent.wrapped(to: width) {
                    rows.append(alignedLine(
                        color(content, contentTint),
                        columns: content.displayWidth,
                        width: width,
                        trailing: isBroadcaster,
                        isSelected: isSelected
                    ))
                }
            } else if state.configuration.singleLine {
                let separator = username.isEmpty ? "" : "  "
                let availableContent = max(1, width - time.displayWidth - username.displayWidth - separator.displayWidth)
                let contentLines = eventContent.wrapped(
                    firstLineWidth: availableContent,
                    continuationWidth: width
                )
                let firstContent = contentLines.first ?? ""
                let columns = min(width, time.displayWidth + username.displayWidth + separator.displayWidth + firstContent.displayWidth)
                let rendered = color(time, state.configuration.palette.time)
                    + color(username, usernameTint, bold: isBroadcaster || event.kind == .superchat)
                    + separator
                    + color(firstContent, contentTint)
                rows.append(StyledLine(rendered, columns: columns, isSelected: isSelected))
                rows += contentLines.dropFirst().map {
                    StyledLine(color($0, contentTint), columns: $0.displayWidth, isSelected: isSelected)
                }
            } else {
                let visibleTime = time.fittedWithoutPadding(to: width)
                let visibleUsername = username.fittedWithoutPadding(to: max(0, width - visibleTime.displayWidth))
                let header = visibleTime + visibleUsername
                if !header.isEmpty {
                    let rendered = color(visibleTime, usernameTint, bold: true)
                        + color(visibleUsername, usernameTint, bold: true)
                    rows.append(StyledLine(rendered, columns: header.displayWidth, isSelected: isSelected))
                }
                rows += eventContent.wrapped(to: width).map {
                    StyledLine(color($0, contentTint), columns: $0.displayWidth, isSelected: isSelected)
                }
            }
        }
        if let notice = state.notice {
            rows += ("⚠ \(notice)").wrapped(to: width).map { line in
                StyledLine(
                    color(line, state.configuration.palette.warning),
                    columns: line.displayWidth
                )
            }
        }
        return rows
    }

    private func alignedLine(
        _ rendered: String,
        columns: Int,
        width: Int,
        trailing: Bool,
        isSelected: Bool
    ) -> StyledLine {
        guard trailing else { return StyledLine(rendered, columns: columns, isSelected: isSelected) }
        let padding = String(repeating: " ", count: max(0, width - columns))
        return StyledLine(padding + rendered, columns: width, isSelected: isSelected)
    }

    private func isBroadcasterMessage(_ event: DanmuEvent, state: TerminalViewState) -> Bool {
        BroadcasterIdentity(
            nickname: state.broadcasterNickname,
            authorID: state.broadcasterAuthorID
        )?.matches(event) == true
    }

    private func roomInfoRows(_ room: BilibiliRoomSnapshot?, now: Date) -> [String] {
        guard let room else { return ["正在加载房间信息…"] }
        return [
            room.title,
            "ID: \(room.roomID)",
            "分区: \(room.parentAreaName)/\(room.areaName)",
            "👀 \(room.onlineCount.formatted())",
            "♥ \(room.followerCount.formatted())",
            "◷ \(liveDuration(room.liveStartedAt, now: now))",
        ]
    }

    private func rankRows(_ room: BilibiliRoomSnapshot?, showUsername: Bool) -> [String] {
        guard showUsername else { return ["用户名已隐藏"] }
        guard let users = room?.onlineRankUsers, !users.isEmpty else { return ["暂无在线榜"] }
        return users.map { user in
            let prefix: String
            switch user.rank {
            case 1: prefix = "👑"
            case 2: prefix = "🥈"
            case 3: prefix = "🥉"
            default: prefix = String(format: "%2d", user.rank)
            }
            return "\(prefix) \(user.name)"
        }
    }

    private func roomSummary(_ room: BilibiliRoomSnapshot?, now: Date) -> String {
        guard let room else { return "房间信息加载中…" }
        return "[\(room.roomID)] \(room.title) · \(room.parentAreaName)/\(room.areaName) · 👀 \(room.onlineCount.formatted()) · ♥ \(room.followerCount.formatted()) · ◷ \(liveDuration(room.liveStartedAt, now: now))"
    }

    private func liveDuration(_ startedAt: Date?, now: Date) -> String {
        guard let startedAt else { return "未开播" }
        let seconds = max(0, Int(now.timeIntervalSince(startedAt)))
        let days = seconds / 86_400
        let hours = seconds % 86_400 / 3_600
        let minutes = seconds % 3_600 / 60
        if days > 0 { return "\(days)天\(hours)时\(minutes)分" }
        if hours > 0 { return "\(hours)时\(minutes)分" }
        return "\(minutes)分"
    }

    private func visibleContent(_ event: DanmuEvent, state: TerminalViewState) -> String {
        let source = event.terminalFallbackContent
        let content: String
        if !state.configuration.showUsername,
           event.kind != .danmu,
           let username = event.username,
           source.hasPrefix(username) {
            let remainder = source.dropFirst(username.count)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            content = remainder.isEmpty ? kindName(event.kind) : remainder
        } else {
            content = source
        }
        return content.replacingKnownBilibiliEmotes()
    }

    private func kindName(_ kind: DanmuEventKind) -> String {
        switch kind {
        case .danmu: "弹幕"
        case .gift: "礼物"
        case .guardEvent: "舰长"
        case .superchat: "SC"
        case .enter: "进场"
        case .like: "点赞"
        case .follow: "关注"
        case .share: "分享"
        case .pk: "PK"
        case .lottery: "抽奖"
        case .moderation: "房管"
        case .roomStatus: "房间"
        case .system: "系统"
        }
    }

    private func tint(_ kind: DanmuEventKind, palette: TerminalPalette) -> String {
        switch kind {
        case .gift, .guardEvent, .superchat: palette.rank
        case .follow, .share, .like: palette.info
        case .moderation, .roomStatus, .system, .pk, .lottery: palette.warning
        default: palette.content
        }
    }

    private func box(
        title: String,
        width: Int,
        height: Int,
        body: [StyledLine],
        frame: String,
        bodyColor: String? = nil
    ) -> [String] {
        let width = max(4, width)
        let height = max(2, height)
        let inner = width - 2
        let titleText = title.isEmpty ? "" : " \(title) "
        let safeTitle = titleText.fittedWithoutPadding(to: max(0, inner))
        let topFill = max(0, inner - safeTitle.displayWidth)
        var result = [color("╭\(safeTitle)\(String(repeating: "─", count: topFill))╮", frame)]
        let bodyHeight = max(0, height - 2)
        let visible: [StyledLine]
        if let selectedIndex = body.lastIndex(where: \.isSelected) {
            let end = min(body.count, max(bodyHeight, selectedIndex + 1))
            visible = Array(body[max(0, end - bodyHeight)..<end])
        } else {
            visible = Array(body.suffix(bodyHeight))
        }
        let leadingBlankCount = max(0, bodyHeight - visible.count)
        let lines = Array(repeating: StyledLine.plain(""), count: leadingBlankCount) + visible
        for line in lines {
            let isPlain = !line.rendered.contains("\u{001B}")
            let content = isPlain && line.columns > inner ? line.rendered.fitted(to: inner) : line.rendered
            let columns = min(inner, isPlain ? content.displayWidth : line.columns)
            let padding = String(repeating: " ", count: max(0, inner - columns))
            let rendered = bodyColor.map { color(content, $0) } ?? content
            let selectedRendered = line.isSelected ? "\u{001B}[7m\(rendered)\(padding)\u{001B}[27m" : rendered + padding
            result.append(color("│", frame) + selectedRendered + color("│", frame))
        }
        result.append(color("╰\(String(repeating: "─", count: inner))╯", frame))
        return Array(result.prefix(height))
    }

    private func zipColumns(_ columns: [String]...) -> [String] {
        let count = columns.map(\.count).max() ?? 0
        return (0..<count).map { row in
            columns.map { column in column.indices.contains(row) ? column[row] : "" }.joined()
        }
    }

    private func color(_ text: String, _ hex: String, bold: Bool = false) -> String {
        guard let rgb = RGB(hex: hex) else { return text }
        return "\u{001B}[\(bold ? "1;" : "")38;2;\(rgb.r);\(rgb.g);\(rgb.b)m\(text)\(styleReset)"
    }

    private func backgroundSequence(_ value: String) -> String {
        guard value.uppercased() != "NONE", let rgb = RGB(hex: value) else { return "" }
        return "\u{001B}[48;2;\(rgb.r);\(rgb.g);\(rgb.b)m"
    }
}

private struct TerminalStatusSegment {
    let text: String
    let tint: String
    let bold: Bool

    init(_ text: String, tint: String, bold: Bool = false) {
        self.text = text
        self.tint = tint
        self.bold = bold
    }
}

private struct StyledLine {
    let rendered: String
    let columns: Int
    let isSelected: Bool

    init(_ rendered: String, columns: Int, isSelected: Bool = false) {
        self.rendered = rendered
        self.columns = columns
        self.isSelected = isSelected
    }

    static func plain(_ text: String, isSelected: Bool = false) -> StyledLine {
        StyledLine(text, columns: text.displayWidth, isSelected: isSelected)
    }
}
private struct RGB {
    let r: Int
    let g: Int
    let b: Int

    init?(hex: String) {
        let value = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        guard value.count == 6, let number = Int(value, radix: 16) else { return nil }
        r = number >> 16
        g = number >> 8 & 0xFF
        b = number & 0xFF
    }
}

private let bilibiliEmoteEmoji: [String: String] = [
    "[dog]": "🐶",
    "[doge]": "🐶",
    "[笑哭]": "😂",
    "[滑稽]": "😏",
    "[吃瓜]": "🍉",
    "[妙啊]": "👏",
    "[藏狐]": "🦊",
    "[辣眼睛]": "🙈",
    "[呲牙]": "😁",
    "[OK]": "👌",
    "[ok]": "👌",
    "[星星眼]": "🤩",
    "[哦呼]": "😮",
    "[捂脸]": "🤦",
    "[支持]": "👍",
    "[鼓掌]": "👏",
    "[点赞]": "👍",
    "[爱心]": "❤️",
    "[大笑]": "😄",
    "[惊讶]": "😮",
    "[疑惑]": "🤔",
    "[偷笑]": "🤭",
    "[调皮]": "😜",
    "[生气]": "😠",
    "[尴尬]": "😅",
    "[委屈]": "🥺",
    "[哭泣]": "😭",
    "[害羞]": "😊",
    "[亲亲]": "😘",
    "[打call]": "📣",
    "[抱拳]": "🙏",
    "[保佑]": "🙏",
    "[牛哇牛哇]": "🐮",
    "[给心心]": "🫶",
    "[热词系列_知识增加]": "🧠",
    "[热词系列_破防了]": "💔",
    "[热词系列_好耶]": "🙌",
    "[热词系列_三连]": "👍",
    "[热词系列_爷青回]": "🥹",
]
private extension DanmuEvent {
    var terminalFallbackContent: String {
        content.isEmpty ? emotes.map(\.text).joined() : content
    }
}

private extension String {
    var displayWidth: Int {
        reduce(0) { result, character in result + character.terminalColumnWidth }
    }

    func replacingKnownBilibiliEmotes() -> String {
        guard contains("[") else { return self }

        var result = ""
        result.reserveCapacity(utf8.count)
        var cursor = startIndex
        while let opening = self[cursor...].firstIndex(of: "["),
              let closing = self[opening...].firstIndex(of: "]") {
            result += self[cursor..<opening]
            let token = String(self[opening...closing])
            result += bilibiliEmoteEmoji[token] ?? token
            cursor = index(after: closing)
        }
        result += self[cursor...]
        return result
    }

    func wrapped(to width: Int) -> [String] {
        wrapped(firstLineWidth: width, continuationWidth: width)
    }

    func wrapped(firstLineWidth: Int, continuationWidth: Int) -> [String] {
        let initialWidth = max(1, firstLineWidth)
        let followingWidth = max(1, continuationWidth)
        var lineWidth = initialWidth
        var lines: [String] = []
        var line = ""
        var used = 0
        var endedWithLineBreak = false

        for character in self {
            if character == "\n" {
                lines.append(line)
                line = ""
                used = 0
                lineWidth = followingWidth
                endedWithLineBreak = true
                continue
            }

            endedWithLineBreak = false
            let value = String(character)
            let characterWidth = value.displayWidth
            if !line.isEmpty, used + characterWidth > lineWidth {
                lines.append(line)
                line = ""
                used = 0
                lineWidth = followingWidth
            }
            if characterWidth > lineWidth {
                lines.append(value.fittedWithoutPadding(to: lineWidth))
                lineWidth = followingWidth
                continue
            }
            line.append(character)
            used += characterWidth
        }

        if !line.isEmpty || lines.isEmpty || endedWithLineBreak {
            lines.append(line)
        }
        return lines
    }

    func fittedWithoutPadding(to width: Int) -> String {
        guard width > 0 else { return "" }
        guard displayWidth > width else { return self }
        return fitted(to: width).trimmingCharacters(in: .whitespaces)
    }

    func fitted(to width: Int) -> String {
        guard width > 0 else { return "" }
        if displayWidth == width { return self }
        if displayWidth < width { return self + String(repeating: " ", count: width - displayWidth) }
        var result = ""
        var used = 0
        for character in self {
            let value = String(character)
            let characterWidth = value.displayWidth
            guard used + characterWidth <= max(1, width - 1) else { break }
            result.append(character)
            used += characterWidth
        }
        return result + "…" + String(repeating: " ", count: max(0, width - used - 1))
    }
}

private extension Character {
    var terminalColumnWidth: Int {
        let scalars = unicodeScalars
        let containsEmojiPresentation = scalars.contains { $0.properties.isEmojiPresentation }
        let containsEmojiVariation = scalars.contains { $0.value == 0xFE0F }
        let containsEmoji = scalars.contains { $0.properties.isEmoji }
        if containsEmojiPresentation || (containsEmojiVariation && containsEmoji) {
            return 2
        }
        return scalars.reduce(0) { result, scalar in
            result + scalar.terminalColumnWidth
        }
    }
}

private extension Unicode.Scalar {
    var terminalColumnWidth: Int {
        let value = self.value
        if value == 0 || value < 32 || (0x7F...0x9F).contains(value) { return 0 }
        if (0x0300...0x036F).contains(value) || (0xFE00...0xFE0F).contains(value) || value == 0x200D { return 0 }
        if (0x1100...0x115F).contains(value)
            || (0x2E80...0xA4CF).contains(value)
            || (0xAC00...0xD7A3).contains(value)
            || (0xF900...0xFAFF).contains(value)
            || (0xFE10...0xFE19).contains(value)
            || (0xFE30...0xFE6F).contains(value)
            || (0xFF01...0xFF60).contains(value)
            || (0xFFE0...0xFFE6).contains(value)
            || (0x1F300...0x1FAFF).contains(value)
            || (0x20000...0x3FFFD).contains(value) { return 2 }
        return 1
    }
}

private extension DateFormatter {
    static let terminalTime: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "zh_CN")
        formatter.dateFormat = "HH:mm"
        return formatter
    }()
}

private extension BilibiliConnectionState {
    var isConnected: Bool {
        if case .connected = self { return true }
        return false
    }
}
