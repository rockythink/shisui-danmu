import BilibiliDanmu
import DanmuCore
import Dispatch
import Darwin
import Foundation
import OBSControl

@main
struct ShisuiDanmuTerminal {
    @MainActor
    static func main() {
        let arguments = Array(CommandLine.arguments.dropFirst())
        if arguments.contains("--login-helper") {
            TerminalLoginAppLauncher.recordDiagnostic("helper process entered")
            let accountClient = BilibiliAccountClient(sessionURL: TerminalStoragePaths.accountSessionURL)
            let loginController = TerminalLoginWindowController(accountClient: accountClient)
            TerminalLoginAppLauncher.recordDiagnostic("helper controller starting")
            loginController.run()
            TerminalLoginAppLauncher.recordDiagnostic("helper controller finished")
            return
        }
        Task.detached {
            await run(arguments: arguments)
            Darwin.exit(EXIT_SUCCESS)
        }
        dispatchMain()
    }

    private static func run(arguments: [String]) async {
        if arguments.contains("--help") || arguments.contains("-h") {
            print(TerminalConfiguration.usage)
            return
        }
        if arguments.contains("--version") || arguments.contains("-v") {
            print("danmu 0.3.1")
            return
        }
        if arguments.contains("--configure-obs") {
            await configureOBSInteractively()
            return
        }
        if arguments.contains("--login") {
            do {
                try await TerminalLoginAppLauncher.run()
            } catch {
                FileHandle.standardError.write(Data("打开 B 站授权窗口失败：\(error.localizedDescription)\n".utf8))
            }
            return
        }
        if arguments.contains("--logout") {
            let accountClient = BilibiliAccountClient(sessionURL: TerminalStoragePaths.accountSessionURL)
            do {
                try await accountClient.signOut()
                print("B 站登录态已清除。")
            } catch {
                FileHandle.standardError.write(Data("清除 B 站登录态失败：\(error.localizedDescription)\n".utf8))
            }
            return
        }

        let configuration: TerminalConfiguration
        do {
            configuration = try TerminalConfiguration.load()
        } catch {
            FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8))
            return
        }

        guard isatty(STDIN_FILENO) == 1, isatty(STDOUT_FILENO) == 1 else {
            FileHandle.standardError.write(Data("终端舞台需要在交互式终端中运行。\n".utf8))
            return
        }

        let obsConfiguration: OBSConfiguration
        let obsConfigurationNotice: String?
        do {
            obsConfiguration = try OBSConfigurationStore(fileURL: TerminalStoragePaths.obsConfigurationURL).load()
            obsConfigurationNotice = nil
        } catch {
            obsConfiguration = OBSConfiguration()
            obsConfigurationNotice = "OBS 配置无法读取，已使用安全默认值；原文件未修改：\(error.localizedDescription)"
        }
        if OBSCLIPathResolver.resolve(configuredPath: obsConfiguration.executablePath) == nil {
            await offerOBSCLIInstallation()
        }

        let terminal = TerminalSession()
        guard terminal.activate() else {
            FileHandle.standardError.write(Data("无法切换终端输入模式。\n".utf8))
            return
        }
        defer { terminal.restore() }

        let journal = try? LocalJSONLDanmuSessionJournal(bundleIdentifier: TerminalStoragePaths.namespace)
        let obsKeychain = OBSKeychainPasswordStore(service: TerminalStoragePaths.obsKeychainService)
        let obsController = ObsCLIController(
            configuration: obsConfiguration,
            lockPath: TerminalStoragePaths.obsLockPath
        ) {
            if let temporary = ProcessInfo.processInfo.environment["OBS_API_PASSWORD"], !temporary.isEmpty {
                return temporary
            }
            return try obsKeychain.read(host: obsConfiguration.host, port: obsConfiguration.port)
        }
        let state = TerminalState(
            configuration: configuration,
            journal: journal,
            obsConfiguration: obsConfiguration
        )
        if let obsConfigurationNotice { await state.report(obsConfigurationNotice) }
        let danmuClient = BilibiliDanmuClient()
        let accountClient = BilibiliAccountClient(sessionURL: TerminalStoragePaths.accountSessionURL)
        let renderer = TerminalRenderer()

        terminal.enterApplicationMode()

        let accountTask = Task {
            do {
                switch try await accountClient.status() {
                case .signedIn(let displayName, let userID):
                    await state.updateBroadcasterIdentity(nickname: displayName, authorID: userID)
                case .signedOut:
                    await state.updateBroadcasterIdentity(nickname: nil, authorID: nil)
                }
            } catch {
                await state.updateBroadcasterIdentity(nickname: nil, authorID: nil)
            }
        }

        let connectionTask = Task {
            for await update in danmuClient.connect(roomID: configuration.roomID) {
                await state.consume(update)
            }
        }

        let obsTask = Task {
            var lastSceneRefresh = Date.distantPast
            while !Task.isCancelled {
                do {
                    await state.updateOBSStatus(try await obsController.fetchStatus())
                    if Date.now.timeIntervalSince(lastSceneRefresh) >= 15 {
                        async let scenes = obsController.listScenes()
                        async let inputs = obsController.listInputs()
                        if let resources = try? await (scenes, inputs) {
                            await state.updateOBSScenes(resources.0)
                            await state.updateOBSInputs(resources.1)
                            lastSceneRefresh = .now
                        }
                    }
                    try? await Task.sleep(for: .seconds(2))
                } catch {
                    await state.reportOBSError(error)
                    try? await Task.sleep(for: .seconds(5))
                }
            }
        }

        let roomTask = Task {
            while !Task.isCancelled {
                do {
                    let snapshot = try await danmuClient.roomSnapshot(roomID: configuration.roomID)
                    await state.updateRoom(snapshot)
                } catch {
                    await state.report("房间信息刷新失败：\(error.localizedDescription)")
                }
                try? await Task.sleep(for: .seconds(30))
            }
        }

        // 匿名 WebSocket 会把部分 uid 置为 0、昵称脱敏。只在最近出现这类
        // 弹幕时短暂轮询公开历史，用内容、时间和脱敏模式做一对一校正。
        let usernameRefreshTask = Task {
            while !Task.isCancelled {
                if await state.needsUsernameRefresh() {
                    let history = await danmuClient.recentEvents(roomID: configuration.roomID)
                    await state.refreshCanonicalUsernames(from: history)
                    try? await Task.sleep(for: .seconds(2))
                } else {
                    try? await Task.sleep(for: .seconds(3))
                }
            }
        }

        let inputTask = Task.detached {
            var buffer = [UInt8](repeating: 0, count: 256)
            while !Task.isCancelled {
                let count = Darwin.read(STDIN_FILENO, &buffer, buffer.count)
                if count < 0 {
                    if errno == EINTR { continue }
                    break
                }
                let bytes = count > 0 ? Array(buffer.prefix(count)) : []
                let action = await state.handle(bytes: bytes)
                switch action {
                case .none:
                    break
                case .quit:
                    return
                case .account(let intent):
                    Task {
                        await performAccountIntent(
                            intent,
                            accountClient: accountClient,
                            state: state
                        )
                    }
                case .obs(let intent):
                    Task {
                        await performOBSIntent(
                            intent,
                            controller: obsController,
                            configuration: obsConfiguration,
                            state: state
                        )
                    }
                case .send(let message, let replyToAuthorID):
                    Task {
                        defer { Task { await state.finishSending() } }
                        do {
                            guard case .signedIn(let nickname, let authorID) = try await accountClient.status() else {
                                await state.report("发送失败：还没有 B 站登录态")
                                return
                            }
                            await state.updateBroadcasterIdentity(nickname: nickname, authorID: authorID)
                            let segments = BilibiliDanmuMessageSegmenter.segments(for: message)
                            for segment in segments {
                                await state.beginAwaitingEcho(
                                    message: segment,
                                    broadcasterNickname: nickname,
                                    broadcasterAuthorID: authorID
                                )
                                try await accountClient.sendDanmu(
                                    message: segment,
                                    roomID: configuration.roomID,
                                    replyToAuthorID: replyToAuthorID
                                )
                                await state.markSubmittedUnlessAlreadyConfirmed()

                                let clock = ContinuousClock()
                                let deadline = clock.now.advanced(by: .seconds(15))
                                while !(await state.hasConfirmedEcho()), clock.now < deadline {
                                    try? await Task.sleep(for: .milliseconds(100))
                                }
                                guard await state.hasConfirmedEcho() else {
                                    await state.expireEchoIfNeeded()
                                    return
                                }
                                if await state.takeHistoryIdentityRefreshRequest() {
                                    try? await Task.sleep(for: .milliseconds(350))
                                    let history = await danmuClient.recentEvents(roomID: configuration.roomID)
                                    await state.refreshCanonicalUsernames(from: history)
                                }
                            }
                        } catch {
                            await state.report("发送失败：\(error.localizedDescription)")
                        }
                    }
                }
            }
        }

        var lastRevision: UInt64?
        var lastRenderedSecond: Int?
        var lastSize: TerminalSize?
        var presenter = TerminalFramePresenter()
        while !(await state.quitting()) {
            let viewState = await state.snapshot()
            let second = Int(Date.now.timeIntervalSince1970)
            let size = terminal.size()
            if lastRevision != viewState.revision || lastRenderedSecond != second || lastSize != size {
                let result = renderer.renderInteractive(viewState, size: size)
                if let lastSize, lastSize != size { presenter.reset() }
                terminal.write(presenter.present(result))
                lastRevision = viewState.revision
                lastRenderedSecond = second
                lastSize = size
            }
            try? await Task.sleep(for: .milliseconds(25))
        }

        accountTask.cancel()
        connectionTask.cancel()
        roomTask.cancel()
        obsTask.cancel()
        usernameRefreshTask.cancel()
        inputTask.cancel()
        await danmuClient.disconnect()
        await state.finish()
        terminal.leaveApplicationMode()
    }

    private static func configureOBSInteractively() async {
        let configurationStore = OBSConfigurationStore(fileURL: TerminalStoragePaths.obsConfigurationURL)
        var configuration = (try? configurationStore.load()) ?? OBSConfiguration()
        if OBSCLIPathResolver.resolve(configuredPath: configuration.executablePath) == nil {
            await offerOBSCLIInstallation()
        }

        print("配置 OBS WebSocket（直接回车保留当前值）")
        print("Host [\(configuration.host)]:", terminator: " ")
        if let host = readLine()?.trimmingCharacters(in: .whitespacesAndNewlines), !host.isEmpty {
            configuration.host = host
        }
        print("Port [\(configuration.port)]:", terminator: " ")
        if let value = readLine()?.trimmingCharacters(in: .whitespacesAndNewlines),
           !value.isEmpty, let port = Int(value), (1...65_535).contains(port) {
            configuration.port = port
        }
        print("WebSocket 密码（输入不回显，留空保持 Keychain 现有密码）:", terminator: " ")
        let password = readSecureLine() ?? ""

        do {
            try configurationStore.save(configuration)
            let keychain = OBSKeychainPasswordStore(service: TerminalStoragePaths.obsKeychainService)
            if !password.isEmpty {
                try keychain.save(password, host: configuration.host, port: configuration.port)
            }
            let credentialHost = configuration.host
            let credentialPort = configuration.port
            let controller = ObsCLIController(
                configuration: configuration,
                lockPath: TerminalStoragePaths.obsLockPath
            ) {
                if let temporary = ProcessInfo.processInfo.environment["OBS_API_PASSWORD"], !temporary.isEmpty {
                    return temporary
                }
                return try keychain.read(host: credentialHost, port: credentialPort)
            }
            _ = try await controller.fetchStatus()
            let scenes = try await controller.listScenes()
            let inputs = try await controller.listInputs()

            if !scenes.isEmpty {
                print("\nOBS 现有场景：")
                for (index, scene) in scenes.enumerated() { print("  \(index + 1). \(scene)") }
                print("开播默认场景序号（留空保持当前设置）:", terminator: " ")
                if let value = readLine(), let index = Int(value), scenes.indices.contains(index - 1) {
                    configuration.defaultLiveScene = scenes[index - 1]
                }
            }
            if !inputs.isEmpty {
                print("\nOBS 输入：")
                for (index, input) in inputs.enumerated() { print("  \(index + 1). \(input)") }
                print("麦克风输入序号（留空保持当前设置）:", terminator: " ")
                if let value = readLine(), let index = Int(value), inputs.indices.contains(index - 1) {
                    configuration.microphoneInputName = inputs[index - 1]
                }
            }
            try configurationStore.save(configuration)
            print("\nOBS 配置完成。场景和输入均直接读取自当前 OBS。")
        } catch {
            print("\nOBS 配置或连接失败：\(error.localizedDescription)")
            print("请确认 OBS 已启动、WebSocket v5 已开启且密码正确。")
        }
    }

    private static func readSecureLine() -> String? {
        var original = termios()
        guard tcgetattr(STDIN_FILENO, &original) == 0 else { return readLine() }
        var hidden = original
        hidden.c_lflag &= ~tcflag_t(ECHO)
        guard tcsetattr(STDIN_FILENO, TCSAFLUSH, &hidden) == 0 else { return readLine() }
        defer { tcsetattr(STDIN_FILENO, TCSAFLUSH, &original) }
        let value = readLine()
        print("")
        return value
    }

    private static func offerOBSCLIInstallation() async {
        print("""
        未检测到 obs-cli。未安装时以下功能不可用：
        \(OBSCLIInstaller.unavailableFeaturesDescription)

        是否现在通过 `uv tool install obs-cli` 自动安装？[y/N]
        """, terminator: " ")
        guard let answer = readLine()?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
              ["y", "yes", "是"].contains(answer) else {
            print("已跳过安装。弹幕查看、提问队列和发送功能仍可正常使用。")
            return
        }
        print("正在安装 obs-cli…")
        do {
            try await OBSCLIInstaller.install()
            print("obs-cli 安装完成。")
        } catch {
            print("obs-cli 安装失败：\(error.localizedDescription)")
            print("可以稍后手工运行：uv tool install obs-cli")
        }
    }

    private static func performAccountIntent(
        _ intent: TerminalAccountIntent,
        accountClient: BilibiliAccountClient,
        state: TerminalState
    ) async {
        switch intent {
        case .signIn:
            do {
                try await TerminalLoginAppLauncher.run()
                switch try await accountClient.status() {
                case .signedIn(let nickname, let authorID):
                    await state.updateBroadcasterIdentity(nickname: nickname, authorID: authorID)
                    await state.finishAccountAction(message: "已登录 B 站：\(nickname)")
                case .signedOut:
                    await state.finishAccountAction(message: "尚未完成 B 站登录")
                }
            } catch {
                await state.finishAccountAction(message: "B 站登录失败：\(error.localizedDescription)")
            }
        case .signOut:
            do {
                try await accountClient.signOut()
                await state.updateBroadcasterIdentity(nickname: nil, authorID: nil)
                await state.finishAccountAction(message: "B 站登录态已清除")
            } catch {
                await state.finishAccountAction(message: "清除 B 站登录态失败：\(error.localizedDescription)")
            }
        }
    }

    private static func performOBSIntent(
        _ intent: TerminalOBSIntent,
        controller: ObsCLIController,
        configuration: OBSConfiguration,
        state: TerminalState
    ) async {
        do {
            switch intent {
            case .connect, .refresh:
                break
            case .configurePassword(let password):
                try OBSKeychainPasswordStore(service: TerminalStoragePaths.obsKeychainService).save(
                    password,
                    host: configuration.host,
                    port: configuration.port
                )
            case .configureMicrophone(let input):
                var updatedConfiguration = configuration
                updatedConfiguration.microphoneInputName = input
                try OBSConfigurationStore(fileURL: TerminalStoragePaths.obsConfigurationURL).save(updatedConfiguration)
                await controller.updateConfiguration(updatedConfiguration)
                await state.updateOBSConfiguration(updatedConfiguration)
            case .setMuted(let muted, let input):
                try await controller.setMicrophoneMuted(muted, input: input)
            case .switchScene(let scene):
                try await controller.switchScene(to: scene)
            case .startStreaming:
                try await controller.startStreaming(defaultScene: configuration.defaultLiveScene)
            case .stopStreaming:
                try await controller.stopStreaming()
            }
            let status = try await controller.fetchStatus()
            await state.updateOBSStatus(status)
            async let scenes = controller.listScenes()
            async let inputs = controller.listInputs()
            if let resources = try? await (scenes, inputs) {
                await state.updateOBSScenes(resources.0)
                await state.updateOBSInputs(resources.1)
            }
            let message: String?
            switch intent {
            case .connect: message = "OBS 已连接，场景和输入已同步"
            case .configurePassword: message = "OBS WebSocket 密码已更新并验证"
            case .configureMicrophone(let input): message = "麦克风控制目标已改为“\(input)”"
            case .startStreaming: message = "OBS 已开始推流"
            case .stopStreaming: message = "OBS 推流已停止；弹幕连接保持不变"
            case .switchScene(let scene): message = "已切换到“\(scene)”"
            case .setMuted(let muted, _): message = muted ? "麦克风已静音" : "麦克风已取消静音"
            case .refresh: message = nil
            }
            await state.finishOBSAction(message: message)
        } catch {
            await state.reportOBSError(error)
            await state.finishOBSAction()
        }
    }
}
enum TerminalApplicationModes {
    static let enter = "\u{001B}[?1049h\u{001B}[?1007l\u{001B}[?2004h\u{001B}[?25h\u{001B}[2J"
    static let exit = "\u{001B}[0m\u{001B}[0 q\u{001B}[?7h\u{001B}[?1007h\u{001B}[?2004l\u{001B}[?25h\u{001B}[?1049l"
}

