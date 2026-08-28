import Foundation

enum TerminalStoragePaths {
    static let namespace = "cc.ss-data.ShisuiDanmuTerminal"

    static var supportDirectory: URL {
        applicationSupportBase
            .appendingPathComponent(namespace, isDirectory: true)
    }

    static var accountSessionURL: URL {
        supportDirectory
            .appendingPathComponent("BilibiliAccount", isDirectory: true)
            .appendingPathComponent("session.json", isDirectory: false)
    }

    static var loginDiagnosticLogURL: URL {
        supportDirectory.appendingPathComponent("login-diagnostic.log", isDirectory: false)
    }

    static var obsConfigurationURL: URL {
        supportDirectory.appendingPathComponent("obs-control.json", isDirectory: false)
    }

    static var obsLockPath: String {
        supportDirectory.appendingPathComponent("obs-control.lock", isDirectory: false).path
    }

    static let obsKeychainService = "cc.ss-data.ShisuiDanmuTerminal.obs-websocket"

    private static var applicationSupportBase: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
    }
}
