import Testing
@testable import ShisuiDanmuTerminal

@Suite("Terminal configuration")
struct TerminalConfigurationTests {
    @Test func commandLineSupportsUpstreamThemeNumbersAndDisplayFlags() throws {
        let configuration = try TerminalConfiguration.load(arguments: [
            "--room", "9527",
            "--theme", "4",
            "--single-line", "0",
            "--show-time", "0",
            "--name-color", "#12ABEF",
        ])

        #expect(configuration.roomID == "9527")
        #expect(configuration.theme == .info)
        #expect(configuration.singleLine == false)
        #expect(configuration.showTime == false)
        #expect(configuration.palette.name == "#12ABEF")
    }

    @Test func commandLineCanHideUsernames() throws {
        let hidden = try TerminalConfiguration.load(arguments: ["1", "--hide-name"])
        let explicit = try TerminalConfiguration.load(arguments: ["1", "--show-name", "0"])

        #expect(hidden.showUsername == false)
        #expect(explicit.showUsername == false)
    }

    @Test func positionalRoomAndNamedThemeAreAccepted() throws {
        let configuration = try TerminalConfiguration.load(arguments: ["233", "--theme", "pure"])

        #expect(configuration.roomID == "233")
        #expect(configuration.theme == .pure)
        #expect(configuration.chatLayout == false)
    }

    @Test func paperAndLightAliasSelectTheLightPalette() throws {
        let paper = try TerminalConfiguration.load(arguments: ["1", "--theme", "paper"])
        let alias = try TerminalConfiguration.load(arguments: ["1", "--theme", "light"])

        #expect(paper.theme == .paper)
        #expect(alias.theme == .paper)
        #expect(paper.palette.background == "#F4F1E8")
        #expect(paper.palette.content == "#252922")
        #expect(paper.palette.broadcaster == "#176B4D")
    }

    @Test func additionalLightThemesHaveReadableDistinctPalettes() throws {
        let porcelain = try TerminalConfiguration.load(arguments: ["1", "--theme", "porcelain"])
        let sage = try TerminalConfiguration.load(arguments: ["1", "--theme", "sage"])
        let blush = try TerminalConfiguration.load(arguments: ["1", "--theme", "blush"])

        #expect(porcelain.theme == .porcelain)
        #expect(sage.theme == .sage)
        #expect(blush.theme == .blush)
        #expect([porcelain, sage, blush].allSatisfy { $0.palette.background != "NONE" })
        #expect(Set([porcelain, sage, blush].map(\.palette.background)).count == 3)
    }

    @Test func everyThemeOwnsADistinctPalette() {
        let palettes = TerminalTheme.allCases.map(\.palette)

        #expect(Set(palettes.map(\.background)).count == TerminalTheme.allCases.count)
        #expect(Set(palettes.map(\.frame)).count == TerminalTheme.allCases.count)
    }

    @Test func changingThemeAppliesItsPalette() throws {
        var configuration = try TerminalConfiguration.load(arguments: ["1", "--theme", "shisui"])

        configuration.theme = .info

        #expect(configuration.palette.background == TerminalTheme.info.palette.background)
        #expect(configuration.palette.frame == TerminalTheme.info.palette.frame)
    }

    @Test func invalidRoomIsRejected() {
        #expect(throws: TerminalConfiguration.ConfigurationError.self) {
            try TerminalConfiguration.load(arguments: ["--room", "not-a-room"])
        }
    }
}
