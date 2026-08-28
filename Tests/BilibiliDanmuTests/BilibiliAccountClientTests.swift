import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili account danmu sending", .serialized)
struct BilibiliAccountClientTests {
    @Test func loginStatePersistsInPrivateAppFile() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("BilibiliAccountClientTests-\(UUID().uuidString)", isDirectory: true)
        let url = directory.appendingPathComponent("session.json", isDirectory: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = BilibiliAccountSessionStore(sessionURL: url)
        let credential = BilibiliAccountCredential(
            cookieHeader: "SESSDATA=private-value; bili_jct=private-csrf",
            csrf: "private-csrf"
        )

        try await store.save(credential)

        #expect(await store.currentCredential() == credential)
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        #expect(attributes[.posixPermissions] as? Int == 0o600)
    }

    @Test func signingOutRemovesPersistedLoginState() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("BilibiliAccountSignOutTests-\(UUID().uuidString)", isDirectory: true)
        let url = directory.appendingPathComponent("session.json", isDirectory: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = BilibiliAccountSessionStore(sessionURL: url)
        let credential = BilibiliAccountCredential(
            cookieHeader: "SESSDATA=private-value; bili_jct=private-csrf",
            csrf: "private-csrf"
        )
        try await store.save(credential)
        let client = BilibiliAccountClient(sessionURL: url)

        try await client.signOut()

        #expect(await store.currentCredential() == nil)
        #expect(!FileManager.default.fileExists(atPath: url.path))
    }

    @Test func signedInAccountCanSendDanmuWithoutExposingCredentials() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [BilibiliAccountURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let credential = BilibiliAccountCredential(
            cookieHeader: "SESSDATA=session-value; bili_jct=csrf-token",
            csrf: "csrf-token"
        )
        let client = BilibiliAccountClient(
            session: session,
            credentialProvider: StaticBilibiliCredentialProvider(credential: credential),
            now: { Date(timeIntervalSince1970: 1_234) }
        )

        BilibiliAccountURLProtocol.requestHandler = { request in
            #expect(request.url?.absoluteString == "https://api.live.bilibili.com/msg/send")
            #expect(request.httpMethod == "POST")
            #expect(request.value(forHTTPHeaderField: "Cookie") == credential.cookieHeader)
            #expect(request.value(forHTTPHeaderField: "Origin") == "https://live.bilibili.com")
            #expect(request.value(forHTTPHeaderField: "Referer") == "https://live.bilibili.com/392612")

            let body = try #require(Self.requestBodyString(from: request))
            #expect(body.contains("roomid=392612"))
            #expect(body.contains("csrf=csrf-token"))
            #expect(body.contains("csrf_token=csrf-token"))
            #expect(body.contains("rnd=1234"))
            #expect(body.contains("msg=%E4%B8%BB%E5%8A%A8%E5%8F%91%E5%BC%B9%E5%B9%95%E6%B5%8B%E8%AF%95"))

            let data = #"{"code":0,"message":"0"}"#.data(using: .utf8)!
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: ["Content-Type": "application/json"]
            )!
            return (response, data)
        }
        defer { BilibiliAccountURLProtocol.requestHandler = nil }

        try await client.sendDanmu(message: " 主动发弹幕测试 ", roomID: "392612")
    }

    @Test func sendingDanmuRequiresLoginState() async {
        let client = BilibiliAccountClient(
            session: URLSession(configuration: .ephemeral),
            credentialProvider: StaticBilibiliCredentialProvider(credential: nil)
        )

        await #expect(throws: BilibiliAccountClient.AccountError.notSignedIn) {
            try await client.sendDanmu(message: "测试", roomID: "392612")
        }
    }

    @Test func sendingDanmuRejectsMoreThanTwentyTwoCharactersBeforeNetwork() async {
        let credential = BilibiliAccountCredential(
            cookieHeader: "SESSDATA=session-value; bili_jct=csrf-token",
            csrf: "csrf-token"
        )
        let client = BilibiliAccountClient(
            session: URLSession(configuration: .ephemeral),
            credentialProvider: StaticBilibiliCredentialProvider(credential: credential)
        )

        await #expect(throws: BilibiliAccountClient.AccountError.messageTooLong(limit: 22)) {
            try await client.sendDanmu(message: String(repeating: "弹", count: 23), roomID: "392612")
        }
    }

    @Test func accountStatusExposesDisplayNameButNotCredentialFields() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [BilibiliAccountURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let credential = BilibiliAccountCredential(
            cookieHeader: "SESSDATA=private-value; bili_jct=private-csrf",
            csrf: "private-csrf"
        )
        let client = BilibiliAccountClient(
            session: session,
            credentialProvider: StaticBilibiliCredentialProvider(credential: credential)
        )

        BilibiliAccountURLProtocol.requestHandler = { request in
            #expect(request.url?.absoluteString == "https://api.bilibili.com/x/web-interface/nav")
            let data = #"{"code":0,"data":{"isLogin":true,"uname":"石头","mid":12345}}"#.data(using: .utf8)!
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: ["Content-Type": "application/json"]
            )!
            return (response, data)
        }
        defer { BilibiliAccountURLProtocol.requestHandler = nil }

        #expect(try await client.status() == .signedIn(displayName: "石头", userID: "12345"))
    }

    @Test func accountStatusRequiresBroadcasterNickname() async {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [BilibiliAccountURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let credential = BilibiliAccountCredential(
            cookieHeader: "SESSDATA=private-value; bili_jct=private-csrf",
            csrf: "private-csrf"
        )
        let client = BilibiliAccountClient(
            session: session,
            credentialProvider: StaticBilibiliCredentialProvider(credential: credential)
        )

        BilibiliAccountURLProtocol.requestHandler = { request in
            let data = #"{"code":0,"data":{"isLogin":true,"uname":" "}}"#.data(using: .utf8)!
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: ["Content-Type": "application/json"]
            )!
            return (response, data)
        }
        defer { BilibiliAccountURLProtocol.requestHandler = nil }

        await #expect(throws: BilibiliAccountClient.AccountError.requestFailed("未获取到 B 站昵称")) {
            try await client.status()
        }
    }

    private static func requestBodyString(from request: URLRequest) -> String? {
        if let body = request.httpBody {
            return String(data: body, encoding: .utf8)
        }
        guard let stream = request.httpBodyStream else { return nil }

        stream.open()
        defer { stream.close() }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 1_024)
        while stream.hasBytesAvailable {
            let count = stream.read(&buffer, maxLength: buffer.count)
            guard count > 0 else { break }
            data.append(buffer, count: count)
        }
        return String(data: data, encoding: .utf8)
    }
}

private struct StaticBilibiliCredentialProvider: BilibiliCredentialProviding {
    let credential: BilibiliAccountCredential?

    func currentCredential() async -> BilibiliAccountCredential? {
        credential
    }
}

private final class BilibiliAccountURLProtocol: URLProtocol, @unchecked Sendable {
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
