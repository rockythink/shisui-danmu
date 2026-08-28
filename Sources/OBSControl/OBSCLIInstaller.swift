import Foundation

public enum OBSCLIInstaller {
    public static func resolveUV(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        fileManager: FileManager = .default
    ) -> URL? {
        var candidates = [
            NSString(string: "~/.local/bin/uv").expandingTildeInPath,
            "/opt/homebrew/bin/uv",
            "/usr/local/bin/uv"
        ]
        if let path = environment["PATH"] {
            candidates += path.split(separator: ":").map { "\($0)/uv" }
        }
        return candidates.first(where: fileManager.isExecutableFile(atPath:)).map(URL.init(fileURLWithPath:))
    }

    public static func install(
        runner: any OBSProcessRunning = FoundationOBSProcessRunner()
    ) async throws {
        guard let uv = resolveUV() else {
            throw OBSControlError.configurationMissing("uv。请先安装 uv，再运行 uv tool install obs-cli")
        }
        let result = try await runner.run(
            executableURL: uv,
            arguments: ["tool", "install", "obs-cli"],
            environment: [:],
            timeout: .seconds(120)
        )
        guard result.exitCode == 0 else {
            let detail = result.standardError.trimmingCharacters(in: .whitespacesAndNewlines)
            throw OBSControlError.commandFailed(
                exitCode: result.exitCode,
                detail: detail.isEmpty ? "uv 安装 obs-cli 失败" : detail
            )
        }
        guard OBSCLIPathResolver.resolve(configuredPath: nil) != nil else {
            throw OBSControlError.confirmationFailed("安装命令已结束，但仍未找到 obs-cli。请检查 uv tool 的 bin 目录。")
        }
    }

    public static let unavailableFeatures = [
        "OBS 连接与推流状态检查",
        "当前场景、场景列表和预设场景切换",
        "麦克风静音状态、静音与取消静音",
        "一键开始直播和二次确认后结束直播"
    ]

    public static var unavailableFeaturesDescription: String {
        unavailableFeatures.map { "• \($0)" }.joined(separator: "\n")
    }
}
