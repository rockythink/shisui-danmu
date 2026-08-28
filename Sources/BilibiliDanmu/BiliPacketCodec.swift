import Compression
import DanmuCore
import Foundation

enum BiliPacketCodec {
    enum CodecError: Error {
        case decompressionFailed
    }

    static func encode<T: Encodable>(operation: UInt32, body: T) throws -> Data {
        let bodyData: Data
        if let string = body as? String {
            bodyData = Data(string.utf8)
        } else {
            bodyData = try JSONEncoder().encode(body)
        }
        return encode(operation: operation, bodyData: bodyData)
    }

    static func encode(operation: UInt32, bodyData: Data, version: UInt16 = 1) -> Data {
        var data = Data()
        data.appendUInt32BE(UInt32(16 + bodyData.count))
        data.appendUInt16BE(16)
        data.appendUInt16BE(version)
        data.appendUInt32BE(operation)
        data.appendUInt32BE(1)
        data.append(bodyData)
        return data
    }

    static func decodePackets(data: Data) throws -> [BiliPacket] {
        var packets: [BiliPacket] = []
        var offset = 0

        while offset + 16 <= data.count {
            let packetLength = Int(data.readUInt32BE(at: offset))
            let headerLength = Int(data.readUInt16BE(at: offset + 4))
            let version = data.readUInt16BE(at: offset + 6)
            let operation = data.readUInt32BE(at: offset + 8)

            guard packetLength >= headerLength, offset + packetLength <= data.count else {
                break
            }

            let body = data.subdata(in: (offset + headerLength)..<(offset + packetLength))
            if operation == 5, version == 2 {
                packets.append(contentsOf: try decodePackets(data: decompress(body, algorithm: COMPRESSION_ZLIB)))
            } else if operation == 5, version == 3 {
                packets.append(contentsOf: try decodePackets(data: decompress(body, algorithm: COMPRESSION_BROTLI)))
            } else {
                packets.append(BiliPacket(version: version, operation: operation, body: body))
            }
            offset += packetLength
        }

        return packets
    }

    private static func decompress(_ data: Data, algorithm: compression_algorithm) throws -> Data {
        var outputSize = max(64 * 1_024, data.count * 4)
        while outputSize <= 32 * 1_024 * 1_024 {
            var decoded = Data(count: outputSize)
            let decodedCount = decoded.withUnsafeMutableBytes { outputBuffer in
                data.withUnsafeBytes { inputBuffer in
                    guard
                        let destination = outputBuffer.bindMemory(to: UInt8.self).baseAddress,
                        let source = inputBuffer.bindMemory(to: UInt8.self).baseAddress
                    else {
                        return 0
                    }
                    return compression_decode_buffer(
                        destination,
                        outputSize,
                        source,
                        data.count,
                        nil,
                        algorithm
                    )
                }
            }
            if decodedCount > 0 {
                decoded.count = decodedCount
                return decoded
            }
            outputSize *= 2
        }
        throw CodecError.decompressionFailed
    }
}

struct BiliPacket: Equatable {
    let version: UInt16
    let operation: UInt32
    let body: Data

    var onlineCount: Int? {
        guard operation == 3, body.count >= 4 else {
            return nil
        }
        return Int(body.readUInt32BE(at: 0))
    }

    var events: [DanmuEvent] {
        guard operation == 5, version <= 1 else {
            return []
        }
        return commandParts.compactMap { part in
            BiliLiveCommandParser.parse(jsonData: Data(part.utf8))
        }
    }

    var commandNames: [String] {
        commandParts.compactMap { part in
            guard
                let object = try? JSONSerialization.jsonObject(with: Data(part.utf8)),
                let raw = object as? [String: Any],
                let rawCommand = raw["cmd"] as? String
            else {
                return nil
            }
            return rawCommand.split(separator: ":").first.map(String.init) ?? rawCommand
        }
    }

    var acknowledgementBodies: [Data] {
        commandParts.compactMap { part in
            guard
                let object = try? JSONSerialization.jsonObject(with: Data(part.utf8)),
                let raw = object as? [String: Any],
                raw["p_is_ack"] as? Bool == true,
                let messageID = raw["msg_id"],
                !(messageID is NSNull),
                let command = raw["cmd"] as? String
            else {
                return nil
            }

            let acknowledgement: [String: Any] = [
                "msg_id": messageID,
                "cmd": command,
                "p_msg_type": raw["p_msg_type"] ?? 0,
            ]
            guard JSONSerialization.isValidJSONObject(acknowledgement) else { return nil }
            return try? JSONSerialization.data(withJSONObject: acknowledgement, options: [.sortedKeys])
        }
    }

    private var commandParts: [Substring] {
        guard operation == 5, version <= 1 else {
            return []
        }
        let text = String(decoding: body, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? [] : text.split(separator: "\0")
    }
}

private extension Data {
    mutating func appendUInt16BE(_ value: UInt16) {
        append(UInt8((value >> 8) & 0xff))
        append(UInt8(value & 0xff))
    }

    mutating func appendUInt32BE(_ value: UInt32) {
        append(UInt8((value >> 24) & 0xff))
        append(UInt8((value >> 16) & 0xff))
        append(UInt8((value >> 8) & 0xff))
        append(UInt8(value & 0xff))
    }

    func readUInt16BE(at offset: Int) -> UInt16 {
        (UInt16(self[offset]) << 8) | UInt16(self[offset + 1])
    }

    func readUInt32BE(at offset: Int) -> UInt32 {
        (UInt32(self[offset]) << 24) |
            (UInt32(self[offset + 1]) << 16) |
            (UInt32(self[offset + 2]) << 8) |
            UInt32(self[offset + 3])
    }
}
