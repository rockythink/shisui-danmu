---
name: shisui-danmu-safe-delivery
description: "用于 shisui-danmu 项目中的 shisui-danmu 安全修改与交付。先核对当前源码、项目规则和最新用户指令；历史验收与授权不自动恢复。"
---

> 导入来源：`/Users/elazer/.omp/agent/memories/--Users-elazer-Desktop-Workspace-90_Dev-拾穗弹幕台--/skills/shisui-danmu-safe-delivery/SKILL.md`。导入日期：2026-10-02。当前项目规则与最新用户指令优先；历史版本、候选、授权和验收状态必须重新核验。

# shisui-danmu 安全修改与交付

## 适用范围

用于免费 Rust TUI `/Users/elazer/Desktop/Workspace/90_Dev/danmu` 的代码修改、缺陷修复和本机安装。商业 Swift GUI 不适用，除非用户另行明确授权。

## 1. 确认目标和当前规则

- 运行 `git rev-parse --show-toplevel`，确认没有误入 sibling `拾穗弹幕台`。
- 阅读 `docs/agent-onboarding.md` 首节及当前任务对应小节。该文档和当前代码优先于历史记忆。
- 查看工作树和相关文件，保留用户及其他人的改动。
- 不使用 reset、clean、stash、切分支或 worktree 来回避现有改动。

## 2. 收窄修改边界

- 复用已有业务路径、命令解析和事件循环，不创建平行实现。
- 对助手、身份、存储或发送路径的修复，要明确哪些错误应局部恢复、哪些错误必须继续传播。
- 不以删除数据库、历史归档或凭据作为修复手段。
- 真实平台发送、直播控制、OBS 停播和生产写入需要当前任务的明确授权；优先使用隔离目录、合成事件和本地受控服务。

## 3. 建立回归证据

- 尽量先加入能在旧行为上失败、在修复后通过的最小回归。
- 同时覆盖直接入口和间接入口，例如命令输入、命令搜索、键盘和鼠标共享分发。
- 对 TUI 退出类故障，测试生产事件循环边界，而不只测试内部函数。
- 对持久化故障，额外证明真实写错误仍会传播，避免局部错误处理吞掉磁盘故障。

## 4. 完整验证

从仓库根执行 onboarding 当前要求。历史标准入口为：

`RUST_TEST_THREADS=1 ./script/verify.sh`

不要只运行针对性测试后就声明完成。记录实际测试数、Clippy、格式检查、Release 构建和 CLI 检查结果，但不要复用旧轮次数字。

## 5. 安装并证明安装一致

验证通过后执行：

`./script/install_cli.sh`

然后比较：

`shasum -a 256 target/release/danmu "$HOME/.local/bin/danmu"`

两个哈希必须一致。若平台命令不同，使用等价 SHA-256 工具。只检查版本号或时间戳不足以证明安装的是本轮构建。

## 6. 交付报告

报告应包括：

- 实际根因和修复边界；
- 明确未修复或不保证的事项；
- 当前轮次实际验证结果；
- Release 与安装文件一致性；
- 是否使用真实平台、是否发送消息、是否触碰直播实例；
- 提醒用户自行完全退出并重启所有旧 TUI 窗口。

不要关闭用户终端、Ghostty、OBS 或直播，不要把“安装成功”描述成运行中实例已经升级。
