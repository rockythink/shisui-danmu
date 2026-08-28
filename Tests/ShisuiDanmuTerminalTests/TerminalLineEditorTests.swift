import Testing
@testable import ShisuiDanmuTerminal

@Suite("Terminal line editor")
struct TerminalLineEditorTests {
    @Test func decoderWaitsForSplitEscapeSequences() {
        var decoder = TerminalInputDecoder()

        #expect(decoder.feed([0x1B]).isEmpty)
        #expect(decoder.feed([0x5B]).isEmpty)
        #expect(decoder.feed([0x44]) == [.left])
    }

    @Test func bareEscapeUsesShortDisambiguationDelay() {
        var decoder = TerminalInputDecoder()
        let start = ContinuousClock.now

        #expect(decoder.feed([0x1B], now: start).isEmpty)
        #expect(decoder.feed([], now: start.advanced(by: .milliseconds(20))).isEmpty)
        #expect(decoder.feed([], now: start.advanced(by: .milliseconds(50))) == [.escape])
    }

    @Test func f9ShortcutsDecodeForMicrophoneControl() {
        var decoder = TerminalInputDecoder()

        #expect(decoder.feed(Array("\u{001B}[20~".utf8)) == [.toggleMicrophone])
        #expect(decoder.feed(Array("\u{001B}[20;2~".utf8)) == [.muteMicrophone])
    }
    @Test func sgrMouseReportsAreIgnored() {
        var decoder = TerminalInputDecoder()
        #expect(decoder.feed(Array("\u{001B}[<64;10;5M".utf8)).isEmpty)
    }

    @Test func alternateScreenDisablesWheelToArrowTranslation() {
        #expect(TerminalApplicationModes.enter.contains("\u{001B}[?1007l"))
        #expect(!TerminalApplicationModes.enter.contains("\u{001B}[?1007h"))
        #expect(TerminalApplicationModes.exit.contains("\u{001B}[?1007h"))
    }

    @Test func bracketedPasteIsOneInputAndNormalizesNewlines() {
        var decoder = TerminalInputDecoder()
        let first = Array("\u{001B}[200~第一行\n".utf8)
        let second = Array("第二行\u{001B}[201~".utf8)

        #expect(decoder.feed(first).isEmpty)
        #expect(decoder.feed(second) == [.paste("第一行 第二行")])
    }

    @Test func editorSupportsCursorInsertionDeletionAndWordMovement() {
        var editor = TerminalLineEditor()
        editor.insert("数据直播")
        editor.moveLeft()
        editor.moveLeft()
        editor.insert("领域")
        #expect(editor.snapshot.text == "数据领域直播")
        #expect(editor.snapshot.cursor == 4)

        editor.moveWordLeft()
        editor.deleteForward()
        #expect(editor.snapshot.text == "据领域直播")
        editor.moveEnd()
        editor.backspace()
        #expect(editor.snapshot.text == "据领域直")
    }

    @Test func editorDropsInvisibleIMEFormattingAroundPunctuation() {
        var editor = TerminalLineEditor()
        editor.insert("看定金，\u{200B}\u{2060}\u{FEFF}")

        #expect(editor.snapshot.text == "看定金，")
        #expect(editor.snapshot.cursor == 4)
        editor.backspace()
        #expect(editor.snapshot.text == "看定金")
    }
}
