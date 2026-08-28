import Foundation

enum TerminalInput: Equatable, Sendable {
    case text(String)
    case paste(String)
    case enter
    case escape
    case tab
    case backTab
    case toggleMicrophone
    case muteMicrophone
    case backspace
    case delete
    case left
    case right
    case up
    case down
    case home
    case end
    case wordLeft
    case wordRight
    case control(Character)
}

struct TerminalInputDecoder: Sendable {
    private var buffer: [UInt8] = []
    private var escapeStartedAt: ContinuousClock.Instant?

    mutating func feed(
        _ bytes: [UInt8],
        now: ContinuousClock.Instant = .now
    ) -> [TerminalInput] {
        buffer.append(contentsOf: bytes)
        var result: [TerminalInput] = []

        while !buffer.isEmpty {
            if buffer[0] == 0x1B {
                if consumeEscapeSequence(into: &result, now: now) { continue }
                break
            }

            escapeStartedAt = nil
            let byte = buffer[0]
            if let input = controlInput(for: byte) {
                buffer.removeFirst()
                result.append(input)
                continue
            }

            let end = buffer.firstIndex { $0 == 0x1B || $0 < 0x20 || $0 == 0x7F } ?? buffer.endIndex
            let candidate = Array(buffer[..<end])
            guard !candidate.isEmpty else {
                buffer.removeFirst()
                continue
            }
            if let text = String(data: Data(candidate), encoding: .utf8) {
                buffer.removeFirst(candidate.count)
                result.append(.text(text))
                continue
            }
            if let validPrefix = longestValidUTF8Prefix(candidate) {
                buffer.removeFirst(validPrefix.count)
                result.append(.text(String(decoding: validPrefix, as: UTF8.self)))
                continue
            }
            // A UTF-8 scalar can span reads. Keep a short incomplete suffix.
            if candidate.count <= 4 { break }
            buffer.removeFirst()
        }
        return result
    }

    private mutating func consumeEscapeSequence(
        into result: inout [TerminalInput],
        now: ContinuousClock.Instant
    ) -> Bool {
        let pasteStart = Array("\u{001B}[200~".utf8)
        let pasteEnd = Array("\u{001B}[201~".utf8)
        if buffer.starts(with: pasteStart) {
            guard let end = buffer.range(of: pasteEnd, after: pasteStart.count) else { return false }
            let payload = Array(buffer[pasteStart.count..<end.lowerBound])
            buffer.removeFirst(end.upperBound)
            let text = String(decoding: payload, as: UTF8.self)
                .replacingOccurrences(of: "\r\n", with: " ")
                .replacingOccurrences(of: "\n", with: " ")
                .replacingOccurrences(of: "\r", with: " ")
            result.append(.paste(text))
            escapeStartedAt = nil
            return true
        }
        let sequences: [([UInt8], TerminalInput)] = [
            (Array("\u{001B}[20;2~".utf8), .muteMicrophone),
            (Array("\u{001B}[20~".utf8), .toggleMicrophone),
            (Array("\u{001B}[Z".utf8), .backTab),
            (Array("\u{001B}[A".utf8), .up),
            (Array("\u{001B}[B".utf8), .down),
            (Array("\u{001B}[C".utf8), .right),
            (Array("\u{001B}[D".utf8), .left),
            (Array("\u{001B}[H".utf8), .home),
            (Array("\u{001B}[F".utf8), .end),
            (Array("\u{001B}OH".utf8), .home),
            (Array("\u{001B}OF".utf8), .end),
            (Array("\u{001B}[1~".utf8), .home),
            (Array("\u{001B}[4~".utf8), .end),
            (Array("\u{001B}[3~".utf8), .delete),
            (Array("\u{001B}[1;3D".utf8), .wordLeft),
            (Array("\u{001B}[1;3C".utf8), .wordRight),
            ([0x1B, 0x62], .wordLeft),
            ([0x1B, 0x66], .wordRight),
        ]
        if let match = sequences.first(where: { buffer.starts(with: $0.0) }) {
            buffer.removeFirst(match.0.count)
            result.append(match.1)
            escapeStartedAt = nil
            return true
        }
        if buffer.count == 1 {
            if let started = escapeStartedAt {
                guard now - started >= .milliseconds(40) else { return false }
                buffer.removeFirst()
                result.append(.escape)
                escapeStartedAt = nil
                return true
            }
            escapeStartedAt = now
            return false
        }

        if sequences.contains(where: { $0.0.starts(with: buffer) }) || pasteStart.starts(with: buffer) {
            if escapeStartedAt == nil { escapeStartedAt = now }
            return false
        }

        // Consume an unknown complete CSI/SS3 sequence without turning its tail
        // into text. If it is still incomplete, wait for the next read.
        if buffer.count >= 2, buffer[1] == 0x5B || buffer[1] == 0x4F {
            if let final = buffer.dropFirst(2).firstIndex(where: { (0x40...0x7E).contains($0) }) {
                buffer.removeFirst(final + 1)
                escapeStartedAt = nil
                return true
            }
            return false
        }

        buffer.removeFirst()
        result.append(.escape)
        escapeStartedAt = nil
        return true
    }
    private func controlInput(for byte: UInt8) -> TerminalInput? {
        switch byte {
        case 0x0A, 0x0D: .enter
        case 0x09: .tab
        case 0x7F, 0x08: .backspace
        case 0x01...0x1A: .control(Character(UnicodeScalar(byte + 0x60)))
        default: nil
        }
    }


