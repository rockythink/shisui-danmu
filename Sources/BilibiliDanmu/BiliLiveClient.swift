import Foundation

protocol BiliWebSocketTransport: AnyObject, Sendable {
    func resume()
    func cancel(with closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?)
    func send(_ message: URLSessionWebSocketTask.Message) async throws
    func receive() async throws -> URLSessionWebSocketTask.Message
}

extension URLSessionWebSocketTask: BiliWebSocketTransport {}

typealias BiliWebSocketFactory = @Sendable (URLRequest) -> any BiliWebSocketTransport
typealias BiliHeartbeatSleeper = @Sendable () async throws -> Void

actor BiliLiveConnection {
    enum ClientError: LocalizedError {
        case invalidRoomID
        case invalidResponse(String)
        case disconnected

        var errorDescription: String? {
            switch self {
            case .invalidRoomID:
                "房间号不合法"
            case .invalidResponse(let message):
                message
            case .disconnected:
                "弹幕连接已断开"
            }
        }
    }

    private let session: URLSession
    private let webSocketFactory: BiliWebSocketFactory
    private let heartbeatSleeper: BiliHeartbeatSleeper
    private let deviceIdentityFetcher: BiliDeviceIdentityFetcher
    private var webSocketTask: (any BiliWebSocketTransport)?
    private var heartbeatTask: Task<Void, Never>?

    init(
        session: URLSession? = nil,
        webSocketFactory: BiliWebSocketFactory? = nil,
        heartbeatSleeper: @escaping BiliHeartbeatSleeper = {
            try await Task.sleep(nanoseconds: 30_000_000_000)
        },
        deviceIdentityFetcher: @escaping BiliDeviceIdentityFetcher = {
            await BiliDeviceIdentityProvider.shared.identity()
        }
    ) {
        let resolvedSession: URLSession
        if let session {
            resolvedSession = session
        } else {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.timeoutIntervalForRequest = 15
            configuration.timeoutIntervalForResource = 24 * 60 * 60
            configuration.waitsForConnectivity = true
            configuration.httpCookieStorage = nil
            configuration.httpShouldSetCookies = false
            configuration.urlCredentialStorage = nil
            resolvedSession = URLSession(configuration: configuration)
        }
        self.session = resolvedSession
        self.webSocketFactory = webSocketFactory ?? { request in
            resolvedSession.webSocketTask(with: request)
        }
        self.heartbeatSleeper = heartbeatSleeper
        self.deviceIdentityFetcher = deviceIdentityFetcher
    }

    nonisolated func connect(roomID: String) -> AsyncThrowingStream<BiliTransportUpdate, Error> {
        AsyncThrowingStream { continuation in
            let receiveTask = Task {
                do {
                    try await self.connectAndReceive(roomInput: roomID, continuation: continuation)
                } catch is CancellationError {
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { [weak self] _ in
                receiveTask.cancel()
                Task { await self?.disconnect() }
            }
        }
    }

    private func connectAndReceive(
        roomInput: String,
        continuation: AsyncThrowingStream<BiliTransportUpdate, Error>.Continuation
    ) async throws {
        disconnect()

        guard let requestedRoomID = Int(roomInput), requestedRoomID > 0 else {
            throw ClientError.invalidRoomID
        }

        async let deviceIdentity = deviceIdentityFetcher()
        let room = try await fetchRoomInit(roomID: requestedRoomID)
        let identity = await deviceIdentity
        let config = try await fetchDanmuConfig(roomID: room.roomID, identity: identity)
        let historyTask = Task {
            try? await BiliDanmuHistoryLoader(session: session).fetch(roomID: room.roomID)
        }
        let server = config.servers.first ?? BiliDanmuServer(host: config.host, wssPort: 443)
        guard let url = URL(string: "wss://\(server.host):\(server.wssPort)/sub") else {
            throw ClientError.invalidResponse("弹幕服务器地址无效")
        }

        let task = webSocketFactory(request(url: url, roomID: room.roomID, identity: identity))
        webSocketTask = task
        task.resume()

        try await sendPacket(
            operation: 7,
            body: BiliAuthPayload(roomID: room.roomID, token: config.token, identity: identity),
            retryCount: 5
        )
        startHeartbeat()

        let historicalEvents = await historyTask.value ?? []
        var didEmitHistory = false
        var deduplicator = BiliEventDeduplicator()

        while !Task.isCancelled {
            let message = try await task.receive()
            let packets = try BiliPacketCodec.decodePackets(data: data(from: message))
            for packet in packets {
                switch packet.operation {
                case 3:
                    if let onlineCount = packet.onlineCount {
                        continuation.yield(.onlineCount(onlineCount))
                    }
                case 5:
                    for acknowledgementBody in packet.acknowledgementBodies {
                        try await sendPacket(operation: 24, bodyData: acknowledgementBody)
                    }
                    for event in packet.events {
                        if deduplicator.shouldEmit(event) {
                            continuation.yield(.event(event))
                        }
                    }
                case 8:
                    if !didEmitHistory {
                        didEmitHistory = true
                        for event in historicalEvents {
                            if deduplicator.shouldEmit(event) {
                                continuation.yield(.event(event))
                            }
                        }
                    }
                    continuation.yield(.connected(resolvedRoomID: String(room.roomID)))
                default:
                    break
                }
            }
        }
    }

    private func disconnect() {
        heartbeatTask?.cancel()
        heartbeatTask = nil
        webSocketTask?.cancel(with: .normalClosure, reason: nil)
        webSocketTask = nil
    }

    private func startHeartbeat() {
        heartbeatTask?.cancel()
        let sleeper = heartbeatSleeper
        heartbeatTask = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    try await sleeper()
                } catch {
                    return
                }
                guard !Task.isCancelled else { return }
                do {
                    try await self?.sendPacket(operation: 2, body: "")
                } catch {
                    await self?.terminateAfterHeartbeatFailure()
                    return
                }
            }
        }
    }

    private func terminateAfterHeartbeatFailure() {
        webSocketTask?.cancel(with: .goingAway, reason: nil)
        webSocketTask = nil
    }

    private func sendPacket<T: Encodable>(
        operation: UInt32,
        body: T,
        retryCount: Int = 1
    ) async throws {
        let packet = try BiliPacketCodec.encode(operation: operation, body: body)
        try await sendEncodedPacket(packet, retryCount: retryCount)
    }

    private func sendPacket(
        operation: UInt32,
        bodyData: Data,
        retryCount: Int = 1
    ) async throws {
        let packet = BiliPacketCodec.encode(operation: operation, bodyData: bodyData)
        try await sendEncodedPacket(packet, retryCount: retryCount)
    }

    private func sendEncodedPacket(_ packet: Data, retryCount: Int) async throws {
        guard let webSocketTask else {
            throw ClientError.disconnected
        }

        var delay: UInt64 = 200_000_000
        let attempts = max(1, retryCount)

        for attempt in 1...attempts {
            do {
                try await webSocketTask.send(.data(packet))
                return
            } catch {
                guard attempt < attempts, !Task.isCancelled else {
                    throw error
                }
                try? await Task.sleep(nanoseconds: delay)
                delay = min(delay * 2, 1_200_000_000)
            }
        }
    }

    private func data(from message: URLSessionWebSocketTask.Message) throws -> Data {
        switch message {
        case .data(let data):
            data
        case .string(let string):
            Data(string.utf8)
        @unknown default:
            throw ClientError.disconnected
        }
    }

    private func fetchRoomInit(roomID: Int) async throws -> BiliRoomInfo {
        let url = URL(string: "https://api.live.bilibili.com/room/v1/Room/room_init?id=\(roomID)")!
        let (data, response) = try await session.data(for: request(url: url, roomID: roomID))
        try validateHTTP(response)
        let decoded = try JSONDecoder().decode(BiliRoomInitResponse.self, from: data)
        guard decoded.code == 0, let room = decoded.data else {
            throw ClientError.invalidResponse(decoded.message ?? decoded.msg ?? "直播间发现失败")
        }
        return room
    }

    private func fetchDanmuConfig(
        roomID: Int,
        identity: BiliDeviceIdentity?
    ) async throws -> BiliDanmuConfig {
        let url = URL(string: "https://api.live.bilibili.com/room/v1/Danmu/getConf?room_id=\(roomID)&platform=pc&player=web")!
        let (data, response) = try await session.data(
            for: request(url: url, roomID: roomID, identity: identity)
        )
        try validateHTTP(response)
        let decoded = try JSONDecoder().decode(BiliDanmuConfigResponse.self, from: data)
        guard decoded.code == 0, let config = decoded.data else {
            throw ClientError.invalidResponse(decoded.message ?? decoded.msg ?? "弹幕服务器发现失败")
        }
        return config
    }

    private func request(
        url: URL,
        roomID: Int,
        identity: BiliDeviceIdentity? = nil
    ) -> URLRequest {
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 12)
        request.setValue("https://live.bilibili.com", forHTTPHeaderField: "Origin")
        request.setValue("https://live.bilibili.com/\(roomID)", forHTTPHeaderField: "Referer")
        request.setValue(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/17.0 Safari/605.1.15",
            forHTTPHeaderField: "User-Agent"
        )
        request.setValue("application/json,text/plain,*/*", forHTTPHeaderField: "Accept")
        if let identity {
            request.setValue(identity.cookieHeader, forHTTPHeaderField: "Cookie")
        }
        return request
    }

    private func validateHTTP(_ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw ClientError.invalidResponse("B 站公开接口请求失败")
        }
    }
}

