# 平台协议隐藏在 Adapter Seam 后

日期：2026-07-20

## 决策

`DanmuCore` 只认识标准平台事件。B 站房间发现、长连接、命令解析、登录态和发送弹幕放在 B 站 Adapter 的 Implementation 中。

## Interface

当前只保留 B 站 Adapter 自身的最小 Interface：接收房间标识并产生有序事件流；连接状态与可恢复错误必须显式暴露。事件不得携带 cookie、token 或其他登录凭证。

## 原因

该 Module 通过小 Interface 隐藏平台协议复杂度，为 App 提供更高 Leverage，并让协议变化集中在一个 Adapter，保持修改和测试的 Locality。

## 后果

- 不为尚未接入的平台提前创建 Adapter，也不提前抽象跨平台事件源 Interface；出现第二个真实 Adapter 后再决定共同 seam。
- B 站原始业务字段只有在明确用途且脱敏后才能作为可选数据保留。
- 领域测试通过标准事件验证，不依赖真实网络。
