import Foundation
import Security

public struct OBSConfigurationStore: Sendable {
    public let fileURL: URL

    public init(fileURL: URL) {
        self.fileURL = fileURL
    }

    public func load() throws -> OBSConfiguration {
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return OBSConfiguration() }
        let data = try Data(contentsOf: fileURL)
        return try JSONDecoder().decode(OBSConfiguration.self, from: data)
    }

    public func save(_ configuration: OBSConfiguration) throws {
        guard (1...65_535).contains(configuration.port) else {
            throw OBSControlError.configurationMissing("有效端口")
        }
        let directory = fileURL.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(configuration).write(to: fileURL, options: .atomic)
    }

}

public struct OBSKeychainPasswordStore: Sendable {
    public let service: String

    public init(service: String) {
        self.service = service
    }

    public func read(host: String, port: Int) throws -> String? {
        var query = baseQuery(host: host, port: port)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw keychainError(status)
        }
        return String(data: data, encoding: .utf8)
    }

    public func save(_ password: String, host: String, port: Int) throws {
        let query = baseQuery(host: host, port: port)
        let data = Data(password.utf8)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var item = query
            item[kSecValueData as String] = data
            let addStatus = SecItemAdd(item as CFDictionary, nil)
            guard addStatus == errSecSuccess else { throw keychainError(addStatus) }
        } else if status != errSecSuccess {
            throw keychainError(status)
        }
    }

    public func delete(host: String, port: Int) throws {
        let status = SecItemDelete(baseQuery(host: host, port: port) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw keychainError(status) }
    }

    private func baseQuery(host: String, port: Int) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: "\(host.lowercased()):\(port)"
        ]
    }

    private func keychainError(_ status: OSStatus) -> OBSControlError {
        let message = SecCopyErrorMessageString(status, nil) as String? ?? "Keychain 错误 \(status)"
        return .commandFailed(exitCode: status, detail: message)
    }
}
