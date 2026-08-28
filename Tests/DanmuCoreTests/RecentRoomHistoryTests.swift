import DanmuCore
import Testing

@Suite("Recent room history")
struct RecentRoomHistoryTests {
    @Test func initializationNormalizesDeduplicatesAndLimitsRooms() {
        let history = RecentRoomHistory(
            roomIDs: [" 5050 ", "123", "5050", "", "456"],
            limit: 3
        )

        #expect(history.roomIDs == ["5050", "123", "456"])
    }

    @Test func recordingMovesRoomToFrontAndDropsOldestRoom() {
        var history = RecentRoomHistory(roomIDs: ["5050", "123", "456"], limit: 3)

        history.record("123")
        #expect(history.roomIDs == ["123", "5050", "456"])

        history.record(" 789 ")
        #expect(history.roomIDs == ["789", "123", "5050"])
    }
}
