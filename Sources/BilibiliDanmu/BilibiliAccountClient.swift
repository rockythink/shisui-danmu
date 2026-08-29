import Foundation

struct BilibiliAccountCredential: Codable, Equatable, Sendable {
    let cookieHeader: String
    let csrf: String

    var isUsable: Bool {
        !cookieHeader.isEmpty && !csrf.isEmpty
    }
}

protocol BilibiliCredentialProviding: Sendable {
    func currentCredential() async -> BilibiliAccountCredential?
}

actor BilibiliAccountSessionStore: BilibiliCredentialProviding {
    private let sessionURL: URL

    init(sessionURL: URL) {
        self.sessionURL = sessionURL
    }

    func currentCredential() -> BilibiliAccountCredential? {
        guard let data = try? Data(contentsOf: sessionURL) else {
            return nil
        }
        return try? JSONDecoder().decode(BilibiliAccountCredential.self, from: data)
    }

    func save(_ credential: BilibiliAccountCredential) throws {
        try FileManager.default.createDirectory(
            at: sessionURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let data = try JSONEncoder().encode(credential)
        try data.write(to: sessionURL, options: .atomic)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o600],
            ofItemAtPath: sessionURL.path
        )
    }

    func clear() throws {
        guard FileManager.default.fileExists(atPath: sessionURL.path) else {
            return
        }
        try FileManager.default.removeItem(at: sessionURL)
    }

}

public enum BilibiliAccountStatus: Equatable, Sendable {
    case signedOut
    case signedIn(displayName: String, userID: String)
}

