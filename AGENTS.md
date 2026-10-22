# shisui-danmu 项目协作入口

<!-- omp-chatgpt-migration:begin -->
## 本地 App 上下文入口

- 在 ChatGPT App 的本地工作任务中使用本项目目录；默认中文回复。
- 本项目已迁移 Skill 位于 `.agents/skills/`：`danmu-assistant-failure-isolation`, `shisui-danmu-safe-delivery`。匹配任务先读对应 SKILL.md；迁移本身不要求执行 Skill 工作流。
- 当前代码、测试、项目既有规则和最新用户指令优先。当前未发布内置助手直接使用双 API Key 协议或官方 ChatGPT 订阅 preview，不再依赖 Pi/OMP/ACP；外部 coding Agent 的 CLI/MCP 接入仍独立保留。上下文迁移不授权提交、推送、发布或生产操作。
- 历史 `local://`、`skill://` 或 OMP 工具名不保证在本会话可用；Skill 可直接读本地文件，其他能力先核对真实接口，不伪造调用。
<!-- omp-chatgpt-migration:end -->
