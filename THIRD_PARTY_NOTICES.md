# Third-party notices

## obws

- Repository: <https://forge.dnaka91.rocks/dnaka91/obws>
- License: MIT
- Usage: native OBS WebSocket v5 control and event subscriptions.

## tui-input

- Repository: <https://github.com/sayanarijit/tui-input>
- License: MIT
- Usage: Unicode-safe terminal input editing state and operations.

## bililive_dm

- Repository: <https://github.com/copyliu/bililive_dm>
- Referenced commit: `38172aa0b859b8b9deb77ca2aa8d52329a17d10d`
- License: WTFPL
- Referenced files: `BilibiliDM_PluginFramework/DanmakuModel.cs`, `BiliDMLibCore/Model.cs`
- Reused material: public-room command names and JSON fixture shapes, including the
  `SUPER_CHAT_MESSAGE_JP` / `SUPER_CHAT_MESSAGE_JPN` aliases.
- Implementation: fixtures were rewritten as Rust tests and the parser/deduplication
  behavior was independently implemented in Rust.

No DLL plugin host, WPF state, open-platform identity code, third-party signing
service, `EndianBitConverter`, or submodule source is copied into this project.
