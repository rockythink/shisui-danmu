import Foundation
import Testing
@testable import BilibiliDanmu

@Suite("Bilibili reconnect policy")
struct BiliReconnectPolicyTests {
    @Test func delayProgressionCapsAtLastConfiguredValue() {
        let policy = BiliReconnectPolicy(delays: [1, 2, 4])

        #expect(policy.delay(forAttempt: 1) == 1)
        #expect(policy.delay(forAttempt: 2) == 2)
        #expect(policy.delay(forAttempt: 3) == 4)
        #expect(policy.delay(forAttempt: 8) == 4)
    }
}
