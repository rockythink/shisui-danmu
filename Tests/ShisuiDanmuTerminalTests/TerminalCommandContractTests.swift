import Foundation
import Testing
@testable import ShisuiDanmuTerminal

@Suite("Terminal command and storage contract")
struct TerminalCommandContractTests {
    @Test func usageListsAccountAndOBSCommands() {
        let usage = TerminalConfiguration.usage

        #expect(usage.contains("danmu --login"))
        #expect(usage.contains("danmu --logout"))
        #expect(usage.contains("--configure-obs"))
        #expect(usage.contains("danmu <房间号>"))
    }

    @Test func terminalStorageUsesDedicatedNamespace() {
        #expect(TerminalStoragePaths.namespace == "cc.ss-data.ShisuiDanmuTerminal")
        #expect(TerminalStoragePaths.namespace != "cc.ss-data.ShisuiDanmuDesk")
        #expect(TerminalStoragePaths.accountSessionURL.path.hasPrefix(TerminalStoragePaths.supportDirectory.path))
        #expect(TerminalStoragePaths.accountSessionURL.path.hasSuffix("BilibiliAccount/session.json"))
        #expect(TerminalStoragePaths.obsConfigurationURL.lastPathComponent == "obs-control.json")
        #expect(TerminalStoragePaths.obsLockPath.hasSuffix("obs-control.lock"))
        #expect(TerminalStoragePaths.obsKeychainService == "cc.ss-data.ShisuiDanmuTerminal.obs-websocket")
    }
}