public actor BilibiliAccountClient {
    public enum AccountError: LocalizedError, Equatable, Sendable {
        case notSignedIn
        case invalidMessage
        case messageTooLong(limit: Int)
        case requestFailed(String)

        public var errorDescription: String? {
            switch self {
            case .notSignedIn:
                "还没有 B 站登录态"
            case .invalidMessage:
                "弹幕内容不能为空"
            case .messageTooLong(let limit):
                "弹幕不能超过 \(limit) 个字"
            case .requestFailed(let message):
                message
            }
        }
    }

    private let session: URLSession
    private let credentialProvider: any BilibiliCredentialProviding
    private let credentialStore: BilibiliAccountSessionStore?
    private let now: @Sendable () -> Date

    public static let messageCharacterLimit = 22

    public init(sessionURL: URL) {
        let store = BilibiliAccountSessionStore(sessionURL: sessionURL)
        self.session = URLSession(configuration: .ephemeral)
        self.credentialProvider = store
        self.credentialStore = store
        self.now = Date.init
    }

    init(
        session: URLSession,
        credentialProvider: any BilibiliCredentialProviding,
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.session = session
        self.credentialProvider = credentialProvider
        self.credentialStore = credentialProvider as? BilibiliAccountSessionStore
        self.now = now
    }

    func saveLoginCookies(_ cookies: [HTTPCookie]) async throws -> BilibiliAccountStatus {
        guard let credentialStore,
              let credential = BilibiliAccountCredentialParser.credential(from: cookies) else {
            throw AccountError.notSignedIn
        }
        try await credentialStore.save(credential)
        return try await status()
    }

    public func status() async throws -> BilibiliAccountStatus {
        guard let credential = await credentialProvider.currentCredential(), credential.isUsable else {
            return .signedOut
        }

        var request = URLRequest(url: URL(string: "https://api.bilibili.com/x/web-interface/nav")!)
        request.setValue(credential.cookieHeader, forHTTPHeaderField: "Cookie")
        request.setValue("https://www.bilibili.com", forHTTPHeaderField: "Referer")
        request.setValue(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
            forHTTPHeaderField: "User-Agent"
        )

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw AccountError.requestFailed("验证 B 站账号状态失败")
        }
        let decoded = try JSONDecoder().decode(BilibiliNavResponse.self, from: data)
        guard decoded.code == 0,
              let profile = decoded.data,
              profile.isLogin == true else {
            return .signedOut
        }
        guard let displayName = profile.uname?.trimmingCharacters(in: .whitespacesAndNewlines),
              !displayName.isEmpty else {
            throw AccountError.requestFailed("未获取到 B 站昵称")
        }
        guard let userID = profile.mid, userID > 0 else {
            throw AccountError.requestFailed("未获取到 B 站账号标识")
        }
        return .signedIn(displayName: displayName, userID: String(userID))
    }

    public func signOut() async throws {
        guard let credentialStore else {
            throw AccountError.requestFailed("当前登录态不能由此客户端清除")
        }
        try await credentialStore.clear()
    }

    public func sendDanmu(
        message: String,
        roomID: String,
        replyToAuthorID: String? = nil
    ) async throws {
        guard let credential = await credentialProvider.currentCredential(), credential.isUsable else {
            throw AccountError.notSignedIn
        }

        let sanitizedMessage = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !sanitizedMessage.isEmpty else {
            throw AccountError.invalidMessage
        }
        guard sanitizedMessage.count <= Self.messageCharacterLimit else {
            throw AccountError.messageTooLong(limit: Self.messageCharacterLimit)
        }

        let sanitizedRoomID = roomID.trimmingCharacters(in: .whitespacesAndNewlines)
        let replyMID = replyToAuthorID?.nonEmptyTrimmed ?? "0"
        var request = URLRequest(url: URL(string: "https://api.live.bilibili.com/msg/send")!)
        request.httpMethod = "POST"
        request.setValue("https://live.bilibili.com", forHTTPHeaderField: "Origin")
        request.setValue("https://live.bilibili.com/\(sanitizedRoomID)", forHTTPHeaderField: "Referer")
        request.setValue(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
            forHTTPHeaderField: "User-Agent"
        )
        request.setValue("application/x-www-form-urlencoded; charset=UTF-8", forHTTPHeaderField: "Content-Type")
        request.setValue(credential.cookieHeader, forHTTPHeaderField: "Cookie")

        var components = URLComponents()
        components.queryItems = [
            URLQueryItem(name: "bubble", value: "0"),
            URLQueryItem(name: "msg", value: sanitizedMessage),
            URLQueryItem(name: "color", value: "16777215"),
            URLQueryItem(name: "mode", value: "1"),
            URLQueryItem(name: "room_type", value: "0"),
            URLQueryItem(name: "jumpfrom", value: "0"),
            URLQueryItem(name: "reply_mid", value: replyMID),
            URLQueryItem(name: "reply_attr", value: "0"),
            URLQueryItem(name: "reply_uname", value: ""),
            URLQueryItem(name: "replay_dmid", value: ""),
            URLQueryItem(name: "statistics", value: #"{"appId":100,"platform":5}"#),
            URLQueryItem(name: "reply_type", value: "0"),
            URLQueryItem(name: "fontsize", value: "25"),
            URLQueryItem(name: "rnd", value: String(Int(now().timeIntervalSince1970))),
            URLQueryItem(name: "roomid", value: sanitizedRoomID),
            URLQueryItem(name: "csrf", value: credential.csrf),
            URLQueryItem(name: "csrf_token", value: credential.csrf),
        ]
        request.httpBody = components.percentEncodedQuery?.data(using: .utf8)

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw AccountError.requestFailed("发送弹幕 HTTP 失败")
        }

        let decoded = try JSONDecoder().decode(BilibiliSendDanmuResponse.self, from: data)
        guard decoded.code == 0 else {
            throw AccountError.requestFailed(decoded.message ?? decoded.msg ?? "发送弹幕失败")
        }
    }
}

private struct BilibiliSendDanmuResponse: Decodable {
    let code: Int
    let message: String?
    let msg: String?
}

private struct BilibiliNavResponse: Decodable {
    let code: Int
    let data: BilibiliNavProfile?
}

private struct BilibiliNavProfile: Decodable {
    let isLogin: Bool?
    let uname: String?
    let mid: Int64?
}

enum BilibiliAccountCredentialParser {
    static func credential(from cookies: [HTTPCookie]) -> BilibiliAccountCredential? {
        let platformCookies = cookies.filter { cookie in
            let domain = cookie.domain.lowercased()
            return domain.hasSuffix("bilibili.com") || domain.hasSuffix("biliapi.net")
        }
        var valuesByName: [String: String] = [:]
        for cookie in platformCookies {
            valuesByName[cookie.name] = cookie.value
        }
        guard let session = valuesByName["SESSDATA"]?.nonEmptyTrimmed,
              let csrf = valuesByName["bili_jct"]?.nonEmptyTrimmed else {
            return nil
        }

        valuesByName["SESSDATA"] = session
        valuesByName["bili_jct"] = csrf
        let cookieHeader = valuesByName
            .sorted { $0.key < $1.key }
            .map { "\($0.key)=\($0.value)" }
            .joined(separator: "; ")
        return BilibiliAccountCredential(cookieHeader: cookieHeader, csrf: csrf)
    }
}

private extension String {
    var nonEmptyTrimmed: String? {
        let value = trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
