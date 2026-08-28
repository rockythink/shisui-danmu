import Foundation

enum TerminalTheme: String, CaseIterable, Sendable {
    case chatroom
    case pure
    case simple
    case info
    case paper
    case porcelain
    case sage
    case blush
    case shisui

    init(argument: String) {
        switch argument.lowercased() {
        case "1", "chatroom": self = .chatroom
        case "2", "pure": self = .pure
        case "3", "simple": self = .simple
        case "4", "info": self = .info
        case "5", "paper", "light": self = .paper
        case "6", "porcelain", "snow": self = .porcelain
        case "7", "sage", "green": self = .sage
        case "8", "blush", "pink": self = .blush
        default: self = .shisui
        }
    }

    var palette: TerminalPalette {
        switch self {
        case .shisui:
            TerminalPalette(
                time: "#8492A6",
                name: "#69D6C1",
                content: "#E8EEF2",
                frame: "#356B62",
                info: "#69D6C1",
                rank: "#DDB76A",
                background: "#10191D"
            )
        case .chatroom:
            TerminalPalette(
                time: "#7F8EA3",
                name: "#65BFFF",
                content: "#E6EDF7",
                frame: "#315A7D",
                info: "#62D6B5",
                rank: "#FFD166",
                background: "#101722"
            )
        case .pure:
            TerminalPalette(
                time: "#70798C",
                name: "#C4A7E7",
                content: "#E7E5F2",
                frame: "#403A5F",
                info: "#9CCFD8",
                rank: "#F6C177",
                background: "#171522"
            )
        case .simple:
            TerminalPalette(
                time: "#998B7A",
                name: "#D9A066",
                content: "#EEE8DE",
                frame: "#654C38",
                info: "#E0B477",
                rank: "#F2CC8F",
                background: "#1D1814"
            )
        case .info:
            TerminalPalette(
                time: "#8A87A8",
                name: "#57C7FF",
                content: "#EEE9FF",
                frame: "#7057B8",
                info: "#72F1B8",
                rank: "#FAD000",
                background: "#1B1528"
            )
        case .paper:
            TerminalPalette(
                time: "#6B7068",
                name: "#245F8A",
                content: "#252922",
                frame: "#A8B19F",
                info: "#286B59",
                rank: "#805618",
                background: "#F4F1E8",
                broadcaster: "#176B4D",
                warning: "#A0442A",
                inputFrame: "#3D7C6C"
            )
        case .porcelain:
            TerminalPalette(
                time: "#68747C",
                name: "#285E80",
                content: "#202A30",
                frame: "#A6B8C1",
                info: "#20706E",
                rank: "#7B5B18",
                background: "#EEF3F5",
                broadcaster: "#08765D",
                warning: "#A04444",
                inputFrame: "#4F8495"
            )
        case .sage:
            TerminalPalette(
                time: "#6F796A",
                name: "#386A78",
                content: "#273025",
                frame: "#A8B7A1",
                info: "#3C7152",
                rank: "#80611D",
                background: "#EDF3E9",
                broadcaster: "#176C63",
                warning: "#A14B32",
                inputFrame: "#62816B"
            )
        case .blush:
            TerminalPalette(
                time: "#806F73",
                name: "#76547D",
                content: "#33272A",
                frame: "#C4AEB3",
                info: "#39716B",
                rank: "#8A5C24",
                background: "#F8EEEE",
                broadcaster: "#176E67",
                warning: "#A23E4F",
                inputFrame: "#9B6875"
            )
        }
    }
}

struct TerminalPalette: Sendable {
    var time = "#8B98A8"
    var name = "#77D5FF"
    var content = "#E6EDF3"
    var frame = "#34506B"
    var info = "#79E2B6"
    var rank = "#FFD166"
    var background = "NONE"
    var broadcaster = "#79E2B6"
    var warning = "#FF9F6E"
    var inputFrame = "#77D5FF"
}

struct TerminalConfiguration: Sendable {
    var roomID = ""
    var theme: TerminalTheme = .shisui {
        didSet { palette = theme.palette }
    }
    var singleLine = true
    var chatLayout = false
    var showTime = true
    var showUsername = true
    var palette = TerminalTheme.shisui.palette

    static func load(arguments: [String] = Array(CommandLine.arguments.dropFirst())) throws -> Self {
        var configPath = defaultConfigPath
        if let index = arguments.firstIndex(where: { $0 == "-c" || $0 == "--config" }),
           arguments.indices.contains(index + 1) {
            configPath = NSString(string: arguments[index + 1]).expandingTildeInPath
        }

        var result = Self()
        if FileManager.default.fileExists(atPath: configPath) {
            result.apply(values: parseConfig(at: configPath))
        }
        result.apply(arguments: arguments)

        guard let room = Int(result.roomID.trimmingCharacters(in: .whitespacesAndNewlines)), room > 0 else {
            throw ConfigurationError.missingRoom
        }
        result.roomID = String(room)
        return result
    }

