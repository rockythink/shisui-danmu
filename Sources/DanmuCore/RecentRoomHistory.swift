import Foundation

public struct RecentRoomHistory: Equatable, Sendable {
    public private(set) var roomIDs: [String]
    public let limit: Int

    public init(roomIDs: [String] = [], limit: Int = 8) {
        self.limit = max(1, limit)

        var seen = Set<String>()
        self.roomIDs = roomIDs.compactMap { roomID in
            let normalized = roomID.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !normalized.isEmpty, seen.insert(normalized).inserted else {
                return nil
            }
            return normalized
        }
        .prefix(self.limit)
        .map(\.self)
    }

    public mutating func record(_ roomID: String) {
        let normalized = roomID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalized.isEmpty else { return }

        roomIDs.removeAll { $0 == normalized }
        roomIDs.insert(normalized, at: 0)
        if roomIDs.count > limit {
            roomIDs.removeLast(roomIDs.count - limit)
        }
    }
}
