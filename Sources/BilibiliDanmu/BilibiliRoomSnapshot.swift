import Foundation

public struct BilibiliOnlineRankUser: Codable, Equatable, Sendable {
    public let name: String
    public let score: Int
    public let rank: Int

    public init(name: String, score: Int, rank: Int) {
        self.name = name
        self.score = score
        self.rank = rank
    }
}

public struct BilibiliRoomSnapshot: Codable, Equatable, Sendable {
    public let roomID: String
    public let broadcasterID: String
    public let title: String
    public let parentAreaName: String
    public let areaName: String
    public let onlineCount: Int
    public let followerCount: Int
    public let liveStartedAt: Date?
    public let onlineRankUsers: [BilibiliOnlineRankUser]

    public init(
        roomID: String,
        broadcasterID: String,
        title: String,
        parentAreaName: String,
        areaName: String,
        onlineCount: Int,
        followerCount: Int,
        liveStartedAt: Date?,
        onlineRankUsers: [BilibiliOnlineRankUser]
    ) {
        self.roomID = roomID
        self.broadcasterID = broadcasterID
        self.title = title
        self.parentAreaName = parentAreaName
        self.areaName = areaName
        self.onlineCount = onlineCount
        self.followerCount = followerCount
        self.liveStartedAt = liveStartedAt
        self.onlineRankUsers = onlineRankUsers
    }
}

public struct BilibiliRoomSnapshotClient: Sendable {
    public enum ClientError: LocalizedError, Equatable, Sendable {
        case invalidRoomID
        case requestFailed(String)

        public var errorDescription: String? {
            switch self {
            case .invalidRoomID:
                "房间号不合法"
            case .requestFailed(let message):
                message
            }
        }
    }

    private let session: URLSession

    public init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 12
        configuration.timeoutIntervalForResource = 12
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        session = URLSession(configuration: configuration)
    }

    init(session: URLSession) {
        self.session = session
    }

    public func fetch(roomID: String) async throws -> BilibiliRoomSnapshot {
        let candidate = roomID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let roomNumber = Int(candidate), roomNumber > 0 else {
            throw ClientError.invalidRoomID
        }

        let room = try await fetchRoom(roomID: roomNumber)
        let rankUsers = (try? await fetchRank(roomID: room.roomID, broadcasterID: room.broadcasterID)) ?? []
        return BilibiliRoomSnapshot(
            roomID: String(room.roomID),
            broadcasterID: String(room.broadcasterID),
            title: room.title,
            parentAreaName: room.parentAreaName,
            areaName: room.areaName,
            onlineCount: room.onlineCount,
            followerCount: room.followerCount,
            liveStartedAt: Self.parseBilibiliDate(room.liveTime),
            onlineRankUsers: rankUsers.sorted { lhs, rhs in
                if lhs.rank != rhs.rank { return lhs.rank < rhs.rank }
                return lhs.score > rhs.score
            }
        )
    }

    private func fetchRoom(roomID: Int) async throws -> RoomData {
        let url = URL(string: "https://api.live.bilibili.com/room/v1/room/get_info?room_id=\(roomID)")!
        let (data, response) = try await session.data(for: request(url: url, roomID: roomID))
        try validate(response)
        let decoded = try JSONDecoder().decode(RoomResponse.self, from: data)
        guard decoded.code == 0, let room = decoded.data else {
            throw ClientError.requestFailed(decoded.message ?? decoded.msg ?? "获取直播间信息失败")
        }
        return room
    }

    private func fetchRank(roomID: Int, broadcasterID: Int) async throws -> [BilibiliOnlineRankUser] {
        var components = URLComponents(string: "https://api.live.bilibili.com/xlive/general-interface/v1/rank/getOnlineGoldRank")!
        components.queryItems = [
            URLQueryItem(name: "ruid", value: String(broadcasterID)),
            URLQueryItem(name: "roomId", value: String(roomID)),
            URLQueryItem(name: "page", value: "1"),
            URLQueryItem(name: "pageSize", value: "50"),
        ]
        let (data, response) = try await session.data(for: request(url: components.url!, roomID: roomID))
        try validate(response)
        let decoded = try JSONDecoder().decode(RankResponse.self, from: data)
        guard decoded.code == 0 else {
            throw ClientError.requestFailed(decoded.message ?? decoded.msg ?? "获取在线榜失败")
        }
        return (decoded.data?.items ?? []).map {
            BilibiliOnlineRankUser(name: $0.name, score: $0.score, rank: $0.rank)
        }
    }

    private func request(url: URL, roomID: Int) -> URLRequest {
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 12)
        request.setValue("https://live.bilibili.com/\(roomID)", forHTTPHeaderField: "Referer")
        request.setValue(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/17.0 Safari/605.1.15",
            forHTTPHeaderField: "User-Agent"
        )
        request.setValue("application/json,text/plain,*/*", forHTTPHeaderField: "Accept")
        return request
    }

    private func validate(_ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw ClientError.requestFailed("B 站公开接口请求失败")
        }
    }

    private static func parseBilibiliDate(_ value: String) -> Date? {
        guard !value.isEmpty, value != "0000-00-00 00:00:00" else { return nil }
        let formatter = DateFormatter()
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 8 * 60 * 60)
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return formatter.date(from: value)
    }
}

private struct RoomResponse: Decodable {
    let code: Int
    let message: String?
    let msg: String?
    let data: RoomData?
}

private struct RoomData: Decodable {
    let roomID: Int
    let broadcasterID: Int
    let title: String
    let parentAreaName: String
    let areaName: String
    let onlineCount: Int
    let followerCount: Int
    let liveTime: String

    enum CodingKeys: String, CodingKey {
        case roomID = "room_id"
        case broadcasterID = "uid"
        case title
        case parentAreaName = "parent_area_name"
        case areaName = "area_name"
        case onlineCount = "online"
        case followerCount = "attention"
        case liveTime = "live_time"
    }
}

private struct RankResponse: Decodable {
    let code: Int
    let message: String?
    let msg: String?
    let data: RankData?
}

private struct RankData: Decodable {
    let items: [RankItem]

    enum CodingKeys: String, CodingKey {
        case items = "OnlineRankItem"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        items = try container.decodeIfPresent([RankItem].self, forKey: .items) ?? []
    }
}

private struct RankItem: Decodable {
    let name: String
    let score: Int
    let rank: Int

    enum CodingKeys: String, CodingKey {
        case name
        case score
        case rank = "userRank"
    }
}
