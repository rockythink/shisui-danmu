# 有限 OBS 控制位于独立输出 Adapter

日期：2026-07-20

## 决策

Rust `obs` Adapter 通过 `obws` 直接连接 OBS WebSocket v5。GUI 与 TUI 保持相同的用户可见控制契约，但不共享实现：状态检查、读取并切换 OBS 现有场景、指定麦克风静音和 OBS 推流启停。TUI 额外订阅所配置麦克风的音量事件，只在输入框顶栏显示单路电平；弹幕台不定义场景体系或音频混音体系。

`domain` 与 `bilibili` 不依赖 OBS。OBS 操作不改变弹幕连接、会话日志、录制、Replay Buffer、来源或画布。

## 原因

音量事件约每 50 毫秒到达，需要复用长连接并在进程内聚合；为每次读取启动外部进程会增加延迟、部署依赖和故障面。原生 Adapter 继续隔离平台协议，同时去除 Python 与额外 CLI 安装要求。

## 安全约束

- 仅通过类型化 OBS WebSocket 请求通信，不启动 shell 或外部 OBS 控制进程。
- 密码写入 DANMU 独立的 `obs-password` 文件，Unix 权限固定为 `0600`，不进入系统凭据存储、非敏感配置、命令参数、日志或 Journal；`OBS_API_PASSWORD` 可临时覆盖；`danmu --configure-obs` 和 TUI 的 `/obs config password` 均使用隐藏输入。
- 连接在进程内复用并串行创建；修改操作完成后重新读取 OBS 真相确认。
- 高频音量事件聚合为 10 Hz；电平限制在 -60 至 0 dBFS，采用快速上升、平滑回落。
- 停止推流必须二次确认；TUI 使用 `/obs stop` 后再输入 `/obs confirm`。
- 开播不自动取消麦克风静音。

## 依赖与许可证

`obws` 0.15 以 MIT 许可证链接进本项目，用于 OBS WebSocket v5 请求和事件订阅。

## 后果

不提供完整 OBS 控制台、Studio Mode 编排、场景/来源编辑、音频混音、RTMP/编码设置、录制或 Replay Buffer 控制。麦克风电平只反映用户配置的单个 OBS 输入，并在静音、断连和窄终端条件下退化为静态状态或隐藏。
