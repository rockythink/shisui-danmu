import DanmuCore
import Foundation

struct BiliEventDeduplicator: Sendable {
    private var seenEventIDs: Set<String> = []
    private var recentEvents: [DanmuEvent] = []
    private let crossOriginWindow: TimeInterval

    init(crossOriginWindow: TimeInterval = 3) {
        self.crossOriginWindow = max(0, crossOriginWindow)
    }

    mutating func shouldEmit(_ event: DanmuEvent) -> Bool {
        if !seenEventIDs.insert(event.id).inserted { return false }

        let isCrossOriginDuplicate = recentEvents.contains { previous in
            previous.origin != event.origin
                && abs(previous.timestamp.timeIntervalSince(event.timestamp)) <= crossOriginWindow
                && previous.kind == event.kind
                && QuestionClassifier.normalized(previous.content)
                    == QuestionClassifier.normalized(event.content)
                && authorsAreCompatible(previous, event)
        }
        guard !isCrossOriginDuplicate else { return false }

        recentEvents.append(event)
        let cutoff = event.timestamp.addingTimeInterval(-crossOriginWindow)
        recentEvents.removeAll { $0.timestamp < cutoff }
        return true
    }

    private func authorsAreCompatible(_ lhs: DanmuEvent, _ rhs: DanmuEvent) -> Bool {
        let lhsID = lhs.authorID?.usableAuthorID
        let rhsID = rhs.authorID?.usableAuthorID
        if let lhsID, let rhsID { return lhsID == rhsID }

        guard let lhsName = lhs.username?.trimmedNonEmpty,
              let rhsName = rhs.username?.trimmedNonEmpty else { return false }
        if lhsName.caseInsensitiveCompare(rhsName) == .orderedSame { return true }
        return lhsName.maskedPatternMatches(rhsName) || rhsName.maskedPatternMatches(lhsName)
    }
}

private extension String {
    var trimmedNonEmpty: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }

    var usableAuthorID: String? {
        guard let value = trimmedNonEmpty, value != "0" else { return nil }
        return value
    }

    func maskedPatternMatches(_ fullName: String) -> Bool {
        let pattern = replacingOccurrences(of: "＊", with: "*").lowercased()
        let fullName = fullName.lowercased()
        guard pattern.contains("*") else { return false }
        let parts = pattern.split(separator: "*", omittingEmptySubsequences: true).map(String.init)
        guard !parts.isEmpty else { return false }
        if let prefix = parts.first, !fullName.hasPrefix(prefix) { return false }
        if parts.count > 1, let suffix = parts.last, !fullName.hasSuffix(suffix) { return false }
        return true
    }
}
