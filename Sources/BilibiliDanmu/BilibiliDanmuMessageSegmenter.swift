import Foundation

public enum BilibiliDanmuMessageSegmenter {
    public static let compatibilitySegmentLimit = 20

    public static func segments(
        for message: String,
        limit: Int = compatibilitySegmentLimit
    ) -> [String] {
        guard limit > 0 else { return [] }
        let sanitized = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !sanitized.isEmpty else { return [] }

        var result: [String] = []
        var start = sanitized.startIndex
        while start < sanitized.endIndex {
            let end = sanitized.index(start, offsetBy: limit, limitedBy: sanitized.endIndex)
                ?? sanitized.endIndex
            result.append(String(sanitized[start..<end]))
            start = end
        }
        return result
    }
}

public extension BilibiliAccountClient {
    /// Sends a long draft as sequential Bilibili-safe messages. Individual
    /// requests still pass through `sendDanmu`, so account and platform limits
    /// remain enforced. Sending stops at the first failed segment.
    func sendDanmuSegments(
        message: String,
        roomID: String,
        delay: Duration = .seconds(1)
    ) async throws {
        let segments = BilibiliDanmuMessageSegmenter.segments(for: message)
        guard !segments.isEmpty else {
            throw AccountError.invalidMessage
        }

        for (index, segment) in segments.enumerated() {
            try await sendDanmu(message: segment, roomID: roomID)
            if index < segments.index(before: segments.endIndex) {
                try await Task.sleep(for: delay)
            }
        }
    }
}
