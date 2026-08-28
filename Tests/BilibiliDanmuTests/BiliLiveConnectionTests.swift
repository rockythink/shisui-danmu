import DanmuCore
import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili live connection", .serialized)
struct BiliLiveConnectionTests {
    @Test func initialHistoryArrivesBeforeTheConnectionBecomesSendable() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [BiliLiveConnectionURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let socket = RecordingWebSocket(messages: [
            .data(BiliPacketCodec.encode(operation: 8, bodyData: Data(#"{"code":0}"#.utf8))),
        ])
        let connection = BiliLiveConnection(
            session: session,
            webSocketFactory: { _ in socket },
            heartbeatSleeper: { try await Task.sleep(for: .seconds(60)) },
            deviceIdentityFetcher: { nil }
        )
        BiliLiveConnectionURLProtocol.requestHandler = { request in
            if request.url?.path == "/xlive/web-room/v1/dM/gethistory" {
                let body = #"{"code":0,"data":{"room":[{"text":"历史弹幕","uid":7604237,"nickname":"停车拾穗","timeline":"2026-08-15 00:00:00","id_str":"history-1"}]}}"#
                let response = HTTPURLResponse(
                    url: try #require(request.url),
                    statusCode: 200,
                    httpVersion: nil,
                    headerFields: ["Content-Type": "application/json"]
                )!
                return (response, Data(body.utf8))
            }
            return try Self.response(for: request)
        }
        defer { BiliLiveConnectionURLProtocol.requestHandler = nil }

        var updates: [BiliTransportUpdate] = []
        for try await update in connection.connect(roomID: "392612") {
            updates.append(update)
            if updates.count == 2 { break }
        }

        guard case .event(let historyEvent) = try #require(updates.first) else {
            Issue.record("连接可发送前应先交付初始历史")
            return
        }
        #expect(historyEvent.content == "历史弹幕")
        #expect(updates[1] == .connected(resolvedRoomID: "392612"))
    }

    @Test func heartbeatFailureCancelsSocketAndEndsStream() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [BiliLiveConnectionURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let socket = FailingHeartbeatWebSocket()
        let connection = BiliLiveConnection(
            session: session,
            webSocketFactory: { _ in socket },
            heartbeatSleeper: {},
            deviceIdentityFetcher: { nil }
        )
        BiliLiveConnectionURLProtocol.requestHandler = Self.response(for:)
        defer { BiliLiveConnectionURLProtocol.requestHandler = nil }

        let completion = ConnectionCompletionRecorder()
        let updates = connection.connect(roomID: "392612")
        let collector = Task {
            do {
                for try await _ in updates {}
                await completion.recordFinished()
            } catch {
                await completion.recordFailed()
            }
        }

        try await Task.sleep(for: .milliseconds(100))

        #expect(socket.cancellationCount == 1)
        #expect(await completion.didEnd)
        collector.cancel()
        _ = await collector.result
    }

    @Test func modernAnonymousContextIsUsedAndAckRequestsAreAnswered() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [BiliLiveConnectionURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let socket = RecordingWebSocket(messages: [
            .data(BiliPacketCodec.encode(operation: 8, bodyData: Data(#"{"code":0}"#.utf8))),
            .data(try BiliPacketCodec.encode(
                operation: 5,
                body: #"{"cmd":"DANMU_MSG","msg_id":"message-2","p_is_ack":true,"p_msg_type":1,"info":[[],"现代鉴权弹幕",[404,"其他观众"],[],[]]}"#
            )),
        ])
        let requestRecorder = WebSocketRequestRecorder()
        let identity = BiliDeviceIdentity(buvid3: "test-buvid-3", buvid4: "test-buvid-4")
        let connection = BiliLiveConnection(
            session: session,
            webSocketFactory: { request in
                requestRecorder.record(request)
                return socket
            },
            heartbeatSleeper: { try await Task.sleep(for: .seconds(60)) },
            deviceIdentityFetcher: { identity }
        )
        BiliLiveConnectionURLProtocol.requestHandler = Self.response(for:)
        defer { BiliLiveConnectionURLProtocol.requestHandler = nil }

        var receivedEvent: DanmuEvent?
        for try await update in connection.connect(roomID: "392612") {
            if case .event(let event) = update {
                receivedEvent = event
                break
            }
        }

        let webSocketRequest = try #require(requestRecorder.request)
        #expect(webSocketRequest.value(forHTTPHeaderField: "Cookie") == "buvid3=test-buvid-3; buvid4=test-buvid-4")

        let authBody = try #require(socket.sentBody(for: 7))
        let auth = try #require(JSONSerialization.jsonObject(with: authBody) as? [String: Any])
        #expect(auth["buvid"] as? String == "test-buvid-3")
        #expect(auth["support_ack"] as? Bool == true)
        #expect((auth["queue_uuid"] as? String)?.isEmpty == false)
        #expect(auth["scene"] as? String == "")

        let acknowledgementBody = try #require(socket.sentBody(for: 24))
        let acknowledgement = try #require(
            JSONSerialization.jsonObject(with: acknowledgementBody) as? [String: Any]
        )
        #expect(acknowledgement["msg_id"] as? String == "message-2")
        #expect(receivedEvent?.username == "其他观众")
        #expect(receivedEvent?.content == "现代鉴权弹幕")
    }

    private static func response(for request: URLRequest) throws -> (HTTPURLResponse, Data) {
        let path = try #require(request.url?.path)
        let body: String
        switch path {
        case "/room/v1/Room/room_init":
            body = #"{"code":0,"data":{"room_id":392612}}"#
        case "/room/v1/Danmu/getConf":
            body = #"{"code":0,"data":{"token":"test-token","host":"broadcast.example","host_server_list":[{"host":"broadcast.example","wss_port":443}]}}"#
        case "/xlive/web-room/v1/dM/gethistory":
            body = #"{"code":0,"data":{"room":[]}}"#
        default:
            Issue.record("Unexpected request: \(request.url?.absoluteString ?? path)")
            body = #"{"code":-1}"#
        }
        let response = HTTPURLResponse(
            url: try #require(request.url),
            statusCode: 200,
            httpVersion: nil,
            headerFields: ["Content-Type": "application/json"]
        )!
        return (response, Data(body.utf8))
    }
}

private final class WebSocketRequestRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var storedRequest: URLRequest?

    var request: URLRequest? {
        lock.withLock { storedRequest }
    }

    func record(_ request: URLRequest) {
        lock.withLock { storedRequest = request }
    }
}

private final class RecordingWebSocket: BiliWebSocketTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var messages: [URLSessionWebSocketTask.Message]
    private var sentPackets: [Data] = []
    private var isCancelled = false
    private var pendingReceive: CheckedContinuation<URLSessionWebSocketTask.Message, any Error>?

    init(messages: [URLSessionWebSocketTask.Message]) {
        self.messages = messages
    }

    func resume() {}

    func cancel(with closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        let continuation = lock.withLock {
            isCancelled = true
            defer { pendingReceive = nil }
            return pendingReceive
        }
        continuation?.resume(throwing: URLError(.cancelled))
    }

    func send(_ message: URLSessionWebSocketTask.Message) async throws {
        guard case .data(let data) = message else { return }
        lock.withLock { sentPackets.append(data) }
    }

    func receive() async throws -> URLSessionWebSocketTask.Message {
        let immediateResult: Result<URLSessionWebSocketTask.Message, any Error>? = lock.withLock {
            if isCancelled { return .failure(URLError(.cancelled)) }
            if !messages.isEmpty { return .success(messages.removeFirst()) }
            return nil
        }
        if let immediateResult {
            return try immediateResult.get()
        }

        return try await withCheckedThrowingContinuation { continuation in
            let shouldFail = lock.withLock {
                if isCancelled { return true }
                pendingReceive = continuation
                return false
            }
            if shouldFail {
                continuation.resume(throwing: URLError(.cancelled))
            }
        }
    }

    func sentBody(for operation: UInt32) -> Data? {
        lock.withLock {
            sentPackets.first(where: { Self.operation(in: $0) == operation }).map {
                $0.count >= 16 ? Data($0.dropFirst(16)) : Data()
            }
        }
    }

    private static func operation(in data: Data) -> UInt32? {
        guard data.count >= 12 else { return nil }
        return (UInt32(data[8]) << 24)
            | (UInt32(data[9]) << 16)
            | (UInt32(data[10]) << 8)
            | UInt32(data[11])
    }
}

private final class FailingHeartbeatWebSocket: BiliWebSocketTransport, @unchecked Sendable {
    private let lock = NSLock()
    private let authReply = URLSessionWebSocketTask.Message.data(
        BiliPacketCodec.encode(operation: 8, bodyData: Data(#"{"code":0}"#.utf8))
    )
    private var didReturnAuthReply = false
    private var isCancelled = false
    private var pendingReceive: CheckedContinuation<URLSessionWebSocketTask.Message, any Error>?
    private var storedCancellationCount = 0

    var cancellationCount: Int {
        lock.withLock { storedCancellationCount }
    }

    func resume() {}

    func cancel(with closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        let continuation = lock.withLock {
            storedCancellationCount += 1
            isCancelled = true
            defer { pendingReceive = nil }
            return pendingReceive
        }
        continuation?.resume(throwing: URLError(.cancelled))
    }

    func send(_ message: URLSessionWebSocketTask.Message) async throws {
        if operation(in: message) == 2 {
            throw HeartbeatSocketError.sendFailed
        }
    }

    func receive() async throws -> URLSessionWebSocketTask.Message {
        let immediateResult: Result<URLSessionWebSocketTask.Message, any Error>? = lock.withLock {
            if isCancelled {
                return .failure(URLError(.cancelled))
            }
            if !didReturnAuthReply {
                didReturnAuthReply = true
                return .success(authReply)
            }
            return nil
        }
        if let immediateResult {
            return try immediateResult.get()
        }

        return try await withCheckedThrowingContinuation { continuation in
            let shouldFail = lock.withLock {
                if isCancelled { return true }
                pendingReceive = continuation
                return false
            }
            if shouldFail {
                continuation.resume(throwing: URLError(.cancelled))
            }
        }
    }

    private func operation(in message: URLSessionWebSocketTask.Message) -> UInt32? {
        guard case .data(let data) = message, data.count >= 12 else { return nil }
        return (UInt32(data[8]) << 24)
            | (UInt32(data[9]) << 16)
            | (UInt32(data[10]) << 8)
            | UInt32(data[11])
    }
}

private actor ConnectionCompletionRecorder {
    private(set) var didEnd = false

    func recordFinished() {
        didEnd = true
    }

    func recordFailed() {
        didEnd = true
    }
}

private enum HeartbeatSocketError: Error {
    case sendFailed
}

private final class BiliLiveConnectionURLProtocol: URLProtocol, @unchecked Sendable {
    nonisolated(unsafe) static var requestHandler: ((URLRequest) throws -> (HTTPURLResponse, Data))?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let requestHandler = Self.requestHandler else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        do {
            let (response, data) = try requestHandler(request)
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    override func stopLoading() {}
}
