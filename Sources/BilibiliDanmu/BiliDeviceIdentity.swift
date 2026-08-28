import Foundation

struct BiliDeviceIdentity: Equatable, Sendable {
    let buvid3: String
    let buvid4: String

    var cookieHeader: String {
        "buvid3=\(buvid3); buvid4=\(buvid4)"
    }
}

typealias BiliDeviceIdentityFetcher = @Sendable () async -> BiliDeviceIdentity?

actor BiliDeviceIdentityProvider {
    static let shared = BiliDeviceIdentityProvider()

    private let session: URLSession
    private var cachedIdentity: BiliDeviceIdentity?

    init(session: URLSession? = nil) {
        if let session {
            self.session = session
        } else {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.timeoutIntervalForRequest = 12
            configuration.timeoutIntervalForResource = 12
            configuration.httpCookieStorage = nil
            configuration.httpShouldSetCookies = false
            configuration.urlCredentialStorage = nil
            self.session = URLSession(configuration: configuration)
        }
    }

    func identity() async -> BiliDeviceIdentity? {
        if let cachedIdentity { return cachedIdentity }
        guard let identity = try? await fetchIdentity() else { return nil }
        cachedIdentity = identity
        return identity
    }

    private func fetchIdentity() async throws -> BiliDeviceIdentity {
        let url = URL(string: "https://api.bilibili.com/x/frontend/finger/spi")!
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 12)
        request.setValue(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/17.0 Safari/605.1.15",
            forHTTPHeaderField: "User-Agent"
        )
        request.setValue("application/json,text/plain,*/*", forHTTPHeaderField: "Accept")

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw BiliDeviceIdentityError.requestFailed
        }
        let decoded = try JSONDecoder().decode(BiliDeviceIdentityResponse.self, from: data)
        guard
            decoded.code == 0,
            let payload = decoded.data,
            !payload.buvid3.isEmpty,
            !payload.buvid4.isEmpty
        else {
            throw BiliDeviceIdentityError.requestFailed
        }
        return BiliDeviceIdentity(buvid3: payload.buvid3, buvid4: payload.buvid4)
    }
}

private struct BiliDeviceIdentityResponse: Decodable {
    let code: Int
    let data: Payload?

    struct Payload: Decodable {
        let buvid3: String
        let buvid4: String

        enum CodingKeys: String, CodingKey {
            case buvid3 = "b_3"
            case buvid4 = "b_4"
        }
    }
}

private enum BiliDeviceIdentityError: Error {
    case requestFailed
}
