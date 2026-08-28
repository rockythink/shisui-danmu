# 有限 OBS 控制位于独立输出 Adapter

日期：2026-07-20

## 决策

新增独立 `OBSControl` Module，通过用户单独安装的 `pschmitt/obs-cli` 控制 OBS WebSocket v5。GUI 与 TUI 共用同一契约，只提供状态检查、读取并切换 OBS 现有场景、指定麦克风静音和 OBS 推流启停；弹幕台不定义场景体系。

`DanmuCore` 与 `BilibiliDanmu` 不依赖 OBS。OBS 操作不改变弹幕连接、会话日志、录制、Replay Buffer、来源或画布。

## 原因

外部 CLI 可以用较小成本验证知识主播需要的直播遥控闭环，并保留以后替换为原生 OBS WebSocket Adapter 的边界。共享 Module 避免 GUI/TUI 分别拼接命令和维护不同状态。

## 安全约束

- 命令通过 `Foundation.Process` 参数数组执行，不经过 shell。
- 密码保存在 Keychain 或由当前进程环境临时提供，不进入命令参数、命令历史、日志和 Journal；GUI 设置页和 TUI `danmu --configure-obs` 提供完整配置入口，运行中的 TUI 通过 `/obs config password` 安全更新密码。
- 修改命令跨进程串行，并在执行后重新读取 OBS 真相确认。
- 停止推流必须由前端二次确认；TUI 使用 `/obs stop` 后再输入 `/obs confirm`。
- GUI/TUI 每次启动检测 CLI；只有用户明确同意后才通过 `uv tool install obs-cli` 安装，拒绝不影响核心弹幕功能。
- 开播不自动取消麦克风静音。

## 依赖与许可证

`obs-cli` 是 GPL-3.0 的可选外部程序，由用户单独安装和运行；本项目不捆绑其源码、Python 运行时或依赖。若未来随 App 分发，必须重新评估许可证和发布方式。

## 后果

第一版不提供完整 OBS 控制台、Studio Mode 编排、场景/来源编辑、音频电平、RTMP/编码设置、录制或 Replay Buffer 控制。需要事件订阅、准确推流时长或免 Python 安装时，可在保持 `OBSControlling` 契约的前提下替换实现。
