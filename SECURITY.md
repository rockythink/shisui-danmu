# 安全策略

## 支持范围

安全更新只覆盖当前默认分支和最新发布版本。

## 私下报告

请发送邮件到 apps@elazer.wang，主题注明“DANMU Security”。报告应包含受影响版本、复现步骤、影响范围和可安全共享的日志。

请勿在公开 Issue 中提交 B 站 Cookie、CSRF、API Key、OAuth Token、含id_token_hint的授权URL、assistant-secrets文件、直播账号、Keychain内容、Pay/Cloudflare凭据或本机会话日志。当前内置HTTP/官方订阅的私有凭据是权限保护的明文，不是公开复现材料；请用隔离假凭据并脱敏画面/请求/Journal。我们会确认收到报告，并在初步评估后回复处置计划。
