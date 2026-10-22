# Third-party notices

## jsonwebtoken

- Repository: <https://github.com/Keats/jsonwebtoken>
- Locked version: 11.1.0 (RustCrypto backend; default features disabled).
- License: MIT.
- Usage: validate official Sign in with ChatGPT ID/access-token signatures, issuer, audience and expiry.
- Token storage and application authorization remain application-owned.

## rusqlite / libsqlite3-sys

- Repository: <https://github.com/rusqlite/rusqlite>
- Locked versions: rusqlite 0.37.0, libsqlite3-sys 0.35.0.
- Rust binding license: MIT. Bundled SQLite implements the room-scoped persistent FTS5 history index.

## toml_edit

- Repository: <https://github.com/toml-rs/toml>
- Locked version: 0.23.10+spec-1.0.0.
- License: MIT OR Apache-2.0.
- Usage: update TUI preferences without discarding user TOML comments or unrelated sections.

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
