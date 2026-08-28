import DanmuCore
import Foundation

public enum BilibiliConnectionState: Equatable, Sendable {
    case disconnected
    case connecting
    case connected(roomID: String)
    case reconnecting(attempt: Int, delay: TimeInterval)
    case error(message: String)
}

public enum BilibiliDanmuUpdate: Equatable, Sendable {
    case connection(BilibiliConnectionState)
    case event(DanmuEvent)
    case onlineCount(Int)
}

enum BiliTransportUpdate: Equatable, Sendable {
    case connected(resolvedRoomID: String)
    case event(DanmuEvent)
    case onlineCount(Int)
}

typealias BiliConnectionFactory = @Sendable (String) -> AsyncThrowingStream<BiliTransportUpdate, Error>
typealias BiliRecentEventsFetcher = @Sendable (String) async -> [DanmuEvent]

public actor BilibiliDanmuClient {
    private let reconnectPolicy: BiliReconnectPolicy
    private let sleeper: BiliReconnectSleeper
    private let connectionFactory: BiliConnectionFactory
    private let recentEventsFetcher: BiliRecentEventsFetcher
    private let roomSnapshotClient: BilibiliRoomSnapshotClient
    private var runTask: Task<Void, Never>?
    private var continuation: AsyncStream<BilibiliDanmuUpdate>.Continuation?
    private var streamID: UUID?

    public init() {
        self.reconnectPolicy = BiliReconnectPolicy()
        self.sleeper = { delay in
            let nanoseconds = UInt64(max(0, delay) * 1_000_000_000)
            try? await Task.sleep(nanoseconds: nanoseconds)
        }
        self.connectionFactory = { roomID in
            BiliLiveConnection().connect(roomID: roomID)
        }
        self.recentEventsFetcher = fetchBiliRecentEvents
        self.roomSnapshotClient = BilibiliRoomSnapshotClient()
    }

    init(
        reconnectPolicy: BiliReconnectPolicy,
        sleeper: @escaping BiliReconnectSleeper,
        connectionFactory: @escaping BiliConnectionFactory,
        recentEventsFetcher: @escaping BiliRecentEventsFetcher = fetchBiliRecentEvents,
        roomSnapshotClient: BilibiliRoomSnapshotClient = BilibiliRoomSnapshotClient()
    ) {
        self.reconnectPolicy = reconnectPolicy
        self.sleeper = sleeper
        self.connectionFactory = connectionFactory
        self.recentEventsFetcher = recentEventsFetcher
        self.roomSnapshotClient = roomSnapshotClient
    }

    public nonisolated func connect(roomID: String) -> AsyncStream<BilibiliDanmuUpdate> {
        let id = UUID()
        return AsyncStream { continuation in
            continuation.onTermination = { [weak self] _ in
                Task { await self?.stop(streamID: id, emitDisconnected: false) }
            }
            Task { await self.start(roomInput: roomID, streamID: id, continuation: continuation) }
        }
    }

    public func disconnect() {
        stop(streamID: streamID, emitDisconnected: true)
    }

    public func recentEvents(roomID: String) async -> [DanmuEvent] {
        let value = roomID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let roomNumber = Int(value), roomNumber > 0 else { return [] }
        return await recentEventsFetcher(String(roomNumber))
    }

    public func roomSnapshot(roomID: String) async throws -> BilibiliRoomSnapshot {
        try await roomSnapshotClient.fetch(roomID: roomID)
    }

    private func start(
        roomInput: String,
        streamID: UUID,
        continuation: AsyncStream<BilibiliDanmuUpdate>.Continuation
    ) {
        stop(streamID: self.streamID, emitDisconnected: false)
        self.streamID = streamID
        self.continuation = continuation

        let roomID = roomInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let roomNumber = Int(roomID), roomNumber > 0 else {
            continuation.yield(.connection(.error(message: "房间号不合法")))
            continuation.yield(.connection(.disconnected))
            continuation.finish()
            self.continuation = nil
            self.streamID = nil
            return
        }

        runTask = Task { [weak self] in
            await self?.run(roomID: roomID, streamID: streamID)
        }
    }

    private func run(roomID: String, streamID: UUID) async {
        var retryAttempt = 0

        while !Task.isCancelled, self.streamID == streamID {
            if retryAttempt > 0 {
                let delay = reconnectPolicy.delay(forAttempt: retryAttempt)
                continuation?.yield(.connection(.reconnecting(attempt: retryAttempt, delay: delay)))
                await sleeper(delay)
                guard !Task.isCancelled else { break }
            }

            continuation?.yield(.connection(.connecting))
            do {
                let updates = connectionFactory(roomID)
                for try await update in updates {
                    guard !Task.isCancelled, self.streamID == streamID else { break }
                    switch update {
                    case .connected(let resolvedRoomID):
                        retryAttempt = 0
                        continuation?.yield(.connection(.connected(roomID: resolvedRoomID)))
                    case .event(let event):
                        continuation?.yield(.event(event))
                    case .onlineCount(let count):
                        continuation?.yield(.onlineCount(count))
                    }
                }
                guard !Task.isCancelled else { break }
                throw BilibiliClientError.disconnected
            } catch is CancellationError {
                break
            } catch {
                guard !Task.isCancelled else { break }
                continuation?.yield(.connection(.error(message: error.localizedDescription)))
                retryAttempt += 1
            }
        }
    }

    private func stop(streamID: UUID?, emitDisconnected: Bool) {
        guard let streamID, self.streamID == streamID else {
            return
        }
        runTask?.cancel()
        runTask = nil
        if emitDisconnected {
            continuation?.yield(.connection(.disconnected))
        }
        continuation?.finish()
        continuation = nil
        self.streamID = nil
    }
}

private func fetchBiliRecentEvents(roomID: String) async -> [DanmuEvent] {
    guard let roomNumber = Int(roomID), roomNumber > 0 else { return [] }
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 12
    configuration.timeoutIntervalForResource = 12
    configuration.httpCookieStorage = nil
    configuration.httpShouldSetCookies = false
    configuration.urlCredentialStorage = nil
    let session = URLSession(configuration: configuration)
    return (try? await BiliDanmuHistoryLoader(session: session).fetch(roomID: roomNumber)) ?? []
}

private enum BilibiliClientError: LocalizedError {
    case disconnected

    var errorDescription: String? { "弹幕连接已断开" }
}
