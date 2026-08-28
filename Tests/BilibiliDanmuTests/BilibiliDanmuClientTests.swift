import DanmuCore
import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili danmu session lifecycle")
struct BilibiliDanmuClientTests {
    @Test func recentEventsUsePublicHistoryWithoutAccountState() async {
        let expected = DanmuEvent(
            id: "history-event",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 100),
            username: "停车拾穗",
            content: "确认回流"
        )
        let requestedRoom = RequestedRoomRecorder()
        let client = BilibiliDanmuClient(
            reconnectPolicy: BiliReconnectPolicy(delays: [0]),
            sleeper: { _ in },
            connectionFactory: { _ in AsyncThrowingStream { $0.finish() } },
            recentEventsFetcher: { roomID in
                await requestedRoom.record(roomID)
                return [expected]
            }
        )

        let events = await client.recentEvents(roomID: " 5050 ")

        #expect(events == [expected])
        #expect(await requestedRoom.value == "5050")
    }

    @Test func failedConnectionReconnectsAndReturnsToConnected() async {
        let attempts = AttemptCounter()
        let event = DanmuEvent(
            id: "event-1",
            kind: .danmu,
            timestamp: Date(timeIntervalSince1970: 1),
            username: "观众",
            content: "重连成功"
        )
        let client = BilibiliDanmuClient(
            reconnectPolicy: BiliReconnectPolicy(delays: [0]),
            sleeper: { _ in },
            connectionFactory: { roomID in
                AsyncThrowingStream { continuation in
                    Task {
                        let attempt = await attempts.next()
                        if attempt == 1 {
                            continuation.finish(throwing: TestConnectionError.failed)
                        } else {
                            continuation.yield(.connected(resolvedRoomID: roomID))
                            continuation.yield(.event(event))
                        }
                    }
                }
            }
        )

        var updates: [BilibiliDanmuUpdate] = []
        for await update in client.connect(roomID: "123") {
            updates.append(update)
            if updates.count == 6 {
                break
            }
        }
        await client.disconnect()

        #expect(updates[0] == .connection(.connecting))
        #expect(updates[1] == .connection(.error(message: "测试连接失败")))
        #expect(updates[2] == .connection(.reconnecting(attempt: 1, delay: 0)))
        #expect(updates[3] == .connection(.connecting))
        #expect(updates[4] == .connection(.connected(roomID: "123")))
        #expect(updates[5] == .event(event))
        #expect(await attempts.value == 2)
    }

    @Test func invalidRoomEndsWithErrorAndDisconnectedStates() async {
        let client = BilibiliDanmuClient(
            reconnectPolicy: BiliReconnectPolicy(delays: [0]),
            sleeper: { _ in },
            connectionFactory: { _ in AsyncThrowingStream { $0.finish() } }
        )

        var updates: [BilibiliDanmuUpdate] = []
        for await update in client.connect(roomID: "not-a-room") {
            updates.append(update)
        }

        #expect(updates == [
            .connection(.error(message: "房间号不合法")),
            .connection(.disconnected),
        ])
    }
}

private actor RequestedRoomRecorder {
    private(set) var value: String?

    func record(_ roomID: String) {
        value = roomID
    }
}

private actor AttemptCounter {
    private(set) var value = 0

    func next() -> Int {
        value += 1
        return value
    }
}

private enum TestConnectionError: LocalizedError {
    case failed

    var errorDescription: String? { "测试连接失败" }
}
