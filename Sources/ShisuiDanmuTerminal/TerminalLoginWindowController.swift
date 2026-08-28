import AppKit
import BilibiliDanmu
import SwiftUI

@MainActor
final class TerminalLoginWindowController: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private let accountClient: BilibiliAccountClient
    private var window: NSWindow?

    init(accountClient: BilibiliAccountClient) {
        self.accountClient = accountClient
    }

    func run() {
        TerminalLoginAppLauncher.recordDiagnostic("helper app loop starting")
        let application = NSApplication.shared
        application.setActivationPolicy(.regular)
        application.delegate = self
        application.finishLaunching()

        let content = BilibiliLoginView(accountClient: accountClient) { [weak self] status in
            guard case .signedIn = status else { return }
            TerminalLoginAppLauncher.recordDiagnostic("helper login completed")
            self?.finish()
        }
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "拾穗弹幕台 TUI · B 站账号授权"
        window.contentView = NSHostingView(rootView: content)
        window.collectionBehavior.insert(.canJoinAllSpaces)
        window.center()
        window.delegate = self
        window.isReleasedWhenClosed = false
        self.window = window

        window.makeKeyAndOrderFront(nil)
        window.orderFrontRegardless()
        application.activate(ignoringOtherApps: true)
        TerminalLoginAppLauncher.recordDiagnostic("helper window shown")
        application.run()
        TerminalLoginAppLauncher.recordDiagnostic("helper app loop stopped")
        application.delegate = nil
    }

    func windowWillClose(_ notification: Notification) {
        TerminalLoginAppLauncher.recordDiagnostic("helper window closing")
        stopApplicationLoop()
    }

    private func finish() {
        window?.close()
    }

    private func stopApplicationLoop() {
        let application = NSApplication.shared
        application.stop(nil)
        let event = NSEvent.otherEvent(
            with: .applicationDefined,
            location: .zero,
            modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: 0,
            context: nil,
            subtype: 0,
            data1: 0,
            data2: 0
        )
        if let event {
            application.postEvent(event, atStart: false)
        }
    }
}
