import BilibiliDanmu
import DanmuCore
import Foundation
import OBSControl

private struct TerminalCanonicalIdentity: Hashable, Sendable {
    let username: String
    let authorID: String
}

enum TerminalOBSIntent: Equatable, Sendable {
    case connect
    case refresh
    case configurePassword(String)
    case configureMicrophone(String)
    case setMuted(Bool, input: String)
    case switchScene(String)
    case startStreaming
    case stopStreaming
}

enum TerminalAccountIntent: Equatable, Sendable {
    case signIn
    case signOut
}

struct TerminalSlashSuggestion: Equatable, Sendable {
    let completion: String
    let title: String
    let description: String
}

enum TerminalInputAction: Sendable {
    case none
    case quit
    case send(String)
    case obs(TerminalOBSIntent)
    case account(TerminalAccountIntent)
}

struct TerminalViewState: Sendable {
    let configuration: TerminalConfiguration
    let connectionState: BilibiliConnectionState
    let room: BilibiliRoomSnapshot?
    let events: [DanmuEvent]
    let questions: [QuestionRecord]
    let featuredEvent: DanmuEvent?
    var obsConfiguration: OBSConfiguration = OBSConfiguration()
    var obsStatus: OBSStatus = .unknown
    var obsActionInProgress = false
    var obsPasswordEntryActive = false
    var obsStopConfirmationPending = false
    var obsScenes: [String] = []
    var obsInputs: [String] = []
    var slashSuggestions: [TerminalSlashSuggestion] = []
    var selectedSlashSuggestion = 0
    let broadcasterNickname: String?
    let broadcasterAuthorID: String?
    let editor: TerminalEditorSnapshot
    let notice: String?
    let totalEventCount: Int
    var selectedEventID: String?
    let revision: UInt64

    var draft: String { editor.text }
}

