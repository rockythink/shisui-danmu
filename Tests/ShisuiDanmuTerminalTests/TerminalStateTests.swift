import BilibiliDanmu
import DanmuCore
import Foundation
import OBSControl
import Testing
@testable import ShisuiDanmuTerminal

@Suite("Terminal interaction state")
struct TerminalStateTests {
    @Test func arrowsSelectDanmuWithoutChangingDraft() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        for index in 0..<4 {
            await state.consume(.event(DanmuEvent(
                id: "event-\(index)",
                kind: .danmu,
                timestamp: .now,
                username: "观众\(index)",
                content: "消息\(index)"
            )))
        }
        _ = await state.handle(bytes: Array("草稿".utf8))

        _ = await state.handle(bytes: Array("\u{001B}[A".utf8))
        #expect(await state.snapshot().selectedEventID == "event-3")
        _ = await state.handle(bytes: Array("\u{001B}[A".utf8))
        #expect(await state.snapshot().selectedEventID == "event-2")
        _ = await state.handle(bytes: Array("\u{001B}[B".utf8))
        let snapshot = await state.snapshot()
        #expect(snapshot.selectedEventID == "event-3")
        #expect(snapshot.draft == "草稿")
    }

    @Test func ctrlUAndBackspaceEditDraft() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)

        _ = await state.handle(bytes: Array("测试A".utf8) + [0x7F])
        #expect(await state.snapshot().draft == "测试")
        _ = await state.handle(bytes: [0x15])
        #expect(await state.snapshot().draft.isEmpty)
    }

    @Test func preservesUTF8CharactersSplitAcrossReads() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let bytes = Array("中文".utf8)

        _ = await state.handle(bytes: Array(bytes.prefix(2)))
        _ = await state.handle(bytes: Array(bytes.dropFirst(2)))

        #expect(await state.snapshot().draft == "中文")
    }

    @Test func terminalSessionWritesEventsAndEndToSharedJournal() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let journal = InMemoryDanmuSessionJournal()
        let state = TerminalState(configuration: configuration, journal: journal)
        let event = DanmuEvent(
            id: "journal-question",
            kind: .danmu,
            timestamp: .now,
            username: "观众",
            content: "为什么会这样？"
        )

        await state.consume(.event(event))
        await state.finish()

        let sessionID = try #require(journal.sessionIDs().first)
        let kinds = try journal.records(sessionID: sessionID).map(\.kind)
        #expect(kinds.contains(.sessionStarted))
        #expect(kinds.contains(.eventReceived))
        #expect(kinds.contains(.questionClassified))
        #expect(kinds.last == .sessionEnded)
    }

    @Test func maskedBroadcasterNicknameIsCanonicalizedBeforeRendering() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.updateBroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237")
        await state.beginAwaitingEcho(
            message: "测试",
            broadcasterNickname: "停车拾穗",
            broadcasterAuthorID: "7604237"
        )
        await state.markSubmittedUnlessAlreadyConfirmed()
        #expect(await state.snapshot().events.isEmpty)
        #expect(await state.snapshot().notice == nil)

        await state.consume(.event(DanmuEvent(
            id: "masked",
            kind: .danmu,
            timestamp: .now,
            username: "停***",
            authorID: "0",
            content: "测试"
        )))

        let snapshot = await state.snapshot()
        let event = try #require(snapshot.events.first)
        #expect(event.username == "停车拾穗")
        #expect(event.authorID == "7604237")
        #expect(snapshot.notice == nil)
        #expect(await state.hasConfirmedEcho())
        #expect(await state.takeHistoryIdentityRefreshRequest())
        #expect(await state.takeHistoryIdentityRefreshRequest() == false)
    }

    @Test func unrelatedMaskedNicknameCannotConfirmSubmissionWithTheSameContent() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let submittedAt = Date(timeIntervalSince1970: 1_000)
        await state.updateBroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237")
        await state.beginAwaitingEcho(
            message: "相同内容",
            broadcasterNickname: "停车拾穗",
            broadcasterAuthorID: "7604237",
            submittedAt: submittedAt
        )

        await state.consume(.event(DanmuEvent(
            id: "other-masked",
            kind: .danmu,
            timestamp: submittedAt.addingTimeInterval(1),
            username: "观***",
            authorID: "0",
            content: "相同内容"
        )))

        #expect(await state.hasConfirmedEcho() == false)
        #expect(await state.snapshot().events.first?.username == "观***")
    }

    @Test func fullLiveNicknameReplacesMaskedHistoryNicknameForTheSameUser() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.consume(.event(DanmuEvent(
            id: "masked-history",
            kind: .danmu,
            timestamp: .now,
            username: "观***",
            authorID: "viewer-1",
            content: "历史消息",
            origin: .history
        )))
        await state.consume(.event(DanmuEvent(
            id: "full-live",
            kind: .danmu,
            timestamp: .now,
            username: "观众完整昵称",
            authorID: "viewer-1",
            content: "实时消息"
        )))

        let snapshot = await state.snapshot()
        #expect(snapshot.events.filter { $0.authorID == "viewer-1" }.allSatisfy {
            $0.username == "观众完整昵称"
        })
    }

    @Test func maskedLiveNicknameDoesNotReplaceKnownFullNickname() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.consume(.event(DanmuEvent(
            id: "full-first",
            kind: .danmu,
            timestamp: .now,
            username: "观众完整昵称",
            authorID: "viewer-1",
            content: "第一条"
        )))
        await state.consume(.event(DanmuEvent(
            id: "masked-later",
            kind: .danmu,
            timestamp: .now,
            username: "观***",
            authorID: "viewer-1",
            content: "第二条"
        )))

        let snapshot = await state.snapshot()
        #expect(snapshot.events.filter { $0.authorID == "viewer-1" }.allSatisfy {
            $0.username == "观众完整昵称"
        })
    }

    @Test func placeholderAuthorIDsNeverShareCanonicalNames() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.consume(.event(DanmuEvent(
            id: "masked-a",
            kind: .danmu,
            timestamp: .now,
            username: "煜***",
            authorID: "0",
            content: "第一条"
        )))
        await state.consume(.event(DanmuEvent(
            id: "masked-b",
            kind: .danmu,
            timestamp: .now,
            username: "D***",
            authorID: "0",
            content: "第二条"
        )))

        let namesByID = Dictionary(uniqueKeysWithValues: await state.snapshot().events.map { ($0.id, $0.username) })
        #expect(namesByID["masked-a"] == "煜***")
        #expect(namesByID["masked-b"] == "D***")
    }

    @Test func historyReconcilesOneMaskedLiveEventWithoutGuessingByPrefixAlone() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let timestamp = Date(timeIntervalSince1970: 1_000)
        await state.consume(.event(DanmuEvent(
            id: "masked-live",
            kind: .danmu,
            timestamp: timestamp,
            username: "D***",
            authorID: "0",
            content: "同一条内容"
        )))

        await state.refreshCanonicalUsernames(from: [DanmuEvent(
            id: "full-history",
            kind: .danmu,
            timestamp: timestamp,
            username: "Darkkk丶",
            authorID: "153499714",
            content: "同一条内容",
            origin: .history
        )])

        let event = try #require(await state.snapshot().events.first)
        #expect(event.username == "Darkkk丶")
        #expect(event.authorID == "153499714")
    }

    @Test func uniquelyProvenMaskedAliasAlsoRepairsLaterGiftNamesAndSummaries() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let timestamp = Date(timeIntervalSince1970: 2_000)
        await state.consume(.event(DanmuEvent(
            id: "masked-proof",
            kind: .danmu,
            timestamp: timestamp,
            username: "D***",
            authorID: "0",
            content: "用于确认身份"
        )))
        await state.refreshCanonicalUsernames(from: [DanmuEvent(
            id: "history-proof",
            kind: .danmu,
            timestamp: timestamp,
            username: "Darkkk丶",
            authorID: "153499714",
            content: "用于确认身份",
            origin: .history
        )])
        await state.consume(.event(DanmuEvent(
            id: "masked-gift",
            kind: .gift,
            timestamp: timestamp.addingTimeInterval(1),
            username: "D***",
            content: "D*** 送出 牛哇牛哇 x1"
        )))

        let gift = try #require((await state.snapshot().events).first { $0.id == "masked-gift" })
        #expect(gift.username == "Darkkk丶")
        #expect(gift.authorID == "153499714")
        #expect(gift.content == "Darkkk丶 送出 牛哇牛哇 x1")
    }

    @Test func ambiguousMaskedAliasIsNotAppliedToFutureEvents() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let firstTime = Date(timeIntervalSince1970: 3_000)
        await state.consume(.event(DanmuEvent(
            id: "masked-first",
            kind: .danmu,
            timestamp: firstTime,
            username: "D***",
            authorID: "0",
            content: "第一位用户"
        )))
        await state.refreshCanonicalUsernames(from: [DanmuEvent(
            id: "history-first",
            kind: .danmu,
            timestamp: firstTime,
            username: "Darkkk丶",
            authorID: "153499714",
            content: "第一位用户",
            origin: .history
        )])

        let secondTime = firstTime.addingTimeInterval(10)
        await state.consume(.event(DanmuEvent(
            id: "masked-second",
            kind: .danmu,
            timestamp: secondTime,
            username: "D***",
            authorID: "0",
            content: "第二位用户"
        )))
        await state.refreshCanonicalUsernames(from: [DanmuEvent(
            id: "history-second",
            kind: .danmu,
            timestamp: secondTime,
            username: "DifferentUser",
            authorID: "other-id",
            content: "第二位用户",
            origin: .history
        )])
        await state.consume(.event(DanmuEvent(
            id: "ambiguous-future",
            kind: .gift,
            timestamp: secondTime.addingTimeInterval(1),
            username: "D***",
            content: "D*** 送出礼物"
        )))

        let future = try #require((await state.snapshot().events).first { $0.id == "ambiguous-future" })
        #expect(future.username == "D***")
        #expect(future.authorID == nil)
    }

    @Test func oldHistoryCannotConfirmANewSubmission() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let submittedAt = Date(timeIntervalSince1970: 1_000)
        await state.updateBroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237")
        await state.beginAwaitingEcho(
            message: "重复内容",
            broadcasterNickname: "停车拾穗",
            broadcasterAuthorID: "7604237",
            submittedAt: submittedAt
        )

        await state.consume(.event(DanmuEvent(
            id: "old-history",
            kind: .danmu,
            timestamp: submittedAt.addingTimeInterval(-10),
            username: "停***",
            authorID: "7604237",
            content: "重复内容",
            origin: .history
        )))
        #expect(await state.hasConfirmedEcho() == false)

        await state.consume(.event(DanmuEvent(
            id: "new-live",
            kind: .danmu,
            timestamp: submittedAt.addingTimeInterval(1),
            username: "停***",
            authorID: "7604237",
            content: "重复内容"
        )))
        #expect(await state.hasConfirmedEcho())
    }

    @Test func identityLoadedAfterEventStillCanonicalizesExistingProjection() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.consume(.event(DanmuEvent(
            id: "early-masked",
            kind: .danmu,
            timestamp: .now,
            username: "停***",
            authorID: "7604237",
            content: "先于账号状态到达"
        )))

        await state.updateBroadcasterIdentity(nickname: "停车拾穗", authorID: "7604237")

        #expect(await state.snapshot().events.first?.username == "停车拾穗")
    }

    @Test func f9ShortcutsToggleAndMuteConfiguredMicrophone() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(
            configuration: configuration,
            obsConfiguration: OBSConfiguration(microphoneInputName: "桌面麦克风")
        )
        await state.updateOBSStatus(OBSStatus(
            connection: .connected,
            stream: .stopped,
            microphone: .unmuted
        ))

        let toggle = await state.handle(bytes: Array("\u{001B}[20~".utf8))
        guard case .obs(.setMuted(true, let toggleInput)) = toggle else {
            Issue.record("Expected F9 to toggle microphone mute")
            return
        }
        #expect(toggleInput == "桌面麦克风")
        await state.finishOBSAction()
        await state.updateOBSStatus(OBSStatus(
            connection: .connected,
            stream: .stopped,
            microphone: .unmuted
        ))

        let mute = await state.handle(bytes: Array("\u{001B}[20;2~".utf8))
        guard case .obs(.setMuted(true, let muteInput)) = mute else {
            Issue.record("Expected Shift-F9 to force microphone mute")
            return
        }
        #expect(muteInput == "桌面麦克风")
    }

    @Test func tabTogglesLayoutWithoutChangingDraft() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        _ = await state.handle(bytes: Array("草稿".utf8))

        #expect(await state.snapshot().configuration.chatLayout == false)

        _ = await state.handle(bytes: [0x09])
        var snapshot = await state.snapshot()
        #expect(snapshot.configuration.chatLayout)
        #expect(snapshot.draft == "草稿")

        _ = await state.handle(bytes: [0x09])
        snapshot = await state.snapshot()
        #expect(snapshot.configuration.chatLayout == false)
        #expect(snapshot.draft == "草稿")
    }

    @Test func slashPaletteFiltersCompletesAndExplainsCommands() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)

        _ = await state.handle(bytes: Array("/obs st".utf8))
        var snapshot = await state.snapshot()
        #expect(snapshot.slashSuggestions.contains { $0.completion == "/obs status" })
        #expect(snapshot.slashSuggestions.contains { $0.completion == "/obs start" })
        #expect(snapshot.slashSuggestions.allSatisfy { !$0.description.isEmpty })

        _ = await state.handle(bytes: [0x09])
        snapshot = await state.snapshot()
        #expect(snapshot.draft == "/obs status")
        #expect(snapshot.configuration.chatLayout == false)
    }

    @Test func accountSlashCommandsAreDiscoverableAndProduceActions() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let paletteState = TerminalState(configuration: configuration)

        _ = await paletteState.handle(bytes: Array("/".utf8))
        let suggestions = await paletteState.snapshot().slashSuggestions
        #expect(suggestions.contains { $0.completion == "/login" })
        #expect(suggestions.contains { $0.completion == "/logout" })

        let loginState = TerminalState(configuration: configuration)
        let loginAction = await loginState.handle(bytes: Array("/login".utf8) + [0x0D])
        if case .account(.signIn) = loginAction {} else {
            Issue.record("/login should open the Bilibili login flow")
        }

        let logoutState = TerminalState(configuration: configuration)
        let logoutAction = await logoutState.handle(bytes: Array("/logout".utf8) + [0x0D])
        if case .account(.signOut) = logoutAction {} else {
            Issue.record("/logout should clear the Bilibili login state")
        }
    }

    @Test func enterExecutesTheSelectedSlashSuggestion() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)

        _ = await state.handle(bytes: Array("/obs st".utf8))
        let action = await state.handle(bytes: [0x0D])

        if case .obs(.refresh) = action {} else {
            Issue.record("Enter should execute the selected /obs status suggestion")
        }
        #expect(await state.snapshot().draft.isEmpty)
    }

    @Test func namesSlashCommandTogglesUsernameVisibility() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)

        _ = await state.handle(bytes: Array("/names hide\r".utf8))
        #expect(await state.snapshot().configuration.showUsername == false)
        _ = await state.handle(bytes: Array("/names show\r".utf8))
        #expect(await state.snapshot().configuration.showUsername)
    }

    @Test func slashPaletteSuggestsConfiguredScenes() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.updateOBSScenes(["知识直播", "问答场"])

        _ = await state.handle(bytes: Array("/obs scene ".utf8))
        let snapshot = await state.snapshot()
        #expect(snapshot.slashSuggestions.contains { $0.title == "知识直播" })
        #expect(snapshot.slashSuggestions.contains { $0.title == "问答场" })
    }

    @Test func slashPaletteSuggestsOBSInputsAndConfiguresMicrophoneInSession() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let obsConfiguration = OBSConfiguration(microphoneInputName: "桌面麦克风")
        let state = TerminalState(configuration: configuration, obsConfiguration: obsConfiguration)
        await state.updateOBSInputs(["桌面麦克风", "无线麦克风"])

        _ = await state.handle(bytes: Array("/obs config mic ".utf8))
        var snapshot = await state.snapshot()
        #expect(snapshot.slashSuggestions.contains { $0.title == "桌面麦克风（当前）" })
        #expect(snapshot.slashSuggestions.contains { $0.completion == "/obs config mic 无线麦克风" })

        let action = await state.handle(bytes: Array("无线麦克风\r".utf8))
        if case .obs(.configureMicrophone(let input)) = action {
            #expect(input == "无线麦克风")
        } else {
            Issue.record("Expected an in-session microphone configuration action")
        }

        snapshot = await state.snapshot()
        #expect(snapshot.draft.isEmpty)
        #expect(snapshot.obsActionInProgress)
    }

    @Test func obsConnectHasAnExplicitRuntimeAction() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)

        let action = await state.handle(bytes: Array("/obs connect\r".utf8))
        if case .obs(.connect) = action {} else {
            Issue.record("/obs connect should request a runtime OBS connection")
        }
    }

    @Test func obsPasswordCanBeUpdatedSecurelyInsideTheRunningTUI() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)

        _ = await state.handle(bytes: Array("/".utf8))
        #expect(await state.snapshot().slashSuggestions.contains {
            $0.completion == "/obs config password"
        })
        _ = await state.handle(bytes: [0x15])
        _ = await state.handle(bytes: Array("/obs config password\r".utf8))
        var snapshot = await state.snapshot()
        #expect(snapshot.obsPasswordEntryActive)
        #expect(snapshot.slashSuggestions.isEmpty)

        _ = await state.handle(bytes: Array("new-secret".utf8))
        snapshot = await state.snapshot()
        let rendered = TerminalRenderer().render(
            snapshot,
            size: TerminalSize(columns: 100, rows: 24)
        )
        #expect(!rendered.contains("new-secret"))

        let action = await state.handle(bytes: [0x0D])
        if case .obs(.configurePassword(let password)) = action {
            #expect(password == "new-secret")
        } else {
            Issue.record("Expected an in-session OBS password update action")
        }
        #expect(await state.snapshot().obsPasswordEntryActive == false)

        await state.finishOBSAction()
        _ = await state.handle(bytes: [0x1B, 0x5B, 0x41])
        #expect(await state.snapshot().draft != "new-secret")
    }

    @Test func escapeDismissesSlashPaletteBeforeQuitting() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        _ = await state.handle(bytes: Array("/".utf8))
        #expect(await state.snapshot().slashSuggestions.isEmpty == false)

        let now = ContinuousClock.now
        _ = await state.handle(bytes: [0x1B], now: now)
        _ = await state.handle(bytes: [], now: now.advanced(by: .milliseconds(50)))
        #expect(await state.quitting() == false)
        #expect(await state.snapshot().slashSuggestions.isEmpty)

        _ = await state.handle(bytes: Array("o".utf8))
        #expect(await state.snapshot().slashSuggestions.isEmpty == false)
    }

    @Test func obsSlashCommandMapsConfiguredSceneWithoutSendingDanmu() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.updateOBSScenes(["讲解", "答疑", "休息", "访谈"])

        let action = await state.handle(bytes: Array("/obs scene 答疑\r".utf8))
        if case .obs(.switchScene(let scene)) = action {
            #expect(scene == "答疑")
        } else {
            Issue.record("Expected OBS scene action")
        }
        #expect(await state.snapshot().draft.isEmpty)
    }

    @Test func obsStopRequiresASecondSlashCommand() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.updateOBSStatus(OBSStatus(connection: .connected, stream: .live))

        let first = await state.handle(bytes: Array("/obs stop\r".utf8))
        if case .none = first {} else { Issue.record("/obs stop must not stop immediately") }
        #expect(await state.snapshot().obsStopConfirmationPending)
        #expect(await state.snapshot().notice?.contains("/obs confirm") == true)

        let confirmed = await state.handle(bytes: Array("/obs confirm\r".utf8))
        if case .obs(.stopStreaming) = confirmed {} else {
            Issue.record("/obs confirm should execute the pending stop")
        }
        #expect(await state.snapshot().obsStopConfirmationPending == false)
    }

    @Test func unknownSlashCommandIsNotSentAsDanmu() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let action = await state.handle(bytes: Array("/unknown\r".utf8))
        if case .none = action {} else { Issue.record("Unknown slash commands must stay local") }
        #expect(await state.snapshot().notice?.contains("未知命令") == true)
    }

    @Test func shiftTabCyclesThemeWithoutChangingDraft() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "shisui"])
        let state = TerminalState(configuration: configuration)
        _ = await state.handle(bytes: Array("草稿".utf8))

        _ = await state.handle(bytes: Array("\u{001B}[Z".utf8))
        let snapshot = await state.snapshot()

        #expect(snapshot.configuration.theme == .chatroom)
        #expect(snapshot.configuration.palette.background == TerminalTheme.chatroom.palette.background)
        #expect(snapshot.draft == "草稿")
    }
    @Test func enterMentionsSelectedDanmuAndRestoresComposer() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.consume(.event(DanmuEvent(
            id: "target",
            kind: .danmu,
            timestamp: .now,
            username: "观众甲",
            content: "这个问题怎么处理？"
        )))
        _ = await state.handle(bytes: Array("先看这里".utf8))
        _ = await state.handle(bytes: Array("\u{001B}[A".utf8))
        let frozenEventIDs = await state.snapshot().events.map(\.id)
        await state.consume(.event(DanmuEvent(
            id: "newer",
            kind: .danmu,
            timestamp: .now,
            username: "观众乙",
            content: "新消息"
        )))
        #expect(await state.snapshot().events.map(\.id) == frozenEventIDs)

        _ = await state.handle(bytes: Array("\r".utf8))
        #expect(await state.snapshot().draft == "先看这里 @观众甲 ")
        #expect(await state.snapshot().selectedEventID == nil)
        #expect(await state.snapshot().events.first?.id == "newer")
        _ = await state.handle(bytes: Array("继续".utf8))
        #expect(await state.snapshot().draft == "先看这里 @观众甲 继续")
    }

    @Test func submittingCanonicalMentionKeepsFullNameAndTargetAuthorID() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let timestamp = Date(timeIntervalSince1970: 1_000)
        await state.consume(.event(DanmuEvent(
            id: "target",
            kind: .danmu,
            timestamp: timestamp,
            username: "观***",
            authorID: "0",
            content: "这个问题怎么处理？"
        )))
        await state.refreshCanonicalUsernames(from: [DanmuEvent(
            id: "target-history",
            kind: .danmu,
            timestamp: timestamp,
            username: "观众甲",
            authorID: "12345",
            content: "这个问题怎么处理？",
            origin: .history
        )])

        _ = await state.handle(bytes: Array("\u{001B}[A".utf8))
        _ = await state.handle(bytes: Array("\r".utf8))
        #expect(await state.snapshot().draft == "@观众甲 ")
        let action = await state.handle(bytes: Array("这个方案可行\r".utf8))

        guard case .send(let message, let replyToAuthorID) = action else {
            Issue.record("Expected a danmu submission")
            return
        }
        #expect(message == "@观众甲 这个方案可行")
        #expect(replyToAuthorID == "12345")
    }

    @Test func escapeCancelsDanmuSelectionAndRestoresComposer() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        await state.consume(.event(DanmuEvent(
            id: "target",
            kind: .danmu,
            timestamp: .now,
            username: "观众甲",
            content: "问题"
        )))
        _ = await state.handle(bytes: Array("\u{001B}[A".utf8))
        let start = ContinuousClock.now
        _ = await state.handle(bytes: Array("\u{001B}".utf8), now: start)
        let action = await state.handle(bytes: [], now: start.advanced(by: .milliseconds(50)))
        if case .none = action {} else { Issue.record("Esc should only cancel danmu selection") }
        #expect(await state.snapshot().selectedEventID == nil)
        _ = await state.handle(bytes: Array("输入".utf8))
        #expect(await state.snapshot().draft == "输入")
    }
    @Test func sustainedArrowRepeatsContinuouslyMoveSelectionAndViewport() async throws {
        var configuration = try TerminalConfiguration.load(arguments: ["--room", "1", "--theme", "pure"])
        configuration.chatLayout = false
        configuration.singleLine = true
        configuration.showTime = false
        let state = TerminalState(configuration: configuration)
        for index in 0..<40 {
            await state.consume(.event(DanmuEvent(
                id: "event-\(index)",
                kind: .danmu,
                timestamp: .now,
                username: "观众\(index)",
                content: "消息\(index)"
            )))
        }
        let renderer = TerminalRenderer()
        let size = TerminalSize(columns: 80, rows: 16)
        var selectedIDs: [String] = []
        var renderedFrames = Set<String>()

        for _ in 0..<25 {
            _ = await state.handle(bytes: Array("\u{001B}[A".utf8))
            let snapshot = await state.snapshot()
            let selectedID = try #require(snapshot.selectedEventID)
            let selectedUsername = try #require(snapshot.events.first { $0.id == selectedID }?.username)
            let frame = renderer.renderInteractive(snapshot, size: size)
            #expect(frame.output.contains(selectedUsername))
            #expect(frame.output.contains("\u{001B}[7m"))
            selectedIDs.append(selectedID)
            renderedFrames.insert(frame.screen.lines.joined(separator: "\n"))
        }

        #expect(selectedIDs.count == 25)
        #expect(Set(selectedIDs).count == 25)
        #expect(renderedFrames.count == 25)
        #expect(await state.snapshot().selectedEventID == "event-15")

        for _ in 0..<10 {
            _ = await state.handle(bytes: Array("\u{001B}[B".utf8))
        }
        #expect(await state.snapshot().selectedEventID == "event-25")
    }

    @Test func noticesExpireWithoutWaitingForAnotherEvent() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let reportedAt = Date.now

        await state.report("临时系统提示")

        #expect(await state.snapshot(now: reportedAt).notice == "临时系统提示")
        #expect(await state.snapshot(now: reportedAt.addingTimeInterval(9)).notice == nil)
    }

    @Test func repeatedOBSErrorDoesNotStayVisibleAndRecoveryClearsIt() async throws {
        let configuration = try TerminalConfiguration.load(arguments: ["--room", "1"])
        let state = TerminalState(configuration: configuration)
        let reportedAt = Date.now
        let error = OBSControlError.commandTimedOut

        await state.reportOBSError(error)
        await state.reportOBSError(error)
        #expect(await state.snapshot(now: reportedAt.addingTimeInterval(9)).notice == nil)

        await state.reportOBSError(error)
        await state.updateOBSStatus(OBSStatus(connection: .connected))
        #expect(await state.snapshot().notice == nil)
    }
}
