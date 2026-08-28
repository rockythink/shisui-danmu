import DanmuCore
import Foundation

struct BiliDanmuHistoryLoader: Sendable {
    let session: URLSession

    func fetch(roomID: Int) async throws -> [DanmuEvent] {
        let url = URL(
            string: "https://api.live.bilibili.com/xlive/web-room/v1/dM/gethistory?roomid=\(roomID)&room_type=0"
        )!
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 12)
        request.setValue("https://live.bilibili.com/\(roomID)", forHTTPHeaderField: "Referer")
        request.setValue(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/17.0 Safari/605.1.15",
            forHTTPHeaderField: "User-Agent"
        )
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw BiliDanmuHistoryError.requestFailed
        }
        return try BiliDanmuHistoryParser.parse(data: data)
    }
}

enum BiliDanmuHistoryParser {
    static func parse(data: Data) throws -> [DanmuEvent] {
        let response = try JSONDecoder().decode(BiliDanmuHistoryResponse.self, from: data)
        guard response.code == 0 else {
            throw BiliDanmuHistoryError.requestFailed
        }
        return (response.data?.room ?? [])
            .compactMap(\.event)
            .sorted { $0.timestamp < $1.timestamp }
    }
}

private enum BiliDanmuHistoryError: LocalizedError {
    case requestFailed

    var errorDescription: String? { "最近弹幕加载失败" }
}

private struct BiliDanmuHistoryResponse: Decodable {
    let code: Int
    let data: Payload?

    struct Payload: Decodable {
        let room: [Item]
    }

    struct Item: Decodable {
        let text: String?
        let uid: Int?
        let nickname: String?
        let timeline: String?
        let idString: String?

        enum CodingKeys: String, CodingKey {
            case text
            case uid
            case nickname
            case timeline
            case idString = "id_str"
        }

        var event: DanmuEvent? {
            guard let content = text?.nonEmptyTrimmed else {
                return nil
            }
            return DanmuEvent(
                id: stableIdentifier(content: content),
                kind: .danmu,
                timestamp: parsedTimeline ?? .now,
                username: nickname?.nonEmptyTrimmed ?? "观众",
                authorID: uid.map(String.init),
                content: content,
                origin: .history,
                platformEventID: idString?.nonEmptyTrimmed
            )
        }

        private var parsedTimeline: Date? {
            guard let timeline else { return nil }
            let formatter = DateFormatter()
            formatter.locale = Locale(identifier: "en_US_POSIX")
            formatter.timeZone = TimeZone(identifier: "Asia/Shanghai")
            formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
            return formatter.date(from: timeline)
        }

        private func stableIdentifier(content: String) -> String {
            if let idString = idString?.nonEmptyTrimmed {
                return "bili-\(idString)"
            }
            let source = "\(uid.map(String.init) ?? "anon")|\(timeline ?? "")|\(content)"
            var hash: UInt64 = 14_695_981_039_346_656_037
            for byte in source.utf8 {
                hash ^= UInt64(byte)
                hash &*= 1_099_511_628_211
            }
            return "bili-history-\(String(hash, radix: 16))"
        }
    }
}

private extension String {
    var nonEmptyTrimmed: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
