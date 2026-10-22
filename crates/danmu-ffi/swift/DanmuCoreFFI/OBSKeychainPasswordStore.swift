// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Adapted from shisui-danmu-kit/Sources/OBSControl/OBSConfigurationStore.swift.

import Foundation
import Security

/// The runtime invokes this foreign trait on its worker, never the Swift main actor.
/// It retains no runtime reference and never falls back to a plaintext file.
public final class OBSKeychainPasswordStore: DesktopObsPasswordStore, Sendable {
    public let service: String
    public let allowEnvironmentOverride: Bool

    /// QA must use its own service and leave the environment override disabled.
    public init(
        service: String = "cc.ss-data.ShisuiDanmuDesk.obs-websocket",
        allowEnvironmentOverride: Bool = false
    ) {
        self.service = service
        self.allowEnvironmentOverride = allowEnvironmentOverride
    }

    public func read(host: String, port: UInt16) throws -> String? {
        if allowEnvironmentOverride,
           let password = ProcessInfo.processInfo.environment["OBS_API_PASSWORD"],
           !password.isEmpty {
            return password
        }
        var query = baseQuery(host: host, port: port)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw keychainError(status) }
        guard let data = result as? Data, let password = String(data: data, encoding: .utf8) else {
            throw CoreError.Storage(message: "OBS Keychain password has invalid encoding")
        }
        return password
    }

    public func write(host: String, port: UInt16, password: String) throws {
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

    private func baseQuery(host: String, port: UInt16) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: "\(host.lowercased()):\(port)"
        ]
    }

    private func keychainError(_ status: OSStatus) -> CoreError {
        // Do not expose host, service, account, password, or OS diagnostic text over FFI.
        .Storage(message: "OBS Keychain access failed (OSStatus \(status))")
    }
}
