import Compression
import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili packet codec")
struct BilibiliPacketCodecTests {
    @Test func decodesOnlinePacket() throws {
        let onlineBody = Data([0, 0, 4, 210])
        let data = BiliPacketCodec.encode(operation: 3, bodyData: onlineBody)

        let packets = try BiliPacketCodec.decodePackets(data: data)

        #expect(packets.count == 1)
        #expect(packets.first?.operation == 3)
        #expect(packets.first?.onlineCount == 1_234)
    }

    @Test func decodesCommandPacketIntoStandardEvent() throws {
        let command = #"{"cmd":"DANMU_MSG","info":[[],"今天讲 Flink 吗？",[101,"石头"],[],[]]}"#
        let data = try BiliPacketCodec.encode(operation: 5, body: command)

        let event = try BiliPacketCodec.decodePackets(data: data).first?.events.first

        #expect(event?.kind == .danmu)
        #expect(event?.username == "石头")
        #expect(event?.content == "今天讲 Flink 吗？")
    }

    @Test func decodesZlibCompressedCommandPacket() throws {
        let command = #"{"cmd":"DANMU_MSG","info":[[],"压缩弹幕",[202,"观众"],[],[]]}"#
        let innerPacket = try BiliPacketCodec.encode(operation: 5, body: command)
        let compressed = try compress(innerPacket, algorithm: COMPRESSION_ZLIB)
        let outerPacket = BiliPacketCodec.encode(operation: 5, bodyData: compressed, version: 2)

        let event = try BiliPacketCodec.decodePackets(data: outerPacket).first?.events.first

        #expect(event?.kind == .danmu)
        #expect(event?.content == "压缩弹幕")
    }

    @Test func decodesBrotliCompressedCommandPacket() throws {
        let command = #"{"cmd":"LIKE_INFO_V3_CLICK","data":{"uname":"观众"}}"#
        let innerPacket = try BiliPacketCodec.encode(operation: 5, body: command)
        let compressed = try compress(innerPacket, algorithm: COMPRESSION_BROTLI)
        let outerPacket = BiliPacketCodec.encode(operation: 5, bodyData: compressed, version: 3)

        let event = try BiliPacketCodec.decodePackets(data: outerPacket).first?.events.first

        #expect(event?.kind == .like)
        #expect(event?.username == "观众")
    }

    @Test func buildsSocketAcknowledgementForCommandsThatRequestIt() throws {
        let command = #"{"cmd":"DANMU_MSG","msg_id":"message-1","p_is_ack":true,"p_msg_type":1,"info":[[],"需要确认的弹幕",[303,"观众"],[],[]]}"#
        let data = try BiliPacketCodec.encode(operation: 5, body: command)
        let packet = try #require(BiliPacketCodec.decodePackets(data: data).first)

        let acknowledgement = try #require(packet.acknowledgementBodies.first)
        let body = try #require(
            JSONSerialization.jsonObject(with: acknowledgement) as? [String: Any]
        )

        #expect(body["msg_id"] as? String == "message-1")
        #expect(body["cmd"] as? String == "DANMU_MSG")
        #expect(body["p_msg_type"] as? Int == 1)
    }

    private func compress(_ data: Data, algorithm: compression_algorithm) throws -> Data {
        var outputSize = max(64, data.count * 2)
        while outputSize <= 1024 * 1024 {
            var output = Data(count: outputSize)
            let encodedCount = output.withUnsafeMutableBytes { outputBuffer in
                data.withUnsafeBytes { inputBuffer in
                    compression_encode_buffer(
                        outputBuffer.bindMemory(to: UInt8.self).baseAddress!,
                        outputSize,
                        inputBuffer.bindMemory(to: UInt8.self).baseAddress!,
                        data.count,
                        nil,
                        algorithm
                    )
                }
            }
            if encodedCount > 0 {
                output.count = encodedCount
                return output
            }
            outputSize *= 2
        }
        throw TestCompressionError.failed
    }
}

private enum TestCompressionError: Error {
    case failed
}
