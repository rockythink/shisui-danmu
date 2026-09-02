# 拾穗弹幕台 TUI

[![Rust CI](https://github.com/rockythink/shisui-danmu/actions/workflows/ci.yml/badge.svg)](https://github.com/rockythink/shisui-danmu/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/rockythink/shisui-danmu)](https://github.com/rockythink/shisui-danmu/releases/latest)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-blue.svg)](LICENSE)

面向知识型主播的免费开源弹幕与直播监控工作台。它在终端里监看 B 站直播互动、发送弹幕、记录会话，并提供有限的 OBS 遥控。

本仓库是独立的 Rust TUI，只包含终端产品、领域 Module 和平台 Adapter；商业 macOS GUI 位于私有仓库，不是本项目的依赖。

## 系统要求

- macOS 14+：Apple Silicon、Intel
- Linux：x86_64、aarch64
- Windows 10/11：x86_64
- 公开监看不需要登录；发送弹幕需要扫码登录 B 站账号
- OBS 遥控与麦克风电平需要 OBS 28+ 内置的 WebSocket v5；无需安装额外 CLI

## 安装

### 下载预编译 Release

macOS、Linux 及 Windows Git Bash 可使用安装脚本。脚本按操作系统和处理器选择 Release、验证 SHA-256，然后安装到 `~/.local/bin/danmu`：

```bash
curl -fsSL https://raw.githubusercontent.com/rockythink/shisui-danmu/main/script/install_release.sh | bash
```

指定版本或安装目录：

```bash
curl -fsSL https://raw.githubusercontent.com/rockythink/shisui-danmu/main/script/install_release.sh | \
  DANMU_VERSION=v0.4.0 DANMU_INSTALL_DIR="$HOME/bin" bash
```

Windows 也可以从 [Releases](https://github.com/rockythink/shisui-danmu/releases/latest) 下载 `shisui-danmu-windows-x86_64.zip`，校验同名 `.sha256` 后将 `danmu.exe` 放入 `PATH`。

Release 资产：

| 系统 | 资产 |
| --- | --- |
| macOS Apple Silicon | `shisui-danmu-macos-aarch64.tar.gz` |
| macOS Intel | `shisui-danmu-macos-x86_64.tar.gz` |
| Linux x86_64 | `shisui-danmu-linux-x86_64.tar.gz` |
| Linux aarch64 | `shisui-danmu-linux-aarch64.tar.gz` |
| Windows x86_64 | `shisui-danmu-windows-x86_64.zip` |

### 从源码安装

需要 Rust 1.89 或更高版本：

```bash
git clone https://github.com/rockythink/shisui-danmu.git
cd shisui-danmu
./script/install_cli.sh
```

也可以直接使用 Cargo：

```bash
cargo install --locked --path .
```

## 使用

```bash
danmu <房间号>
danmu --room <房间号>
danmu --login
danmu --logout
danmu --configure-obs
```

运行 `danmu --help` 查看房间、布局、显示和配置参数。TUI 内输入 `/` 打开命令面板，使用 `↑/↓` 选择，按 `Enter` 直接执行；只有需要继续填写参数时才使用 `Tab` 补全。输入 `/help` 查看账号、显示、主题和 OBS 命令。

主题只改变界面配色，不改变布局或功能。内置 `shisui`、`catppuccin-mocha`、`tokyo-night`、`gruvbox-dark` 四套深色主题：
输入 `/theme` 会直接显示主题列表；当前主题排在第一项，选择后按 `Enter` 即时切换。

```text
/theme
/theme tokyo-night
/theme reload
```

首次正常启动会生成平台配置目录下的 `shisui-danmu/themes.json`；macOS 路径为 `~/Library/Application Support/shisui-danmu/themes.json`，`/theme` 会显示当前实际路径。复制现有主题对象、修改主题 ID 和 `label`，再用 `#RRGGBB` 配置 `time`、`name`、`content`、`frame`、`info`、`rank`、`background`、`success`、`host`、`warning` 十个必填语义色，即可增加自定义主题。保存后输入 `/theme reload` 热加载；`/theme <主题名>` 会立即切换并将选择写回 JSON。临时试用可在启动时传入 `--theme <主题名>`，不会改写已选主题。

长弹幕按 20 个 Unicode 字素分段，段间隔一秒；任一段失败会停止后续发送。输入框顶栏会通过 OBS WebSocket 事件显示所配置麦克风的电平：响度上升立即响应、回落平滑，静音时固定显示 `MIC MUTE`，OBS 未连接或终端空间不足时自动隐藏。账号、OBS 配置、系统凭据存储与会话日志使用 TUI 独立命名空间，不读取商业 GUI 的私有凭据。底栏以纯符号显示数据：◉ 为 `WATCHED_CHANGE.data.num` 累计看过，♥ 为 `LIKE_INFO_V3_UPDATE.data.click_count` 累计点赞，▤ 为本次运行收到的弹幕数，● 为登录后通过 `getOnlineRank.data.onlineNum` 读取的当前在线人数；未登录时显示 `--`，绝不使用 `getRoomBaseInfo.online` 人气值冒充人数。

## 卸载

```bash
curl -fsSL https://raw.githubusercontent.com/rockythink/shisui-danmu/main/script/install_release.sh | bash -s -- --uninstall
```

卸载只删除可执行文件，不删除本地配置、系统凭据存储或会话日志。

## 能力边界

免费 TUI 包含公开房间监看、历史与实时事件去重、登录后发送弹幕、分类型互动标记、重点互动、会话归档、可配置高对比 True Color 主题、有限 OBS 控制及单路麦克风电平监看。

本项目不包含播放器、完整直播画布、音频混音台、OBS 场景编辑器，也不在领域 Module 中直接依赖 B 站协议字段。完整功能基线见 [终端舞台功能基线](docs/terminal-feature-matrix.md)。

## 开发与验证

```bash
./script/verify.sh
```

门禁包含 `rustfmt`、严格 `clippy`、全目标测试、Release 构建和 CLI 冒烟。GitHub Actions 在 macOS、Linux、Windows 上执行同等 Rust 门禁。

提交修改前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。

## License 与商标

源代码按 [Mozilla Public License 2.0](LICENSE) 发布。第三方组件见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

MPL-2.0 不授予“拾穗弹幕台”名称、Logo、App 图标或其他品牌资产的商标许可。详见 [TRADEMARKS.md](TRADEMARKS.md)。

## 安全问题

请按 [SECURITY.md](SECURITY.md) 私下报告可能泄露 Cookie、CSRF、系统凭据存储内容或本地会话数据的问题。
