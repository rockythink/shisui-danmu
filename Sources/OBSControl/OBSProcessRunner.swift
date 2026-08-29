import Foundation

public struct OBSProcessResult: Equatable, Sendable {
    public let exitCode: Int32
    public let standardOutput: String
    public let standardError: String

    public init(exitCode: Int32, standardOutput: String, standardError: String) {
        self.exitCode = exitCode
        self.standardOutput = standardOutput
        self.standardError = standardError
    }
}

public protocol OBSProcessRunning: Sendable {
    func run(
        executableURL: URL,
        arguments: [String],
        environment: [String: String],
        timeout: Duration
    ) async throws -> OBSProcessResult
}

public actor FoundationOBSProcessRunner: OBSProcessRunning {
    private let outputLimit = 256 * 1_024

    public init() {}

    public func run(
        executableURL: URL,
        arguments: [String],
        environment: [String: String],
        timeout: Duration = .seconds(5)
    ) async throws -> OBSProcessResult {
        let process = Process()
        let stdout = Pipe()
        let stderr = Pipe()
        process.executableURL = executableURL
        process.arguments = arguments
        process.environment = ProcessInfo.processInfo.environment.merging(environment) { _, new in new }
        // App processes launched by Finder or LLDB do not have a reliable stdin.
        // Never let an external OBS command inherit those process descriptors.
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = stdout
        process.standardError = stderr

        do {
            try process.run()
        } catch {
            throw OBSControlError.commandFailed(exitCode: -1, detail: error.localizedDescription)
        }

        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: timeout)
        while process.isRunning {
            if Task.isCancelled {
                process.terminate()
                process.waitUntilExit()
                throw CancellationError()
            }
            if clock.now >= deadline {
                process.terminate()
                process.waitUntilExit()
                throw OBSControlError.commandTimedOut
            }
            try await Task.sleep(for: .milliseconds(25))
        }

        let outputData = stdout.fileHandleForReading.readDataToEndOfFile()
        let errorData = stderr.fileHandleForReading.readDataToEndOfFile()
        return OBSProcessResult(
            exitCode: process.terminationStatus,
            standardOutput: decode(outputData),
            standardError: decode(errorData)
        )
    }

    private func decode(_ data: Data) -> String {
        let bounded = data.count > outputLimit ? data.prefix(outputLimit) : data[...]
        return String(decoding: bounded, as: UTF8.self)
    }
}

public enum OBSCLIPathResolver {
    public static func resolve(
        configuredPath: String?,
        environment: [String: String] = ProcessInfo.processInfo.environment,
        fileManager: FileManager = .default
    ) -> URL? {
        var candidates: [String] = []
        if let configuredPath, !configuredPath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            candidates.append(NSString(string: configuredPath).expandingTildeInPath)
        }
        candidates += [
            NSString(string: "~/.local/bin/obs-cli").expandingTildeInPath,
            "/opt/homebrew/bin/obs-cli",
            "/usr/local/bin/obs-cli"
        ]
        if let path = environment["PATH"] {
            candidates += path.split(separator: ":").map { "\($0)/obs-cli" }
        }
        for candidate in candidates where fileManager.isExecutableFile(atPath: candidate) {
            return URL(fileURLWithPath: candidate)
        }
        return nil
    }
}
