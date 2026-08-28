import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili room snapshot", .serialized)
struct BilibiliRoomSnapshotTests {
    @Test func fetchesRoomMetadataAndSortedOnlineRank() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RoomSnapshotURLProtocol.self]
        let client = BilibiliRoomSnapshotClient(session: URLSession(configuration: configuration))

        RoomSnapshotURLProtocol.requestHandler = { request in
            let body: String
            if request.url?.path.contains("getOnlineGoldRank") == true {
                body = #"{"code":0,"data":{"OnlineRankItem":[{"name":"第二名","score":80,"userRank":2},{"name":"第一名","score":100,"userRank":1}]}}"#
            } else {
                body = #"{"code":0,"data":{"room_id":9527,"uid":42,"title":"数据夜谈","parent_area_name":"知识","area_name":"科技科普","online":1234,"attention":5678,"live_time":"2026-07-20 20:00:00"}}"#
            }
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: ["Content-Type": "application/json"]
            )!
            return (response, Data(body.utf8))
        }
        defer { RoomSnapshotURLProtocol.requestHandler = nil }

        let snapshot = try await client.fetch(roomID: " 9527 ")

        #expect(snapshot.roomID == "9527")
        #expect(snapshot.broadcasterID == "42")
        #expect(snapshot.title == "数据夜谈")
        #expect(snapshot.parentAreaName == "知识")
        #expect(snapshot.areaName == "科技科普")
        #expect(snapshot.onlineCount == 1_234)
        #expect(snapshot.followerCount == 5_678)
        #expect(snapshot.liveStartedAt != nil)
        #expect(snapshot.onlineRankUsers.map(\.name) == ["第一名", "第二名"])
    }

    @Test func rankFailureDoesNotDiscardRoomMetadata() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RoomSnapshotURLProtocol.self]
        let client = BilibiliRoomSnapshotClient(session: URLSession(configuration: configuration))

        RoomSnapshotURLProtocol.requestHandler = { request in
            let status = request.url?.path.contains("getOnlineGoldRank") == true ? 500 : 200
            let body = #"{"code":0,"data":{"room_id":1,"uid":2,"title":"直播间","parent_area_name":"知识","area_name":"社科","online":3,"attention":4,"live_time":"0000-00-00 00:00:00"}}"#
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!
            return (response, Data(body.utf8))
        }
        defer { RoomSnapshotURLProtocol.requestHandler = nil }

        let snapshot = try await client.fetch(roomID: "1")

        #expect(snapshot.onlineRankUsers.isEmpty)
        #expect(snapshot.liveStartedAt == nil)
    }
}

private final class RoomSnapshotURLProtocol: URLProtocol, @unchecked Sendable {
    nonisolated(unsafe) static var requestHandler: ((URLRequest) throws -> (HTTPURLResponse, Data))?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let requestHandler = Self.requestHandler else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        do {
            let (response, data) = try requestHandler(request)
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    override func stopLoading() {}
}
