// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "ShisuiDanmu",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "DanmuCore", targets: ["DanmuCore"]),
        .library(name: "BilibiliDanmu", targets: ["BilibiliDanmu"]),
        .library(name: "OBSControl", targets: ["OBSControl"]),
        .executable(name: "ShisuiDanmuTerminal", targets: ["ShisuiDanmuTerminal"])
    ],
    targets: [
        .target(name: "DanmuCore"),
        .target(name: "BilibiliDanmu", dependencies: ["DanmuCore"]),
        .target(name: "OBSControl", linkerSettings: [.linkedFramework("Security")]),
        .executableTarget(
            name: "ShisuiDanmuTerminal",
            dependencies: ["DanmuCore", "BilibiliDanmu", "OBSControl"]
        ),
        .testTarget(name: "DanmuCoreTests", dependencies: ["DanmuCore"]),
        .testTarget(name: "BilibiliDanmuTests", dependencies: ["BilibiliDanmu", "DanmuCore"]),
        .testTarget(name: "OBSControlTests", dependencies: ["OBSControl"]),
        .testTarget(
            name: "ShisuiDanmuTerminalTests",
            dependencies: ["ShisuiDanmuTerminal", "DanmuCore", "BilibiliDanmu"]
        )
    ]
)
