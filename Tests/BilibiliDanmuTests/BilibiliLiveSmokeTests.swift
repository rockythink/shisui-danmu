import DanmuCore
import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili live smoke", .serialized)
struct BilibiliLiveSmokeTests {
    @Test func publicRoomLoadsRecentHistoryWhenConfigured() async {
        guard let roomID = ProcessInfo.processInfo.environment["BILIBILI_LIVE_SMOKE_ROOM_ID"] else {
            return
        }

        var sawConnected = false
        var receivedHistoryEvent: DanmuEvent?
        let client = BilibiliDanmuClient()
        let timeout = Task {
            try? await Task.sleep(nanoseconds: 20_000_000_000)
            await client.disconnect()
        }

        for await update in client.connect(roomID: roomID) {
            switch update {
            case .connection(.connected):
                sawConnected = true
            case .event(let event) where event.origin == .history:
                receivedHistoryEvent = event
            default:
                break
            }
            if sawConnected, receivedHistoryEvent != nil {
                break
            }
        }

        timeout.cancel()
        await client.disconnect()

        #expect(sawConnected)
        #expect(receivedHistoryEvent != nil)
    }

    @Test func publicRoomConnectsReceivesLiveDanmuAndReconnectsWhenConfigured() async {
        guard let roomID = ProcessInfo.processInfo.environment["BILIBILI_LIVE_SMOKE_ROOM_ID"] else {
            return
        }

        let attempts = LiveAttemptCounter()
        let client = BilibiliDanmuClient(
            reconnectPolicy: BiliReconnectPolicy(delays: [0.2]),
            sleeper: { delay in
                try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            },
            connectionFactory: { roomID in
                AsyncThrowingStream { continuation in
                    let task = Task {
                        let attempt = await attempts.next()
                        do {
                            for try await update in BiliLiveConnection().connect(roomID: roomID) {
                                continuation.yield(update)
                                if attempt == 1, case .connected = update {
                                    continuation.finish(throwing: ForcedLiveDisconnect.triggered)
                                    return
                                }
                            }
                            continuation.finish()
                        } catch {
                            continuation.finish(throwing: error)
                        }
                    }
                    continuation.onTermination = { _ in task.cancel() }
                }
            }
        )

        var sawReconnect = false
        var connectedCount = 0
        var receivedDanmu: DanmuEvent?
        let timeout = Task {
            try? await Task.sleep(nanoseconds: 60_000_000_000)
            await client.disconnect()
        }

        for await update in client.connect(roomID: roomID) {
            switch update {
            case .connection(.connected):
                connectedCount += 1
            case .connection(.reconnecting):
                sawReconnect = true
            case .event(let event)
                where connectedCount >= 2 && event.origin == .live && event.kind == .danmu:
                receivedDanmu = event
            default:
                break
            }
            if connectedCount >= 2, receivedDanmu != nil {
                break
            }
        }

        timeout.cancel()
        await client.disconnect()

        #expect(sawReconnect)
        #expect(connectedCount >= 2)
        #expect(receivedDanmu != nil)
    }
}

private actor LiveAttemptCounter {
    private var value = 0

    func next() -> Int {
        value += 1
        return value
    }
}

private enum ForcedLiveDisconnect: LocalizedError {
    case triggered

    var errorDescription: String? { "测试触发连接中断" }
}
