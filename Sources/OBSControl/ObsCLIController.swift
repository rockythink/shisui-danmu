import Foundation

public actor ObsCLIController: OBSControlling {
    public typealias PasswordProvider = @Sendable () async throws -> String?

    private var configuration: OBSConfiguration
    private let runner: any OBSProcessRunning
    private let passwordProvider: PasswordProvider
    private let lockPath: String
    private var cachedCompatibilityWarning: String?
    private var didCheckCompatibility = false

    public init(
        configuration: OBSConfiguration,
        runner: any OBSProcessRunning = FoundationOBSProcessRunner(),
        lockPath: String,
        passwordProvider: @escaping PasswordProvider = { ProcessInfo.processInfo.environment["OBS_API_PASSWORD"] }
    ) {
        self.configuration = configuration
        self.runner = runner
        self.passwordProvider = passwordProvider
        self.lockPath = lockPath
    }

    public func updateConfiguration(_ configuration: OBSConfiguration) {
        self.configuration = configuration
    }

    public func fetchStatus() async throws -> OBSStatus {
        let versionWarning = try await compatibilityWarning()
        _ = try await command(["--json", "info"])
        let scene = try await currentScene()
        let stream = try await streamState()
        let microphone: OBSMicrophoneState
        let input = configuration.microphoneInputName.trimmingCharacters(in: .whitespacesAndNewlines)
        if input.isEmpty {
            microphone = .unknown
        } else {
            do {
                microphone = try await microphoneState(input: input)
            } catch {
                microphone = .unknown
            }
        }
        return OBSStatus(
            connection: .connected,
            currentScene: scene,
            stream: stream,
            microphone: microphone,
            compatibilityWarning: versionWarning,
            refreshedAt: .now
        )
    }

    public func listScenes() async throws -> [String] {
        let result = try await command(["scene", "list", "--json"])
        guard let data = sanitizedJSONData(from: result.standardOutput),
              let value = try? JSONSerialization.jsonObject(with: data) else {
            throw OBSControlError.invalidResponse("场景列表不是有效 JSON")
        }
        let rows: [[String: Any]]
        if let array = value as? [[String: Any]] {
            rows = array
        } else if let dictionary = value as? [String: Any],
                  let scenes = dictionary["scenes"] as? [[String: Any]] {
            rows = scenes
        } else {
            throw OBSControlError.invalidResponse("场景列表结构未知")
        }
        return rows.compactMap { row in
            (row["sceneName"] ?? row["name"]) as? String
        }
    }

    public func listInputs() async throws -> [String] {
        let result = try await command(["input", "list", "--json"])
        guard let data = sanitizedJSONData(from: result.standardOutput),
              let value = try? JSONSerialization.jsonObject(with: data),
              let rows = value as? [[String: Any]] else {
            throw OBSControlError.invalidResponse("输入列表不是有效 JSON")
        }
        return rows.compactMap { row in
            (row["inputName"] ?? row["name"]) as? String
        }
    }

    public func switchScene(to name: String) async throws {
        let target = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty else { throw OBSControlError.configurationMissing("场景名称") }
        try await withMutationLock {
            do {
                _ = try await self.command(["scene", "switch", target])
            } catch OBSControlError.sceneNotFound {
                throw OBSControlError.sceneNotFound(target)
            }
            guard try await self.currentScene() == target else {
                throw OBSControlError.confirmationFailed("无法确认 OBS 已切换到“\(target)”。")
            }
        }
    }

    public func setMicrophoneMuted(_ muted: Bool, input: String) async throws {
        let target = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty else { throw OBSControlError.configurationMissing("麦克风输入") }
        try await withMutationLock {
            do {
                _ = try await self.command(["input", muted ? "mute" : "unmute", target])
            } catch OBSControlError.inputNotFound {
                throw OBSControlError.inputNotFound(target)
            }
            let actual = try await self.microphoneState(input: target)
            guard actual == (muted ? .muted : .unmuted) else {
                throw OBSControlError.confirmationFailed("无法确认麦克风状态，请到 OBS 核对。")
            }
        }
    }

    public func startStreaming(defaultScene: String?) async throws {
        try await withMutationLock {
            if try await self.streamState() == .live { return }
            let scene = defaultScene?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            if !scene.isEmpty, try await self.currentScene() != scene {
                _ = try await self.command(["scene", "switch", scene])
                guard try await self.currentScene() == scene else {
                    throw OBSControlError.confirmationFailed("无法确认默认直播场景，已取消开播。")
                }
            }
            _ = try await self.command(["stream", "start"])
            guard try await self.confirmStreamState(.live) else {
                throw OBSControlError.confirmationFailed("无法确认是否已经开播，请立即到 OBS 核对。")
            }
        }
    }

    public func stopStreaming() async throws {
        try await withMutationLock {
            if try await self.streamState() == .stopped { return }
            _ = try await self.command(["stream", "stop"])
            guard try await self.confirmStreamState(.stopped) else {
                throw OBSControlError.confirmationFailed("无法确认是否已经停播，请立即到 OBS 核对。")
            }
        }
    }

    private func currentScene() async throws -> String {
        let result = try await command(["scene", "current"])
        let value = clean(result.standardOutput)
        guard !value.isEmpty else { throw OBSControlError.invalidResponse("当前场景为空") }
        return value
    }

    private func streamState() async throws -> OBSStreamState {
        let result = try await command(["stream", "status"])
        switch clean(result.standardOutput).lowercased() {
        case "started": return .live
        case "stopped": return .stopped
        default: throw OBSControlError.invalidResponse("未知推流状态")
        }
    }

    private func microphoneState(input: String) async throws -> OBSMicrophoneState {
        let result = try await command(["input", "is-muted", input])
        switch clean(result.standardOutput).lowercased() {
        case "enabled": return .muted
        case "disabled": return .unmuted
        default: throw OBSControlError.invalidResponse("未知麦克风状态")
        }
    }

    private func confirmStreamState(_ expected: OBSStreamState) async throws -> Bool {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(5))
        repeat {
            if try await streamState() == expected { return true }
            try await Task.sleep(for: .milliseconds(350))
        } while clock.now < deadline
        return false
    }

    private func compatibilityWarning() async throws -> String? {
        if didCheckCompatibility { return cachedCompatibilityWarning }
        let result = try await command(["--version"], checksExitCodeOnly: true)
        let version = clean(result.standardOutput)
        let warning: String?
        if version.range(of: #"(^|\s)0\.9\."#, options: .regularExpression) == nil {
            warning = "obs-cli \(version) 未经兼容性验证，建议使用 0.9.x。"
        } else {
            warning = nil
        }
        cachedCompatibilityWarning = warning
        didCheckCompatibility = true
        return warning
    }

    private func command(
        _ arguments: [String],
        checksExitCodeOnly: Bool = false
    ) async throws -> OBSProcessResult {
        guard let executable = OBSCLIPathResolver.resolve(configuredPath: configuration.executablePath) else {
            throw OBSControlError.cliNotInstalled
        }
        let password = try await passwordProvider()
        var environment = [
            "OBS_API_HOST": configuration.host,
            "OBS_API_PORT": String(configuration.port)
        ]
        if let password, !password.isEmpty { environment["OBS_API_PASSWORD"] = password }
        let result = try await runner.run(
            executableURL: executable,
            arguments: arguments,
            environment: environment,
            timeout: .seconds(5)
        )
        guard result.exitCode == 0 else {
            throw classifyFailure(result, password: password)
        }
        if checksExitCodeOnly { return result }
        return result
    }

    private func classifyFailure(_ result: OBSProcessResult, password: String?) -> OBSControlError {
        // obs-cli renders unexpected Rich tracebacks to stdout, while its own
        // validation errors use stderr. Inspect both, but never surface the raw
        // traceback or child-process locals to the user.
        var diagnostic = clean(result.standardError + "\n" + result.standardOutput)
        if let password, !password.isEmpty {
            diagnostic = diagnostic.replacingOccurrences(of: password, with: "••••")
        }
        let lower = diagnostic.lowercased()
        if lower.contains("authentication") || lower.contains("identified failed") || lower.contains("authenticationfailure") {
            return .authenticationFailed
        }
        if lower.contains("connection refused") || lower.contains("failed to connect") || lower.contains("connectionfailure") {
            return .obsUnavailable("请确认 OBS 已启动并启用 WebSocket（\(configuration.host):\(configuration.port)）。")
        }
        if lower.contains("scene") && lower.contains("not found") {
            return .sceneNotFound("")
        }
        if lower.contains("input") && lower.contains("not found") {
            return .inputNotFound(configuration.microphoneInputName)
        }
        return .commandFailed(exitCode: result.exitCode, detail: "obs-cli 返回退出码 \(result.exitCode)")
    }

    private func withMutationLock<T: Sendable>(_ operation: () async throws -> T) async throws -> T {
        let directory = URL(fileURLWithPath: lockPath).deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        guard let lock = NSDistributedLock(path: lockPath) else {
            throw OBSControlError.busy
        }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(2))
        while !lock.try() {
            if clock.now >= deadline { throw OBSControlError.busy }
            try await Task.sleep(for: .milliseconds(50))
        }
        defer { lock.unlock() }
        return try await operation()
    }

    private func clean(_ value: String) -> String {
        value
            .replacingOccurrences(of: #"\u001B\[[0-9;?]*[ -/]*[@-~]"#, with: "", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func sanitizedJSONData(from output: String) -> Data? {
        let value = clean(output)
        guard let start = value.firstIndex(where: { $0 == "[" || $0 == "{" }),
              let end = value.lastIndex(where: { $0 == "]" || $0 == "}" }),
              start <= end else { return nil }
        return String(value[start...end]).data(using: .utf8)
    }

}
