import Foundation

public struct OBSConfiguration: Codable, Equatable, Sendable {
    public var host: String
    public var port: Int
    public var executablePath: String?
    public var microphoneInputName: String
    public var defaultLiveScene: String

    public init(
        host: String = "localhost",
        port: Int = 4455,
        executablePath: String? = nil,
        microphoneInputName: String = "Mic/Aux",
        defaultLiveScene: String = ""
    ) {
        self.host = host
        self.port = port
        self.executablePath = executablePath
        self.microphoneInputName = microphoneInputName
        self.defaultLiveScene = defaultLiveScene
    }
}

public enum OBSConnectionState: String, Codable, Sendable {
    case unknown
    case connected
    case unavailable
}

public enum OBSStreamState: String, Codable, Sendable {
    case unknown
    case stopped
    case starting
    case live
    case stopping
}

public enum OBSMicrophoneState: String, Codable, Sendable {
    case unknown
    case unmuted
    case muted
}

public struct OBSStatus: Equatable, Sendable {
    public var connection: OBSConnectionState
    public var currentScene: String?
    public var stream: OBSStreamState
    public var microphone: OBSMicrophoneState
    public var lastError: String?
    public var compatibilityWarning: String?
    public var refreshedAt: Date

    public init(
        connection: OBSConnectionState = .unknown,
        currentScene: String? = nil,
        stream: OBSStreamState = .unknown,
        microphone: OBSMicrophoneState = .unknown,
        lastError: String? = nil,
        compatibilityWarning: String? = nil,
        refreshedAt: Date = .now
    ) {
        self.connection = connection
        self.currentScene = currentScene
        self.stream = stream
        self.microphone = microphone
        self.lastError = lastError
        self.compatibilityWarning = compatibilityWarning
        self.refreshedAt = refreshedAt
    }

    public static let unknown = OBSStatus()
}

public enum OBSControlError: LocalizedError, Equatable, Sendable {
    case cliNotInstalled
    case obsUnavailable(String)
    case authenticationFailed
    case incompatibleVersion(String)
    case sceneNotFound(String)
    case inputNotFound(String)
    case commandTimedOut
    case commandFailed(exitCode: Int32, detail: String)
    case confirmationFailed(String)
    case configurationMissing(String)
    case busy
    case invalidResponse(String)

    public var errorDescription: String? {
        switch self {
        case .cliNotInstalled:
            "未找到 obs-cli。请先运行：uv tool install obs-cli"
        case .obsUnavailable(let detail):
            detail.isEmpty ? "无法连接 OBS WebSocket。" : "无法连接 OBS：\(detail)"
        case .authenticationFailed:
            "OBS WebSocket 认证失败，请检查密码。"
        case .incompatibleVersion(let version):
            "obs-cli 版本 \(version) 可能不兼容，当前支持 0.9.x。"
        case .sceneNotFound(let name):
            "OBS 中不存在场景“\(name)”。"
        case .inputNotFound(let name):
            "OBS 中不存在输入“\(name)”。"
        case .commandTimedOut:
            "OBS 命令超时。"
        case .commandFailed(_, let detail):
            detail.isEmpty ? "OBS 命令执行失败。" : "OBS 命令失败：\(detail)"
        case .confirmationFailed(let detail):
            detail
        case .configurationMissing(let field):
            "OBS 配置缺少：\(field)"
        case .busy:
            "OBS 正被另一前端操作，请稍后再试。"
        case .invalidResponse(let detail):
            "无法解析 OBS 返回结果：\(detail)"
        }
    }
}

public protocol OBSControlling: Sendable {
    func fetchStatus() async throws -> OBSStatus
    func listScenes() async throws -> [String]
    func listInputs() async throws -> [String]
    func switchScene(to name: String) async throws
    func setMicrophoneMuted(_ muted: Bool, input: String) async throws
    func startStreaming(defaultScene: String?) async throws
    func stopStreaming() async throws
}