    static var usage: String {
        """
        用法: danmu <房间号> [选项]
              danmu --room <房间号> [选项]
              danmu --login
              danmu --logout
          -r, --room <id>             B 站房间号
          -t, --theme <name|1...8>    shisui/chatroom/pure/simple/info/paper/porcelain/sage/blush
          -l, --single-line <0|1>     正文首行是否与时间、用户名同行
          -s, --show-time <0|1>       显示时间
          --show-name <0|1>           显示用户名
          --hide-name                 隐藏消息、提问和榜单中的用户名
          -c, --config <path>         配置文件路径
          --configure-obs             安全配置 OBS WebSocket、密码、场景和麦克风
          --login                     打开最小 B 站账号授权窗口
          --logout                    清除 TUI 自己的 B 站登录态
          --time-color <#RRGGBB>      时间颜色
          --name-color <#RRGGBB>      用户名颜色
          --content-color <#RRGGBB>   内容颜色
          --frame-color <#RRGGBB>     边框颜色
          --info-color <#RRGGBB>      房间信息颜色
          --rank-color <#RRGGBB>      榜单颜色
          -h, --help                  显示帮助
          -v, --version               显示版本
        """
    }

    enum ConfigurationError: LocalizedError {
        case missingRoom

        var errorDescription: String? { "缺少有效房间号\n\n\(TerminalConfiguration.usage)" }
    }

    private static var defaultConfigPath: String {
        NSString(string: "~/.config/shisui-danmu/config.toml").expandingTildeInPath
    }

    private static func parseConfig(at path: String) -> [String: String] {
        guard let text = try? String(contentsOfFile: path, encoding: .utf8) else { return [:] }
        var values: [String: String] = [:]
        for rawLine in text.components(separatedBy: .newlines) {
            let line = rawLine.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !line.hasPrefix("#"), let separator = line.firstIndex(of: "=") else { continue }
            let key = line[..<separator].trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            let value = line[line.index(after: separator)...]
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
            values[key] = value
        }
        return values
    }

    private mutating func apply(values: [String: String]) {
        if let value = values["roomid"] ?? values["room_id"] { roomID = value }
        if let value = values["theme"] { theme = TerminalTheme(argument: value) }
        if let value = values["singleline"] ?? values["single_line"] { singleLine = Self.bool(value) }
        if let value = values["chatlayout"] ?? values["chat_layout"] { chatLayout = Self.bool(value) }
        if let value = values["showtime"] ?? values["show_time"] { showTime = Self.bool(value) }
        if let value = values["showname"] ?? values["show_name"] { showUsername = Self.bool(value) }
        if let value = values["hidename"] ?? values["hide_name"], Self.bool(value) { showUsername = false }
        if let value = values["timecolor"] ?? values["time_color"] { palette.time = Self.color(value, fallback: palette.time) }
        if let value = values["namecolor"] ?? values["name_color"] { palette.name = Self.color(value, fallback: palette.name) }
        if let value = values["contentcolor"] ?? values["content_color"] { palette.content = Self.color(value, fallback: palette.content) }
        if let value = values["framecolor"] ?? values["frame_color"] { palette.frame = Self.color(value, fallback: palette.frame) }
        if let value = values["infocolor"] ?? values["info_color"] { palette.info = Self.color(value, fallback: palette.info) }
        if let value = values["rankcolor"] ?? values["rank_color"] { palette.rank = Self.color(value, fallback: palette.rank) }
        if let value = values["background"] { palette.background = value }
    }

    private mutating func apply(arguments: [String]) {
        var index = 0
        while index < arguments.count {
            let argument = arguments[index]
            func next() -> String? {
                guard arguments.indices.contains(index + 1) else { return nil }
                return arguments[index + 1]
            }
            switch argument {
            case "-r", "--room": if let value = next() { roomID = value; index += 1 }
            case "-t", "--theme": if let value = next() { theme = TerminalTheme(argument: value); index += 1 }
            case "-l", "--single-line": if let value = next() { singleLine = Self.bool(value); index += 1 }
            case "-s", "--show-time": if let value = next() { showTime = Self.bool(value); index += 1 }
            case "--show-name": if let value = next() { showUsername = Self.bool(value); index += 1 }
            case "--hide-name": showUsername = false
            case "--time-color": if let value = next() { palette.time = Self.color(value, fallback: palette.time); index += 1 }
            case "--name-color": if let value = next() { palette.name = Self.color(value, fallback: palette.name); index += 1 }
            case "--content-color": if let value = next() { palette.content = Self.color(value, fallback: palette.content); index += 1 }
            case "--frame-color": if let value = next() { palette.frame = Self.color(value, fallback: palette.frame); index += 1 }
            case "--info-color": if let value = next() { palette.info = Self.color(value, fallback: palette.info); index += 1 }
            case "--rank-color": if let value = next() { palette.rank = Self.color(value, fallback: palette.rank); index += 1 }
            case "-c", "--config": index += 1
            default:
                if !argument.hasPrefix("-") && roomID.isEmpty { roomID = argument }
            }
            index += 1
        }
    }

    private static func bool(_ value: String) -> Bool {
        !["0", "false", "no", "off"].contains(value.lowercased())
    }

    private static func color(_ value: String, fallback: String) -> String {
        let normalized = value.hasPrefix("#") ? value : "#\(value)"
        guard normalized.count == 7,
              normalized.dropFirst().allSatisfy({ $0.isHexDigit }) else { return fallback }
        return normalized.uppercased()
    }
}
