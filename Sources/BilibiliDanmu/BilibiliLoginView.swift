import AppKit
import SwiftUI
import WebKit

public struct BilibiliLoginView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private let accountClient: BilibiliAccountClient
    private let onStatusChange: @MainActor (BilibiliAccountStatus) -> Void

    @State private var captureToken = UUID()
    @State private var message = "在 B 站页面完成登录后，点击“保存登录态”"
    @State private var isSaving = false

    public init(
        accountClient: BilibiliAccountClient,
        onStatusChange: @escaping @MainActor (BilibiliAccountStatus) -> Void
    ) {
        self.accountClient = accountClient
        self.onStatusChange = onStatusChange
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 14) {
                Image(systemName: "person.badge.shield.checkmark.fill")
                    .font(.system(size: 19, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 38, height: 38)
                    .background(Color.accentColor.opacity(0.10), in: RoundedRectangle(cornerRadius: 10))

                VStack(alignment: .leading, spacing: 4) {
                    Text("B 站账号授权")
                        .font(.headline)
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .id(message)
                        .transition(.opacity)
                }

                Spacer()

                if isSaving {
                    ProgressView()
                        .controlSize(.small)
                        .transition(.opacity)
                }
                Button {
                    captureToken = UUID()
                } label: {
                    Label("保存登录态", systemImage: "checkmark.shield.fill")
                }
                .buttonStyle(.borderedProminent)
                .disabled(isSaving)
            }
            .padding(14)
            .background(.thinMaterial)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: message)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: isSaving)

            BilibiliLoginWebView(captureToken: captureToken) { cookies in
                persist(cookies: cookies)
            }

            HStack(spacing: 7) {
                Image(systemName: "lock.shield.fill")
                    .foregroundStyle(.green)
                Text("登录态只保存在本机私有目录，不进入弹幕事件、日志或导出数据。")
                    .foregroundStyle(.secondary)
                Spacer()
                Text("公开监看始终无需登录")
                    .foregroundStyle(.tertiary)
            }
            .font(.caption2)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .background(.thinMaterial)
        }
        .frame(minWidth: 920, minHeight: 680)
        .background {
            NonRestorableWindowConfigurator()
                .frame(width: 0, height: 0)
        }
    }

    private func persist(cookies: [HTTPCookie]) {
        isSaving = true
        message = "正在验证登录态…"
        Task { @MainActor in
            do {
                let status = try await accountClient.saveLoginCookies(cookies)
                onStatusChange(status)
                switch status {
                case .signedOut:
                    message = "没有拿到有效登录态，请确认已在页面中登录"
                case .signedIn(let displayName, _):
                    message = "已保存并验证·\(displayName)"
                }
            } catch {
                message = error.localizedDescription
            }
            isSaving = false
        }
    }
}

private struct NonRestorableWindowConfigurator: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        NonRestorableConfigurationView()
    }

    func updateNSView(_ nsView: NSView, context: Context) {}

    private final class NonRestorableConfigurationView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            window?.isRestorable = false
        }
    }
}

private struct BilibiliLoginWebView: NSViewRepresentable {
    let captureToken: UUID
    let onCookies: ([HTTPCookie]) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(lastCaptureToken: captureToken, onCookies: onCookies)
    }

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = true
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.allowsBackForwardNavigationGestures = true
        webView.load(URLRequest(url: URL(string: "https://passport.bilibili.com/login")!))
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        context.coordinator.onCookies = onCookies
        guard context.coordinator.lastCaptureToken != captureToken else {
            return
        }
        context.coordinator.lastCaptureToken = captureToken
        webView.configuration.websiteDataStore.httpCookieStore.getAllCookies { cookies in
            DispatchQueue.main.async {
                context.coordinator.onCookies(cookies)
            }
        }
    }

    final class Coordinator {
        var lastCaptureToken: UUID?
        var onCookies: ([HTTPCookie]) -> Void

        init(lastCaptureToken: UUID, onCookies: @escaping ([HTTPCookie]) -> Void) {
            self.lastCaptureToken = lastCaptureToken
            self.onCookies = onCookies
        }
    }
}
