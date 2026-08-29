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

    @Test func transientCommandFailureRetriesBeforeSurfacingAnError() async throws {
        let runner = FakeOBSRunner(results: [
            result("temporary websocket close", code: 1),
            result("started")
        ])
        let controller = makeController(runner: runner)

        try await controller.startStreaming(defaultScene: nil)

        #expect(await runner.calls.map(\.arguments) == [
            ["stream", "status"],
            ["stream", "status"]
        ])
    }

    @Test func finalFailurePreservesSanitizedDiagnosticCause() async throws {
        let failure = result("RuntimeError: websocket closed unexpectedly password=secret", code: 7)
        let runner = FakeOBSRunner(results: [failure, failure, failure])
        let controller = makeController(runner: runner)

        do {
            try await controller.startStreaming(defaultScene: nil)
            Issue.record("Expected command failure")
        } catch {
            #expect(error.localizedDescription.contains("websocket closed unexpectedly"))
            #expect(!error.localizedDescription.contains("secret"))
            #expect(await runner.calls.count == 3)
        }
    }

    @Test func concurrentQueriesRunOnlyOneOBSProcessAtATime() async throws {
        let payload = #"[{"sceneName":"直播场景","inputName":"Mic/Aux"}]"#
        let runner = FakeOBSRunner(
            results: [result(payload), result(payload)],
            delay: .milliseconds(100)
        )
        let controller = makeController(runner: runner)

        async let scenes = controller.listScenes()
        async let inputs = controller.listInputs()
        _ = try await (scenes, inputs)

        #expect(await runner.maximumConcurrentRuns == 1)
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

@Suite("OBS process runner")
struct FoundationOBSProcessRunnerTests {
    @Test func commandReceivesClosedStandardInputAndCapturedOutput() async throws {
        let result = try await FoundationOBSProcessRunner().run(
            executableURL: URL(fileURLWithPath: "/bin/sh"),
            arguments: [
                "-c",
                "if read -r _; then exit 9; fi; printf output; printf diagnostic >&2"
            ],
            environment: [:],
            timeout: .seconds(1)
        )

        #expect(result == OBSProcessResult(
            exitCode: 0,
            standardOutput: "output",
            standardError: "diagnostic"
        ))
    }
}

private actor FakeOBSRunner: OBSProcessRunning {
    struct Call: Sendable {
        let arguments: [String]
        let environment: [String: String]
    }

    private var results: [OBSProcessResult]
    private let delay: Duration
    private var concurrentRuns = 0
    private(set) var maximumConcurrentRuns = 0
    private(set) var calls: [Call] = []

    init(results: [OBSProcessResult], delay: Duration = .zero) {
        self.results = results
        self.delay = delay
    }

    func run(
        executableURL: URL,
        arguments: [String],
        environment: [String: String],
        timeout: Duration
    ) async throws -> OBSProcessResult {
        calls.append(Call(arguments: arguments, environment: environment))
        concurrentRuns += 1
        maximumConcurrentRuns = max(maximumConcurrentRuns, concurrentRuns)
        defer { concurrentRuns -= 1 }
        if delay > .zero { try await Task.sleep(for: delay) }
        guard !results.isEmpty else {
            return OBSProcessResult(exitCode: 1, standardOutput: "", standardError: "missing fake result")
        }
        return results.removeFirst()
    }
}
