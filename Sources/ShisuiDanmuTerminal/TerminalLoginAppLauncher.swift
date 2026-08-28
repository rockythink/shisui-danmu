import Darwin
import Foundation

@MainActor
enum TerminalLoginAppLauncher {
    enum LauncherError: LocalizedError {
        case executableUnavailable
        case helperExited(Int32)

        var errorDescription: String? {
            switch self {
            case .executableUnavailable:
                "无法定位当前 danmu 可执行文件。"
            case .helperExited(let status):
                "B 站登录窗口异常退出（状态码 \(status)）。"
            }
        }
    }

    static func run() async throws {
        let diagnosticLog = TerminalLoginDiagnosticLog()
        diagnosticLog.write("launcher requested")

        guard let sourceExecutable = executableURL else {
            diagnosticLog.write("launcher failed: executable unavailable")
            throw LauncherError.executableUnavailable
        }
        diagnosticLog.write("resolved executable: \(sourceExecutable.path)")

        let process = Process()
        process.executableURL = sourceExecutable
        process.arguments = ["--login-helper"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = diagnosticLog.fileHandle ?? FileHandle.nullDevice
        process.standardError = diagnosticLog.fileHandle ?? FileHandle.nullDevice

        try await withCheckedThrowingContinuation { continuation in
            process.terminationHandler = { process in
                diagnosticLog.write("helper terminated: status=\(process.terminationStatus)")
                if process.terminationStatus == EXIT_SUCCESS {
                    continuation.resume()
                } else {
                    continuation.resume(
                        throwing: LauncherError.helperExited(process.terminationStatus)
                    )
                }
            }
            do {
                try process.run()
                diagnosticLog.write("helper started: pid=\(process.processIdentifier)")
            } catch {
                diagnosticLog.write("helper launch failed: \(error.localizedDescription)")
                continuation.resume(throwing: error)
            }
        }
    }

    static func recordDiagnostic(_ event: String) {
        TerminalLoginDiagnosticLog().write(event)
    }

    private static var executableURL: URL? {
        var requiredSize: UInt32 = 0
        guard _NSGetExecutablePath(nil, &requiredSize) == -1, requiredSize > 0 else {
            return nil
        }

        var pathBuffer = [CChar](repeating: 0, count: Int(requiredSize))
        let result = pathBuffer.withUnsafeMutableBufferPointer { buffer in
            _NSGetExecutablePath(buffer.baseAddress, &requiredSize)
        }
        guard result == 0 else { return nil }

        let executablePath = String(
            decoding: pathBuffer.lazy.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) },
            as: UTF8.self
        )
        let resolvedURL = URL(fileURLWithPath: executablePath).resolvingSymlinksInPath()
        guard FileManager.default.isExecutableFile(atPath: resolvedURL.path) else {
            return nil
        }
        return resolvedURL
    }
}

private final class TerminalLoginDiagnosticLog: @unchecked Sendable {
    let fileHandle: FileHandle?
    private let lock = NSLock()

    init() {
        let fileManager = FileManager.default
        let logURL = TerminalStoragePaths.loginDiagnosticLogURL
        try? fileManager.createDirectory(
            at: logURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let descriptor = Darwin.open(logURL.path, O_WRONLY | O_CREAT | O_APPEND, 0o600)
        fileHandle = descriptor >= 0
            ? FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
            : nil
    }

    func write(_ event: String) {
        guard let fileHandle else { return }
        let line = "[\(Date.now.ISO8601Format())] \(event)\n"
        lock.lock()
        defer { lock.unlock() }
        try? fileHandle.write(contentsOf: Data(line.utf8))
    }
}
