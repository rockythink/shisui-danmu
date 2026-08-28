# 拾穗弹幕台 TUI

[![macOS CI](https://github.com/rockythink/shisui-danmu/actions/workflows/ci.yml/badge.svg)](https://github.com/rockythink/shisui-danmu/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/rockythink/shisui-danmu)](https://github.com/rockythink/shisui-danmu/releases/latest)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-blue.svg)](LICENSE)

面向知识型主播的免费开源弹幕与提问工作台。它在终端里监看 B 站直播互动、整理提问队列、记录会话，并提供有限的 OBS 遥控。

本仓库只包含 TUI、平台无关领域模块和平台 Adapter，不包含、安装或启动商业 GUI。

## 系统要求

- macOS 14 或更高版本
- Apple Silicon 或 Intel Mac
- 公开监看不需要登录；发送弹幕需要先完成 B 站账号授权
- OBS 遥控需要 OBS WebSocket 和可用的 `obs-cli`

## 安装

### 安装预编译版本

安装脚本会下载 GitHub 最新 Release 的 Universal 二进制、校验 SHA-256，并安装到 `~/.local/bin/danmu`：

```bash
curl -fsSL https://raw.githubusercontent.com/rockythink/shisui-danmu/main/script/install_release.sh | bash
```

也可以先下载并检查脚本，再执行：

```bash
curl -fsSLO https://raw.githubusercontent.com/rockythink/shisui-danmu/main/script/install_release.sh
less install_release.sh
bash install_release.sh
rm install_release.sh
```

通过 `DANMU_INSTALL_DIR` 指定其他用户可写目录，通过 `DANMU_VERSION` 安装指定版本：

```bash
DANMU_INSTALL_DIR="$HOME/bin" DANMU_VERSION=v0.3.1 bash install_release.sh
```

如果安装目录不在 `PATH` 中，安装脚本会输出需要加入 `~/.zshrc` 的配置。

### 从源码安装

源码安装需要 Swift 6 工具链：

```bash
git clone https://github.com/rockythink/shisui-danmu.git
cd shisui-danmu
./script/install_cli.sh
```

## 使用

```bash
danmu <房间号>
danmu --room <房间号> --theme shisui
danmu --login
danmu --logout
danmu --configure-obs
```

运行 `danmu --help` 查看主题、布局、颜色和配置参数。TUI 的账号、OBS 配置、Keychain 密码与会话日志使用独立命名空间，不读取商业 GUI 的私有凭据。

## 卸载

```bash
curl -fsSL https://raw.githubusercontent.com/rockythink/shisui-danmu/main/script/install_release.sh | bash -s -- --uninstall
```

卸载只删除 `danmu` 可执行文件，不删除本地配置、Keychain 凭据或会话日志。

## 能力边界

免费 TUI 包含公开房间监看、登录后发送弹幕、提问状态流、重点互动、会话日志、多套 True Color 主题和有限 OBS 控制。

本项目不包含播放器、完整直播画布、音频电平、OBS 场景编辑器，也不在领域模块中直接依赖 B 站协议字段。完整功能基线见 [终端舞台功能基线](docs/terminal-feature-matrix.md)。

## 开发与验证

```bash
./script/verify.sh
```

该命令运行完整测试、构建 Release TUI、验证 CLI 帮助契约，并检查公开 Package 没有混入商业 GUI。

提交修改前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。

## License 与商标

源代码按 [Mozilla Public License 2.0](LICENSE) 发布。第三方组件见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

MPL-2.0 不授予“拾穗弹幕台”名称、Logo、App 图标或其他品牌资产的商标许可。详见 [TRADEMARKS.md](TRADEMARKS.md)。

## 安全问题

请按 [SECURITY.md](SECURITY.md) 私下报告可能泄露 Cookie、CSRF、Keychain 凭据或本地会话数据的问题。
