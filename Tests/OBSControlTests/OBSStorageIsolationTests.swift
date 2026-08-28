import Foundation
import OBSControl
import Testing

@Suite("OBS injected storage isolation", .serialized)
struct OBSStorageIsolationTests {
    @Test func configurationStoresDoNotShareFiles() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("OBSStorageIsolation-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let terminalURL = root.appendingPathComponent("terminal/obs-control.json")
        let deskURL = root.appendingPathComponent("desk/obs-control.json")
        let terminalStore = OBSConfigurationStore(fileURL: terminalURL)
        let deskStore = OBSConfigurationStore(fileURL: deskURL)

        try terminalStore.save(OBSConfiguration(host: "terminal.example"))

        #expect(try terminalStore.load().host == "terminal.example")
        #expect(try deskStore.load() == OBSConfiguration())
        #expect(!FileManager.default.fileExists(atPath: deskURL.path))
    }

    @Test func keychainServicesDoNotReadEachOther() throws {
        let suffix = UUID().uuidString
        let first = OBSKeychainPasswordStore(service: "cc.ss-data.test.terminal.\(suffix)")
        let second = OBSKeychainPasswordStore(service: "cc.ss-data.test.desk.\(suffix)")
        let host = "localhost"
        let port = 44_55
        defer {
            try? first.delete(host: host, port: port)
            try? second.delete(host: host, port: port)
        }

        try first.save("terminal-secret", host: host, port: port)

        #expect(try first.read(host: host, port: port) == "terminal-secret")
        try first.save("updated-secret", host: host, port: port)
        #expect(try first.read(host: host, port: port) == "updated-secret")
        #expect(try second.read(host: host, port: port) == nil)
    }
}