    private func longestValidUTF8Prefix(_ bytes: [UInt8]) -> [UInt8]? {
        guard bytes.count > 1 else { return nil }
        for count in stride(from: bytes.count - 1, through: 1, by: -1) {
            let prefix = Array(bytes.prefix(count))
            if String(data: Data(prefix), encoding: .utf8) != nil { return prefix }
        }
        return nil
    }
}

struct TerminalEditorSnapshot: Equatable, Sendable {
    let text: String
    let cursor: Int
}

struct TerminalLineEditor: Sendable {
    private var characters: [Character] = []
    private(set) var cursor = 0

    var snapshot: TerminalEditorSnapshot {
        TerminalEditorSnapshot(text: String(characters), cursor: cursor)
    }

    mutating func insert(_ value: String) {
        // Some macOS IMEs include invisible formatting scalars around committed
        // punctuation. Keeping them creates an invisible editor position that survives
        // deleting the visible punctuation and makes subsequent preedit text look offset.
        let sanitized = value
            .replacingOccurrences(of: "\u{200B}", with: "")
            .replacingOccurrences(of: "\u{2060}", with: "")
            .replacingOccurrences(of: "\u{FEFF}", with: "")
        let inserted = Array(sanitized.filter { !$0.isNewline })
        guard !inserted.isEmpty else { return }
        characters.insert(contentsOf: inserted, at: cursor)
        cursor += inserted.count
    }

    mutating func moveLeft() { cursor = max(0, cursor - 1) }
    mutating func moveRight() { cursor = min(characters.count, cursor + 1) }
    mutating func moveHome() { cursor = 0 }
    mutating func moveEnd() { cursor = characters.count }

    mutating func moveWordLeft() {
        while cursor > 0, characters[cursor - 1].isWhitespace { cursor -= 1 }
        while cursor > 0, !characters[cursor - 1].isWhitespace { cursor -= 1 }
    }

    mutating func moveWordRight() {
        while cursor < characters.count, !characters[cursor].isWhitespace { cursor += 1 }
        while cursor < characters.count, characters[cursor].isWhitespace { cursor += 1 }
    }

    mutating func backspace() {
        guard cursor > 0 else { return }
        characters.remove(at: cursor - 1)
        cursor -= 1
    }

    mutating func deleteForward() {
        guard cursor < characters.count else { return }
        characters.remove(at: cursor)
    }

    mutating func eraseLine() {
        characters.removeAll(keepingCapacity: true)
        cursor = 0
    }

    mutating func eraseToEnd() {
        guard cursor < characters.count else { return }
        characters.removeSubrange(cursor...)
    }

    mutating func eraseWordBackward() {
        let end = cursor
        moveWordLeft()
        characters.removeSubrange(cursor..<end)
    }


    mutating func takeValueWithoutHistory() -> String {
        let value = String(characters)
        characters.removeAll(keepingCapacity: true)
        cursor = 0
        return value
    }

    mutating func submit() -> String? {
        let value = String(characters).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        characters.removeAll(keepingCapacity: true)
        cursor = 0
        return value
    }

}

private extension Array where Element == UInt8 {
    func range(of needle: [UInt8], after start: Int) -> Range<Int>? {
        guard !needle.isEmpty, count >= needle.count, start <= count - needle.count else { return nil }
        for index in start...(count - needle.count) where self[index..<(index + needle.count)].elementsEqual(needle) {
            return index..<(index + needle.count)
        }
        return nil
    }
}
