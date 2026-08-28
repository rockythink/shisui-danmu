import Foundation
import OBSControl
import Testing

@Suite("OBS CLI controller")
struct ObsCLIControllerTests {
    @Test func startsWithConfiguredSceneAndConfirmsLive() async throws {
        let runner = FakeOBSRunner(results: [
            result("stopped"),
            result("旧场景"),
            result(),
            result("直播场景"),
            result(),
            result("started")
        ])
        let controller = makeController(runner: runner)

        try await controller.startStreaming(defaultScene: "直播场景")

        let calls = await runner.calls
        #expect(calls.map(\.arguments) == [
            ["stream", "status"],
            ["scene", "current"],
            ["scene", "switch", "直播场景"],
            ["scene", "current"],
            ["stream", "start"],
            ["stream", "status"]
        ])
        #expect(calls.allSatisfy { $0.environment["OBS_API_PASSWORD"] == "secret" })
    }

    @Test func startIsIdempotentWhenAlreadyLive() async throws {
        let runner = FakeOBSRunner(results: [result("started")])
        let controller = makeController(runner: runner)
        try await controller.startStreaming(defaultScene: "直播场景")
        #expect(await runner.calls.map(\.arguments) == [["stream", "status"]])
    }

    @Test func muteUsesExplicitActionAndConfirms() async throws {
        let runner = FakeOBSRunner(results: [result(), result("enabled")])
        let controller = makeController(runner: runner)
        try await controller.setMicrophoneMuted(true, input: "Mic / 主麦")
        #expect(await runner.calls.map(\.arguments) == [
            ["input", "mute", "Mic / 主麦"],
            ["input", "is-muted", "Mic / 主麦"]
        ])
    }

    @Test func tracebackOnStdoutIsClassifiedWithoutLeakingPassword() async throws {
        let runner = FakeOBSRunner(results: [result("ConnectionFailure password=secret", code: 1)])
        let controller = makeController(runner: runner)
        do {
            try await controller.stopStreaming()
            Issue.record("Expected connection failure")
        } catch {
            #expect(error is OBSControlError)
            #expect(!error.localizedDescription.contains("secret"))
            #expect(error.localizedDescription.contains("OBS"))
        }
    }

    @Test func stopDoesNotTouchRecordingOrDanmu() async throws {
        let runner = FakeOBSRunner(results: [result("started"), result(), result("stopped")])
        let controller = makeController(runner: runner)
        try await controller.stopStreaming()
        let arguments = await runner.calls.map(\.arguments)
        #expect(arguments == [["stream", "status"], ["stream", "stop"], ["stream", "status"]])
    }

    private func makeController(runner: FakeOBSRunner) -> ObsCLIController {
        ObsCLIController(
            configuration: OBSConfiguration(executablePath: "/usr/bin/true", microphoneInputName: "Mic/Aux"),
            runner: runner,
            lockPath: FileManager.default.temporaryDirectory
                .appendingPathComponent(UUID().uuidString).path,
            passwordProvider: { "secret" }
        )
    }

    private func result(_ output: String = "", error: String = "", code: Int32 = 0) -> OBSProcessResult {
        OBSProcessResult(exitCode: code, standardOutput: output, standardError: error)
    }
}

private actor FakeOBSRunner: OBSProcessRunning {
    struct Call: Sendable {
        let arguments: [String]
        let environment: [String: String]
    }

    private var results: [OBSProcessResult]
    private(set) var calls: [Call] = []

    init(results: [OBSProcessResult]) { self.results = results }

    func run(
        executableURL: URL,
        arguments: [String],
        environment: [String: String],
        timeout: Duration
    ) async throws -> OBSProcessResult {
        calls.append(Call(arguments: arguments, environment: environment))
        guard !results.isEmpty else {
            return OBSProcessResult(exitCode: 1, standardOutput: "", standardError: "missing fake result")
        }
        return results.removeFirst()
    }
}
