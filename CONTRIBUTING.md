# 参与贡献

感谢你改进 DANMU。

## 范围

本仓库只接受以下范围的修改：

- `src/domain`：平台无关领域行为；
- `src/bilibili`：B 站平台 Adapter；
- `src/obs.rs`：有限 OBS 遥控 Adapter；
- `src/terminal`：终端渲染与输入交互；
- `src/config.rs`、`src/persistence.rs`、`src/storage.rs`：配置与本地数据；
- `src/runner`、`src/bridge`、`src/workspace.rs`、`src/history.rs`：内置 HTTP 助手、本地记忆与审核发送契约；
- 与上述 Module 直接相关的测试、文档和发行脚本。

商业 GUI、播放器、完整直播画布、音频混音台和 OBS 场景编辑器不属于本仓库。

## 开发环境

- Rust 1.89 或更高版本；
- macOS、Linux 或 Windows；
- `rustfmt` 与 `clippy` 组件。

当前未发布内置助手不需要 coding CLI、Pi、Node 或 ACP 适配器；官网构建单独要求 Node.js 24。OpenAI兼容/Anthropic用私有Key，ChatGPT订阅走官方OSS/local Sign in with ChatGPT preview。外部 coding Agent 的五操作 CLI/MCP 和配置生成器仍保留。最新正式 v0.5.1 的原生说明及旧证据仅属历史，不能据此跳过新HTTP链路验证。

## 提交修改

1. 从 `main` 创建短生命周期分支。
2. 保持平台协议字段在 Adapter 内，不要让领域 Module 依赖 B 站原始 payload。
3. 为新增或改变的可观察行为补充测试。
4. 运行完整门禁：

   ```bash
   ./script/verify.sh
   ```

5. Pull Request 说明用户可见变化、验证方式和可能影响的本地数据或凭据边界。

不要提交 Cookie、CSRF、API Key、OAuth Token、授权URL（可能含id_token_hint）、assistant-secrets、系统凭据、真实账号或本机会话日志。两种模型凭据均本机权限保护的明文，不是加密保险箱；证据打包排除凭据文件，画面/日志/脱敏请求也须无秘密。安全问题按[SECURITY.md](SECURITY.md)私下报告。

## 助手修改的验证与数据边界

- 复用唯一Runner/Bridge发送链；完整envelope内容错误整批拒绝继续新消息，外层HTTP/协议/额度/身份/存储错误安全暂停撤权，不加重试、模型回退或API付费fallback。资料/历史/真实确认段不丢、不以候选冒充已发。
- format4按严格legacy schema只在内存迁移，首次显式保存先逐字assistant.pre-api.json备份；冲突/损坏/失败不覆盖，不静默写回、扫描原生规则或恢复旧自动/续用/搜索。非敏感观察投影不得携带基址/凭据/凭据路径，更不得恢复授权。
- 连接/Key/OAuth/联网变更共享暂停/无flight/无清退/无操作/无发送许可门禁，TUI另拒待续用。Key协议+规范化基址唯一作用域、OAuth注册隔离；Unix权限/链接/父目录/打开后fstat校验不得削弱。
- 自动验证用tempdir、假Key、literal loopback服务及隔离HOME，不读用户认证；覆盖双协议HTTP、冻结公开问题Exa、OAuth绑定/刷新/取消、Responses完整SSE终态、额度失败与无付费回退。受限搜索Unavailable候选强制待审；真实官方授权、实际额度/模型/公网搜索和平台发送须本人当前明确授权，不用旧许可。
- 实际TUI闭环复用script/agent_smoke.py，按当前接口验证秘密输入、审核/取消、完整资料记忆、CLI/MCP互斥。运行统一门禁后安装并比较Release/安装文件SHA-256；只有实际执行才报告通过。LocalTransport confirmed不是B站实发，loopback不是模型效果或订阅额度验收。检查项与历史记录见[接入向导](docs/agent-onboarding.md)。


## 官网更新日志

每次正式发版，同步维护 `website/src/content/docs/changelog.md`，并发布官网：

- 新版本排在最前，记录 UTC 发布日期、用户可见变化、重要升级提醒，以及对应 GitHub Release 和代码变更链接。
- 更新页首的最新版本提示；仅有 Git 标签的记录须单独标注，不将提交日期冒充发布日期，不将未发布改动写入正式版本。
- 早期版本缺少发布说明时，注明整理依据，不把版本已有能力全部当作该版新增。
- 在 `website/` 运行 `npm run build`，检查版本目录、首页与手册入口、手机阅读和站内搜索，再部署官网。

日志随官网源码维护，不会自动从 GitHub Releases 同步。

## License

提交贡献即表示你有权按本仓库的 [Mozilla Public License 2.0](LICENSE) 提供该贡献。项目名称、Logo 和其他品牌标识仍受 [TRADEMARKS.md](TRADEMARKS.md) 约束。
