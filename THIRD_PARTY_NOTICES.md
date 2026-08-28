# Third-party notices

## obs-cli（可选外部程序）

- Repository: <https://github.com/pschmitt/obs-cli>
- License: GPL-3.0
- Usage: users may install it separately to enable limited OBS WebSocket v5 controls.
- Distribution: this project does not bundle, link, copy, or redistribute `obs-cli`, its Python runtime, or its dependencies.

## bililive_dm

- Repository: <https://github.com/copyliu/bililive_dm>
- Referenced commit: `38172aa0b859b8b9deb77ca2aa8d52329a17d10d`
- License: WTFPL
- Referenced files: `BilibiliDM_PluginFramework/DanmakuModel.cs`, `BiliDMLibCore/Model.cs`
- Reused material: public-room command names and JSON fixture shapes, including the
  `SUPER_CHAT_MESSAGE_JP` / `SUPER_CHAT_MESSAGE_JPN` aliases.
- Implementation: fixtures were rewritten as Swift tests and the parser/deduplication
  behavior was independently implemented in Swift.

No DLL plugin host, WPF state, open-platform identity code, third-party signing
service, `EndianBitConverter`, or submodule source is copied into this project.