private struct BiliAuthPayload: Encodable {
    let uid = 0
    let roomid: Int
    let protover = 3
    let buvid: String
    let support_ack = true
    let queue_uuid: String
    let scene = ""
    let platform = "web"
    let type = 2
    let key: String

    init(roomID: Int, token: String, identity: BiliDeviceIdentity?) {
        roomid = roomID
        buvid = identity?.buvid3 ?? ""
        queue_uuid = String(
            UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased().prefix(8)
        )
        key = token
    }
}

private struct BiliRoomInitResponse: Decodable {
    let code: Int
    let message: String?
    let msg: String?
    let data: BiliRoomInfo?
}

private struct BiliRoomInfo: Decodable {
    let roomID: Int

    enum CodingKeys: String, CodingKey {
        case roomID = "room_id"
    }
}

private struct BiliDanmuConfigResponse: Decodable {
    let code: Int
    let message: String?
    let msg: String?
    let data: BiliDanmuConfig?
}

private struct BiliDanmuConfig: Decodable {
    let token: String
    let host: String
    let servers: [BiliDanmuServer]

    enum CodingKeys: String, CodingKey {
        case token
        case host
        case servers = "host_server_list"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        token = try container.decode(String.self, forKey: .token)
        host = try container.decode(String.self, forKey: .host)
        servers = try container.decodeIfPresent([BiliDanmuServer].self, forKey: .servers) ?? []
    }
}

private struct BiliDanmuServer: Decodable {
    let host: String
    let wssPort: Int

    enum CodingKeys: String, CodingKey {
        case host
        case wssPort = "wss_port"
    }

    init(host: String, wssPort: Int) {
        self.host = host
        self.wssPort = wssPort
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        host = try container.decode(String.self, forKey: .host)
        wssPort = try container.decodeIfPresent(Int.self, forKey: .wssPort) ?? 443
    }
}