private final class TerminalSession: @unchecked Sendable {
    private var original = termios()
    private var isActive = false
    private let lock = NSLock()
    private var applicationModeActive = false

    func activate() -> Bool {
        guard tcgetattr(STDIN_FILENO, &original) == 0 else { return false }
        var raw = original
        cfmakeraw(&raw)
        raw.c_oflag |= tcflag_t(OPOST)
        withUnsafeMutableBytes(of: &raw.c_cc) { controlCharacters in
            controlCharacters[Int(VMIN)] = 0
            controlCharacters[Int(VTIME)] = 1
        }
        guard tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw) == 0 else { return false }
        isActive = true
        return true
    }

    func enterApplicationMode() {
        guard !applicationModeActive else { return }
        write(TerminalApplicationModes.enter)
        applicationModeActive = true
    }

    func leaveApplicationMode() {
        guard applicationModeActive else { return }
        write(TerminalApplicationModes.exit)
        applicationModeActive = false
    }

    func restore() {
        guard isActive else { return }
        leaveApplicationMode()
        tcsetattr(STDIN_FILENO, TCSAFLUSH, &original)
        isActive = false
    }

    func write(_ value: String) {
        lock.lock()
        defer { lock.unlock() }
        FileHandle.standardOutput.write(Data(value.utf8))
    }

    func size() -> TerminalSize {
        var window = winsize()
        if ioctl(STDOUT_FILENO, TIOCGWINSZ, &window) == 0,
           window.ws_col > 0,
           window.ws_row > 0 {
            return TerminalSize(columns: Int(window.ws_col), rows: Int(window.ws_row))
        }
        return TerminalSize(columns: 120, rows: 36)
    }

    deinit {
        restore()
    }
}