actor TerminalState {
    private var configuration: TerminalConfiguration
    private let journal: (any DanmuSessionJournal)?
    private var connectionState: BilibiliConnectionState = .disconnected
    private var room: BilibiliRoomSnapshot?
    private var session: DanmuSession
    private var broadcasterNickname: String?
    private var broadcasterAuthorID: String?
    private var canonicalUsernamesByAuthorID: [String: String] = [:]
    private var canonicalIdentityByEventID: [String: TerminalCanonicalIdentity] = [:]
    private var canonicalIdentitiesByMaskedUsername: [String: Set<TerminalCanonicalIdentity>] = [:]
    private var deliveryTracker = DanmuDeliveryTracker()
    private var obsConfiguration: OBSConfiguration
    private var obsStatus: OBSStatus = .unknown
    private var obsActionInProgress = false
    private var obsPasswordEntryActive = false
    private var obsStopConfirmationPending = false
    private var obsScenes: [String] = []
    private var obsInputs: [String] = []
    private var accountActionInProgress = false
    private var selectedSlashSuggestion = 0
    private var isSlashPaletteDismissed = false
    private var deliveryBroadcasterAuthorID: String?
    private var needsHistoryIdentityRefresh = false
    private var isSending = false
    private var editor = TerminalLineEditor()
    private var inputDecoder = TerminalInputDecoder()
    private var notice: String?
    private var shouldQuit = false
    private var selectedEventID: String?
    private var frozenSelectionEvents: [DanmuEvent]?
    private var revision: UInt64 = 0

    init(
        configuration: TerminalConfiguration,
        journal: (any DanmuSessionJournal)? = nil,
        obsConfiguration: OBSConfiguration = OBSConfiguration()
    ) {
        self.configuration = configuration
        self.journal = journal
        self.obsConfiguration = obsConfiguration
        session = DanmuSession(roomID: configuration.roomID)
        _ = try? journal?.append(
            sessionID: session.id,
            kind: .sessionStarted,
            payload: DanmuJournalPayload(roomID: configuration.roomID),
            at: session.startedAt
        )
    }

    func consume(_ update: BilibiliDanmuUpdate) {
        switch update {
        case .connection(let state):
            connectionState = state
            switch state {
            case .error(let message): notice = message
            case .reconnecting(let attempt, let delay):
                notice = "连接中断，\(String(format: "%.0f", delay)) 秒后进行第 \(attempt) 次重连"
            case .connected: notice = nil
            case .connecting, .disconnected: break
            }
            revision &+= 1
        case .event(let incomingEvent):
            rememberCanonicalUsername(from: incomingEvent)
            // Session truth keeps unresolved masked aliases intact. Alias projection is
            // display-only until a history event proves this exact message identity.
            var event = canonicalDisplayEvent(incomingEvent, includeMaskedAlias: false)
            if isExpectedBroadcasterEcho(event) {
                let echoUsedMaskedUsername = incomingEvent.username.map(isMaskedUsername) == true
                event = canonicalExpectedBroadcasterEcho(event)
                if deliveryTracker.receive(event) {
                    needsHistoryIdentityRefresh = echoUsedMaskedUsername
                    notice = nil
                }
            }
            guard session.ingest(event) else { return }
            _ = try? journal?.append(
                sessionID: session.id,
                kind: .eventReceived,
                payload: DanmuJournalPayload(event: event),
                at: .now
            )
            if let question = session.question(for: event.id) {
                _ = try? journal?.append(
                    sessionID: session.id,
                    kind: .questionClassified,
                    payload: DanmuJournalPayload(
                        question: question,
                        isQuestion: true,
                        eventID: event.id,
                        classificationReason: question.recognition.reason
                    ),
                    at: .now
                )
            }
            revision &+= 1
        case .onlineCount(let count):
            guard let current = room else { return }
            room = BilibiliRoomSnapshot(
                roomID: current.roomID,
                broadcasterID: current.broadcasterID,
                title: current.title,
                parentAreaName: current.parentAreaName,
                areaName: current.areaName,
                onlineCount: count,
                followerCount: current.followerCount,
                liveStartedAt: current.liveStartedAt,
                onlineRankUsers: current.onlineRankUsers
            )
            revision &+= 1
        }
    }

    func updateRoom(_ snapshot: BilibiliRoomSnapshot) {
        room = snapshot
        revision &+= 1
    }

    func updateBroadcasterIdentity(nickname: String?, authorID: String?) {
        broadcasterNickname = nickname?.trimmingCharacters(in: .whitespacesAndNewlines)
        broadcasterAuthorID = authorID?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let nickname = broadcasterNickname,
           !nickname.isEmpty,
           let authorID = broadcasterAuthorID,
           !authorID.isEmpty {
            canonicalUsernamesByAuthorID[authorID] = nickname
        }
        revision &+= 1
    }

    func beginAwaitingEcho(
        message: String,
        broadcasterNickname: String,
        broadcasterAuthorID: String?,
        submittedAt: Date = .now
    ) {
        deliveryBroadcasterAuthorID = broadcasterAuthorID?.trimmingCharacters(in: .whitespacesAndNewlines)
        deliveryTracker.beginAwaitingEcho(
            message: message,
            broadcasterNickname: broadcasterNickname,
            at: submittedAt
        )
        notice = nil
        revision &+= 1
    }

    func markSubmittedUnlessAlreadyConfirmed() {
        guard case .awaitingEcho = deliveryTracker.state else { return }
    }

    func hasConfirmedEcho() -> Bool {
        if case .confirmed = deliveryTracker.state { return true }
        return false
    }

    func takeHistoryIdentityRefreshRequest() -> Bool {
        defer { needsHistoryIdentityRefresh = false }
        return needsHistoryIdentityRefresh
    }

    func refreshCanonicalUsernames(from events: [DanmuEvent]) {
        for event in events {
            rememberCanonicalUsername(from: event)
        }
        reconcileMaskedLiveEvents(with: events)
        revision &+= 1
    }

    func needsUsernameRefresh(now: Date = .now) -> Bool {
        session.recentEvents.contains { event in
            guard event.kind == .danmu,
                  event.origin == .live,
                  canonicalIdentityByEventID[event.id] == nil,
                  let username = event.username,
                  isMaskedUsername(username),
                  abs(now.timeIntervalSince(event.timestamp)) <= 90 else { return false }
            if canonicalIdentity(forMaskedUsername: username) != nil { return false }
            guard let authorID = usableAuthorID(event.authorID) else { return true }
            return canonicalUsernamesByAuthorID[authorID].map(isMaskedUsername) ?? true
        }
    }

    func expireEchoIfNeeded(timeout: TimeInterval = 15) {
        if deliveryTracker.expireIfNeeded(at: .now, timeout: timeout) {
            notice = "15 秒内未观察到回流，发送结果待确认"
            revision &+= 1
        }
    }

    func finishSending() {
        isSending = false
        revision &+= 1
    }

    func report(_ message: String?) {
        notice = message
        revision &+= 1
    }

    func snapshot() -> TerminalViewState {
        TerminalViewState(
            configuration: configuration,
            connectionState: connectionState,
            room: room,
            events: (frozenSelectionEvents ?? session.recentEvents).map { canonicalDisplayEvent($0) },
            questions: session.questionRecords
                .map(canonicalQuestion)
                .sorted { lhs, rhs in
                    if lhs.priority != rhs.priority { return lhs.priority == .high }
                    return lhs.queueSequence < rhs.queueSequence
                },
            featuredEvent: session.featuredEvent.map { canonicalDisplayEvent($0) },
            obsConfiguration: obsConfiguration,
            obsStatus: obsStatus,
            obsActionInProgress: obsActionInProgress,
            obsPasswordEntryActive: obsPasswordEntryActive,
            obsStopConfirmationPending: obsStopConfirmationPending,
            obsScenes: obsScenes,
            obsInputs: obsInputs,
            slashSuggestions: currentSlashSuggestions,
            selectedSlashSuggestion: selectedSlashSuggestion,
            broadcasterNickname: broadcasterNickname,
            broadcasterAuthorID: broadcasterAuthorID,
            editor: editor.snapshot,
            notice: notice,
            totalEventCount: session.metrics.totalEventCount,
            selectedEventID: selectedEventID,
            revision: revision
        )
    }

    func quitting() -> Bool { shouldQuit }

    func updateOBSStatus(_ status: OBSStatus) {
        obsStatus = status
        if status.stream != .live { obsStopConfirmationPending = false }
        revision &+= 1
    }

    func updateOBSScenes(_ scenes: [String]) {
        obsScenes = scenes
        revision &+= 1
    }

    func updateOBSInputs(_ inputs: [String]) {
        obsInputs = inputs
        revision &+= 1
    }

    func updateOBSConfiguration(_ configuration: OBSConfiguration) {
        obsConfiguration = configuration
        revision &+= 1
    }

    func reportOBSError(_ error: Error) {
        obsStatus = OBSStatus(
            connection: .unavailable,
            stream: .unknown,
            microphone: .unknown,
            lastError: error.localizedDescription,
            refreshedAt: .now
        )
        notice = error.localizedDescription
        revision &+= 1
    }

    func finishOBSAction(message: String? = nil) {
        obsActionInProgress = false
        if let message { notice = message }
        revision &+= 1
    }

    func finishAccountAction(message: String) {
        accountActionInProgress = false
        notice = message
        revision &+= 1
    }

    func finish() {
        session.end(reason: .completed)
        _ = try? journal?.append(
            sessionID: session.id,
            kind: .sessionEnded,
            payload: DanmuJournalPayload(endReason: .completed),
            at: .now
        )
        if let journal {
            _ = try? DanmuSessionArchive(journal: journal).writeSummary(sessionID: session.id)
        }
    }
    func handle(
        bytes: [UInt8],
        now: ContinuousClock.Instant = .now
    ) -> TerminalInputAction {
        let inputs = inputDecoder.feed(bytes, now: now)
        guard !inputs.isEmpty else { return .none }
        var action: TerminalInputAction = .none
        for input in inputs {
            if input == .control("c") {
                shouldQuit = true
                return .quit
            }

            action = handleComposerInput(input)
            if case .quit = action { shouldQuit = true }
            if case .send = action { break }
            if case .obs = action { break }
            if case .account = action { break }
        }
        revision &+= 1
        return action
    }

    private func handleComposerInput(_ input: TerminalInput) -> TerminalInputAction {
        if obsPasswordEntryActive {
            return handleOBSPasswordInput(input)
        }
        if selectedEventID != nil,
           input != .up,
           input != .down,
           input != .enter,
           input != .escape {
            return .none
        }
        switch input {
        case .text(let value), .paste(let value):
            editor.insert(value)
            reopenSlashPalette()
        case .left:
            editor.moveLeft()
        case .right:
            editor.moveRight()
        case .wordLeft:
            editor.moveWordLeft()
        case .wordRight:
            editor.moveWordRight()
        case .home:
            editor.moveHome()
        case .end:
            editor.moveEnd()
        case .backspace:
            editor.backspace()
            reopenSlashPalette()
        case .delete:
            editor.deleteForward()
            reopenSlashPalette()
        case .up:
            if !currentSlashSuggestions.isEmpty {
                selectedSlashSuggestion = (selectedSlashSuggestion - 1 + currentSlashSuggestions.count)
                    % currentSlashSuggestions.count
            } else {
                moveEventSelection(older: true)
            }
        case .down:
            if !currentSlashSuggestions.isEmpty {
                selectedSlashSuggestion = (selectedSlashSuggestion + 1) % currentSlashSuggestions.count
            } else {
                moveEventSelection(older: false)
            }
        case .backTab:
            cycleTheme()
        case .toggleMicrophone:
            return microphoneAction(muted: obsStatus.microphone != .muted)
        case .muteMicrophone:
            return microphoneAction(muted: true)
        case .tab:
            if !currentSlashSuggestions.isEmpty {
                acceptSelectedSlashSuggestion()
            } else {
                configuration.chatLayout.toggle()
            }
        case .enter:
            if selectedEventID != nil {
                mentionSelectedEvent()
                break
            }
            guard !isSending else {
                notice = "上一条弹幕仍在等待回流"
                break
            }
            if let selected = selectedSlashSuggestionValue {
                acceptSelectedSlashSuggestion()
                // 参数型候选先进入下一层（场景/输入列表），完整命令则直接执行。
                if selected.completion.last?.isWhitespace == true { break }
            }
            if let message = editor.submit() {
                isSlashPaletteDismissed = false
                selectedSlashSuggestion = 0
                if message.hasPrefix("/") {
                    return handleSlashCommand(message)
                }
                isSending = true
                return .send(message)
            }
        case .escape:
            if selectedEventID != nil {
                cancelEventSelection()
                return .none
            }
            if editor.snapshot.text.hasPrefix("/"), !isSlashPaletteDismissed {
                isSlashPaletteDismissed = true
                return .none
            }
            return .quit
        case .control(let key):
            switch key {
            case "a": editor.moveHome()
            case "e": editor.moveEnd()
            case "u": editor.eraseLine(); reopenSlashPalette()
            case "k": editor.eraseToEnd(); reopenSlashPalette()
            case "w": editor.eraseWordBackward(); reopenSlashPalette()
            case "d": editor.deleteForward(); reopenSlashPalette()
            default: break
            }
        }
        return .none
    }

    private var eventSelectionCandidates: [DanmuEvent] {
        (frozenSelectionEvents ?? session.recentEvents).filter { event in
            event.kind == .danmu
                && event.username?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false
        }
    }

    private func moveEventSelection(older: Bool) {
        guard configuration.showUsername else { return }
        if frozenSelectionEvents == nil { frozenSelectionEvents = session.recentEvents }
        let candidates = eventSelectionCandidates
        guard !candidates.isEmpty else {
            cancelEventSelection()
            return
        }
        guard let selectedEventID,
              let currentIndex = candidates.firstIndex(where: { $0.id == selectedEventID }) else {
            self.selectedEventID = candidates[0].id
            return
        }
        let nextIndex = older
            ? min(candidates.count - 1, currentIndex + 1)
            : max(0, currentIndex - 1)
        self.selectedEventID = candidates[nextIndex].id
    }

    private func mentionSelectedEvent() {
        guard let selectedEventID,
              let event = eventSelectionCandidates.first(where: { $0.id == selectedEventID }),
              let username = event.username?.trimmingCharacters(in: .whitespacesAndNewlines),
              !username.isEmpty else {
            cancelEventSelection()
            return
        }
        let snapshot = editor.snapshot
        let characters = Array(snapshot.text)
        let needsLeadingSpace = snapshot.cursor > 0
            && characters.indices.contains(snapshot.cursor - 1)
            && !characters[snapshot.cursor - 1].isWhitespace
        editor.insert("\(needsLeadingSpace ? " " : "")@\(username) ")
        cancelEventSelection()
        reopenSlashPalette()
    }

    private func cancelEventSelection() {
        selectedEventID = nil
        frozenSelectionEvents = nil
    }
    private var currentSlashSuggestions: [TerminalSlashSuggestion] {
        guard !obsPasswordEntryActive else { return [] }
        let draft = editor.snapshot.text
        guard draft.hasPrefix("/"), !isSlashPaletteDismissed else { return [] }
        let normalized = draft.lowercased()
        let scenePrefix = "/obs scene "
        if normalized.hasPrefix(scenePrefix) {
            let query = String(draft.dropFirst(scenePrefix.count)).lowercased()
            return configuredSceneSuggestions.filter {
                query.isEmpty
                    || $0.completion.lowercased().contains(query)
                    || $0.title.lowercased().contains(query)
            }
        }
        let microphonePrefix = "/obs config mic "
        if normalized.hasPrefix(microphonePrefix) {
            let query = String(draft.dropFirst(microphonePrefix.count)).lowercased()
            return configuredMicrophoneSuggestions.filter {
                query.isEmpty
                    || $0.completion.lowercased().contains(query)
                    || $0.title.lowercased().contains(query)
            }
        }
        return slashCommandDefinitions.filter {
            $0.completion.lowercased().hasPrefix(normalized)
                || $0.title.lowercased().contains(normalized.dropFirst())
        }
    }

    private var slashCommandDefinitions: [TerminalSlashSuggestion] {
        var commands = [
            TerminalSlashSuggestion(
                completion: "/login",
                title: "/login",
                description: "打开 B 站登录窗口，登录后可从当前 TUI 发送弹幕"
            ),
            TerminalSlashSuggestion(
                completion: "/logout",
                title: "/logout",
                description: "清除 TUI 独立保存的 B 站登录态"
            ),
            TerminalSlashSuggestion(
                completion: "/obs",
                title: "/obs",
                description: "显示 OBS slash command 帮助"
            ),
            TerminalSlashSuggestion(
                completion: "/obs connect",
                title: "/obs connect",
                description: "连接或重新连接已启动的 OBS，并同步场景与输入"
            ),
            TerminalSlashSuggestion(
                completion: "/obs status",
                title: "/obs status",
                description: "刷新连接、推流、OBS 现有场景和麦克风状态"
            ),
            TerminalSlashSuggestion(
                completion: "/obs config",
                title: "/obs config",
                description: "显示当前 OBS 配置和 TUI 内配置方法"
            ),
            TerminalSlashSuggestion(
                completion: "/obs config password",
                title: "/obs config password",
                description: "安全输入并更新 OBS WebSocket 连接密码"
            ),
            TerminalSlashSuggestion(
                completion: "/obs config mic ",
                title: "/obs config mic <输入>",
                description: "选择并保存 TUI 当前使用的 OBS 麦克风输入"
            ),
            TerminalSlashSuggestion(
                completion: "/obs mute",
                title: "/obs mute",
                description: "静音设置中指定的 OBS 麦克风输入"
            ),
            TerminalSlashSuggestion(
                completion: "/obs unmute",
                title: "/obs unmute",
                description: "取消指定 OBS 麦克风输入的静音"
            ),
            TerminalSlashSuggestion(
                completion: "/obs mic",
                title: "/obs mic",
                description: "根据 OBS 真实状态切换麦克风静音"
            ),
            TerminalSlashSuggestion(
                completion: "/obs scene ",
                title: "/obs scene <场景>",
                description: "从 OBS 实时场景列表中选择并切换 Program 场景"
            ),
            TerminalSlashSuggestion(
                completion: "/obs start",
                title: "/obs start",
                description: "切到默认直播场景并开始 OBS 推流"
            ),
            TerminalSlashSuggestion(
                completion: "/obs stop",
                title: "/obs stop",
                description: "请求停止 OBS 推流；不会立即执行，需要再次确认"
            ),
            TerminalSlashSuggestion(
                completion: "/names hide",
                title: "/names hide",
                description: "隐藏消息、提问和榜单中的用户名"
            ),
            TerminalSlashSuggestion(
                completion: "/names show",
                title: "/names show",
                description: "恢复显示用户名"
            ),
            TerminalSlashSuggestion(
                completion: "/names toggle",
                title: "/names toggle",
                description: "切换用户名显示状态"
            )
        ]
        if obsStopConfirmationPending {
            commands += [
                TerminalSlashSuggestion(
                    completion: "/obs confirm",
                    title: "/obs confirm",
                    description: "确认待执行的停播操作"
                ),
                TerminalSlashSuggestion(
                    completion: "/obs cancel",
                    title: "/obs cancel",
                    description: "取消待执行的停播操作"
                )
            ]
        }
        return commands
    }

    private var configuredSceneSuggestions: [TerminalSlashSuggestion] {
        obsScenes.map { scene in
            TerminalSlashSuggestion(
                completion: "/obs scene \(scene)",
                title: scene == obsStatus.currentScene ? "\(scene)（当前）" : scene,
                description: "切换到 OBS 现有场景“\(scene)”"
            )
        }
    }

    private var configuredMicrophoneSuggestions: [TerminalSlashSuggestion] {
        obsInputs.map { input in
            TerminalSlashSuggestion(
                completion: "/obs config mic \(input)",
                title: input == obsConfiguration.microphoneInputName ? "\(input)（当前）" : input,
                description: "选择 OBS 输入“\(input)”作为麦克风控制目标"
            )
        }
    }

    private func reopenSlashPalette() {
        isSlashPaletteDismissed = false
        selectedSlashSuggestion = 0
    }

    private var selectedSlashSuggestionValue: TerminalSlashSuggestion? {
        let suggestions = currentSlashSuggestions
        guard suggestions.indices.contains(selectedSlashSuggestion) else { return nil }
        return suggestions[selectedSlashSuggestion]
    }

    private func acceptSelectedSlashSuggestion() {
        guard let selected = selectedSlashSuggestionValue else { return }
        editor.eraseLine()
        editor.insert(selected.completion)
        selectedSlashSuggestion = 0
        isSlashPaletteDismissed = false
    }

    private func handleSlashCommand(_ command: String) -> TerminalInputAction {
        let tokens = command
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .split(whereSeparator: { $0.isWhitespace })
            .map(String.init)
        switch tokens.first?.lowercased() {
        case "/login":
            guard !accountActionInProgress else {
                notice = "上一项 B 站账号操作仍在执行。"
                return .none
            }
            accountActionInProgress = true
            notice = "正在打开 B 站登录窗口…"
            return .account(.signIn)
        case "/logout":
            guard !accountActionInProgress else {
                notice = "上一项 B 站账号操作仍在执行。"
                return .none
            }
            accountActionInProgress = true
            notice = "正在清除 B 站登录态…"
            return .account(.signOut)
        default:
            break
        }
        if tokens.first?.lowercased() == "/names" {
            switch tokens.dropFirst().first?.lowercased() ?? "toggle" {
            case "hide", "off":
                configuration.showUsername = false
                notice = "已隐藏 TUI 用户名"
            case "show", "on":
                configuration.showUsername = true
                notice = "已恢复显示 TUI 用户名"
            case "toggle":
                configuration.showUsername.toggle()
                notice = configuration.showUsername ? "已恢复显示 TUI 用户名" : "已隐藏 TUI 用户名"
            default:
                notice = "用法：/names hide | show | toggle"
            }
            return .none
        }
        guard tokens.first?.lowercased() == "/obs" else {
            notice = "未知命令。输入 / 查看可用命令。"
            return .none
        }
        guard !obsActionInProgress else {
            notice = "上一项 OBS 操作仍在执行。"
            return .none
        }
        let verb = tokens.dropFirst().first?.lowercased() ?? "help"
        switch verb {
        case "help":
            notice = "/obs connect | status | config password | config mic <输入> | mute | unmute | scene <场景> | start | stop"
            return .none
        case "connect", "reconnect":
            obsActionInProgress = true
            return .obs(.connect)
        case "status", "refresh":
            obsActionInProgress = true
            return .obs(.refresh)
        case "config", "configure":
            let section = tokens.dropFirst(2).first?.lowercased()
            if section == "password" || section == "pass" {
                obsPasswordEntryActive = true
                notice = "请输入新的 OBS WebSocket 密码；输入内容不会显示，Esc 或留空回车取消。"
                return .none
            }
            guard section == "mic" || section == "microphone" || section == "input" else {
                notice = "当前连接：\(obsConfiguration.host):\(obsConfiguration.port)，麦克风：\(obsConfiguration.microphoneInputName)。输入 /obs config password 更新密码，或 /obs config mic 选择麦克风。"
                return .none
            }
            let requested = tokens.dropFirst(3).joined(separator: " ")
            guard !requested.isEmpty else {
                notice = obsInputs.isEmpty
                    ? "尚未读取到 OBS 输入，请先启动 OBS 并输入 /obs connect。"
                    : "OBS 输入：" + obsInputs.joined(separator: "、")
                return .none
            }
            guard obsInputs.isEmpty || obsInputs.contains(requested) else {
                notice = "OBS 中没有输入“\(requested)”。输入 /obs config mic 查看现有输入。"
                return .none
            }
            obsActionInProgress = true
            return .obs(.configureMicrophone(requested))
        case "mute":
            return microphoneAction(muted: true)
        case "unmute":
            return microphoneAction(muted: false)
        case "mic":
            return microphoneAction(muted: obsStatus.microphone != .muted)
        case "scene", "scenes":
            let requested = tokens.dropFirst(2).joined(separator: " ")
            guard !requested.isEmpty else {
                if obsScenes.isEmpty {
                    notice = "尚未读取到 OBS 场景，请输入 /obs status 刷新。"
                } else {
                    notice = "OBS 场景：" + obsScenes.joined(separator: "、")
                }
                return .none
            }
            guard obsScenes.isEmpty || obsScenes.contains(requested) else {
                notice = "OBS 中没有场景“\(requested)”。输入 /obs scene 查看现有场景。"
                return .none
            }
            obsActionInProgress = true
            return .obs(.switchScene(requested))
        case "start":
            obsStopConfirmationPending = false
            obsActionInProgress = true
            return .obs(.startStreaming)
        case "stop":
            guard obsStatus.stream == .live else {
                notice = "OBS 当前未显示为直播状态，未执行停播。"
                return .none
            }
            obsStopConfirmationPending = true
            notice = "停播只会停止 OBS 推流。请输入 /obs confirm 确认，或 /obs cancel 取消。"
            return .none
        case "confirm":
            guard obsStopConfirmationPending else {
                notice = "当前没有待确认的停播操作。"
                return .none
            }
            obsStopConfirmationPending = false
            obsActionInProgress = true
            return .obs(.stopStreaming)
        case "cancel":
            obsStopConfirmationPending = false
            notice = "已取消停播。"
            return .none
        default:
            notice = "未知 OBS 命令。输入 /obs 查看帮助。"
            return .none
        }
    }

    private func handleOBSPasswordInput(_ input: TerminalInput) -> TerminalInputAction {
        switch input {
        case .text(let value), .paste(let value):
            editor.insert(value)
        case .left:
            editor.moveLeft()
        case .right:
            editor.moveRight()
        case .wordLeft:
            editor.moveWordLeft()
        case .wordRight:
            editor.moveWordRight()
        case .home:
            editor.moveHome()
        case .end:
            editor.moveEnd()
        case .backspace:
            editor.backspace()
        case .delete:
            editor.deleteForward()
        case .enter:
            let password = editor.takeValueWithoutHistory()
            obsPasswordEntryActive = false
            guard !password.isEmpty else {
                notice = "已取消更新 OBS 密码。"
                return .none
            }
            obsActionInProgress = true
            notice = "正在保存并验证新的 OBS 密码…"
            return .obs(.configurePassword(password))
        case .escape:
            editor.eraseLine()
            obsPasswordEntryActive = false
            notice = "已取消更新 OBS 密码。"
        case .control(let key):
            switch key {
            case "a": editor.moveHome()
            case "e": editor.moveEnd()
            case "u": editor.eraseLine()
            case "k": editor.eraseToEnd()
            case "w": editor.eraseWordBackward()
            case "d": editor.deleteForward()
            default: break
            }
        case .up, .down, .tab, .backTab, .toggleMicrophone, .muteMicrophone:
            break
        }
        return .none
    }

    private func microphoneAction(muted: Bool) -> TerminalInputAction {
        guard obsStatus.microphone != .unknown else {
            notice = "麦克风状态未知，请先输入 /obs status。"
            return .none
        }
        guard !obsActionInProgress else {
            notice = "上一项 OBS 操作仍在执行"
            return .none
        }
        obsActionInProgress = true
        return .obs(.setMuted(muted, input: obsConfiguration.microphoneInputName))
    }

    private var broadcasterIdentity: BroadcasterIdentity? {
        BroadcasterIdentity(nickname: broadcasterNickname, authorID: broadcasterAuthorID)
    }

    private func canonicalDisplayEvent(
        _ event: DanmuEvent,
        includeMaskedAlias: Bool = true
    ) -> DanmuEvent {
        let resolvedEvent: DanmuEvent
        let maskedAliasIdentity = includeMaskedAlias && usableAuthorID(event.authorID) == nil
            ? event.username.flatMap(canonicalIdentity(forMaskedUsername:))
            : nil
        if let identity = canonicalIdentityByEventID[event.id] ?? maskedAliasIdentity {
            resolvedEvent = replacingIdentity(
                of: event,
                username: identity.username,
                authorID: identity.authorID
            )
        } else {
            resolvedEvent = event
        }
        let broadcasterEvent = broadcasterIdentity?.canonicalizing(resolvedEvent) ?? resolvedEvent
        guard let authorID = usableAuthorID(broadcasterEvent.authorID),
              let canonicalUsername = canonicalUsernamesByAuthorID[authorID],
              broadcasterEvent.username != canonicalUsername else {
            return broadcasterEvent
        }
        return replacingIdentity(
            of: broadcasterEvent,
            username: canonicalUsername,
            authorID: broadcasterEvent.authorID
        )
    }

    private func canonicalQuestion(_ question: QuestionRecord) -> QuestionRecord {
        QuestionRecord(
            event: canonicalDisplayEvent(question.event),
            recognition: question.recognition,
            priority: question.priority,
            queueSequence: question.queueSequence,
            status: question.status
        )
    }

    private func rememberCanonicalUsername(from event: DanmuEvent) {
        guard let authorID = usableAuthorID(event.authorID),
              let username = event.username?.trimmingCharacters(in: .whitespacesAndNewlines),
              !username.isEmpty else { return }
        if let existing = canonicalUsernamesByAuthorID[authorID],
           !isMaskedUsername(existing),
           isMaskedUsername(username) {
            return
        }
        canonicalUsernamesByAuthorID[authorID] = username
    }

    private func reconcileMaskedLiveEvents(with historyEvents: [DanmuEvent]) {
        var claimedEventIDs = Set(canonicalIdentityByEventID.keys)
        for historyEvent in historyEvents {
            guard historyEvent.kind == .danmu,
                  historyEvent.origin == .history,
                  let fullUsername = historyEvent.username?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !fullUsername.isEmpty,
                  !isMaskedUsername(fullUsername),
                  let fullAuthorID = usableAuthorID(historyEvent.authorID) else { continue }

            let candidates = session.recentEvents.filter { liveEvent in
                guard liveEvent.kind == .danmu,
                      liveEvent.origin == .live,
                      !claimedEventIDs.contains(liveEvent.id),
                      abs(liveEvent.timestamp.timeIntervalSince(historyEvent.timestamp)) <= 3,
                      QuestionClassifier.normalized(liveEvent.content)
                        == QuestionClassifier.normalized(historyEvent.content),
                      let liveUsername = liveEvent.username else { return false }
                if let liveAuthorID = usableAuthorID(liveEvent.authorID) {
                    return liveAuthorID == fullAuthorID
                }
                return liveUsername.caseInsensitiveCompare(fullUsername) == .orderedSame
                    || maskedUsername(liveUsername, matches: fullUsername)
            }
            // 重复内容或相同脱敏前缀无法可靠区分时宁可保留脱敏名，也不猜错人。
            guard candidates.count == 1, let liveEvent = candidates.first else { continue }
            let identity = TerminalCanonicalIdentity(username: fullUsername, authorID: fullAuthorID)
            canonicalIdentityByEventID[liveEvent.id] = identity
            if let maskedUsername = liveEvent.username, isMaskedUsername(maskedUsername) {
                canonicalIdentitiesByMaskedUsername[maskedUsernameKey(maskedUsername), default: []].insert(identity)
            }
            claimedEventIDs.insert(liveEvent.id)
        }
    }

    private func canonicalIdentity(forMaskedUsername username: String) -> TerminalCanonicalIdentity? {
        guard isMaskedUsername(username),
              let identities = canonicalIdentitiesByMaskedUsername[maskedUsernameKey(username)],
              identities.count == 1 else { return nil }
        return identities.first
    }

    private func maskedUsernameKey(_ username: String) -> String {
        username
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "＊", with: "*")
            .lowercased()
    }

    private func replacingIdentity(
        of event: DanmuEvent,
        username: String,
        authorID: String?
    ) -> DanmuEvent {
        let content: String
        if event.kind != .danmu,
           let previousUsername = event.username,
           event.content.hasPrefix(previousUsername) {
            content = username + event.content.dropFirst(previousUsername.count)
        } else {
            content = event.content
        }
        return DanmuEvent(
            id: event.id,
            kind: event.kind,
            timestamp: event.timestamp,
            username: username,
            authorID: authorID,
            content: content,
            origin: event.origin,
            platformEventID: event.platformEventID,
            emotes: event.emotes
        )
    }

    private func usableAuthorID(_ value: String?) -> String? {
        guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty,
              value != "0" else { return nil }
        return value
    }

    private func isMaskedUsername(_ username: String) -> Bool {
        username.contains("*") || username.contains("＊")
    }

    private func isExpectedBroadcasterEcho(_ event: DanmuEvent) -> Bool {
        guard case .awaitingEcho(let expectedMessage, let expectedNickname, let submittedAt) = deliveryTracker.state,
              event.kind == .danmu,
              event.content.trimmingCharacters(in: .whitespacesAndNewlines)
                == expectedMessage.trimmingCharacters(in: .whitespacesAndNewlines),
              event.timestamp >= submittedAt.addingTimeInterval(-3),
              event.timestamp <= submittedAt.addingTimeInterval(15) else { return false }
        if let expectedAuthorID = deliveryBroadcasterAuthorID,
           let eventAuthorID = event.authorID?.trimmingCharacters(in: .whitespacesAndNewlines),
           !eventAuthorID.isEmpty,
           eventAuthorID != "0",
           eventAuthorID == expectedAuthorID {
            return true
        }
        if broadcasterIdentity?.matches(event) == true { return true }
        guard let username = event.username else { return false }
        return maskedUsername(username, matches: expectedNickname)
    }

    private func maskedUsername(_ masked: String, matches full: String) -> Bool {
        let masked = masked.trimmingCharacters(in: .whitespacesAndNewlines)
        let full = full.trimmingCharacters(in: .whitespacesAndNewlines)
        guard isMaskedUsername(masked), !full.isEmpty else { return false }
        let normalized = masked.replacingOccurrences(of: "＊", with: "*").lowercased()
        let normalizedFull = full.lowercased()
        let parts = normalized.split(separator: "*", omittingEmptySubsequences: true).map(String.init)
        guard !parts.isEmpty else { return false }
        if let prefix = parts.first, !normalizedFull.hasPrefix(prefix) { return false }
        if parts.count > 1, let suffix = parts.last, !normalizedFull.hasSuffix(suffix) { return false }
        return true
    }

    private func canonicalExpectedBroadcasterEcho(_ event: DanmuEvent) -> DanmuEvent {
        guard case .awaitingEcho(_, let nickname, _) = deliveryTracker.state else { return event }
        if let authorID = deliveryBroadcasterAuthorID, !authorID.isEmpty {
            canonicalUsernamesByAuthorID[authorID] = nickname
        }
        return DanmuEvent(
            id: event.id,
            kind: event.kind,
            timestamp: event.timestamp,
            username: nickname,
            authorID: deliveryBroadcasterAuthorID ?? event.authorID,
            content: event.content,
            origin: event.origin,
            platformEventID: event.platformEventID,
            emotes: event.emotes
        )
    }

    private func cycleTheme() {
        let themes = TerminalTheme.allCases
        guard let index = themes.firstIndex(of: configuration.theme) else {
            configuration.theme = .shisui
            return
        }
        configuration.theme = themes[(index + 1) % themes.count]
    }

}
