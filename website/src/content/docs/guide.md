---
title: DANMU 使用手册
description: DANMU v0.5.0 中文使用手册，涵盖安装、直播间、OBS、AI助手、审核、隐私与故障排查。
head:
  - tag: script
    attrs:
      type: application/ld+json
    content: |
      {"@context":"https://schema.org","@type":"TechArticle","headline":"DANMU 使用手册","description":"DANMU v0.5.0 中文使用手册，涵盖安装、直播间、OBS、AI助手、审核、隐私与故障排查。","url":"https://danmu.elazer.wang/guide/"}
---

本手册适用于 **DANMU v0.5.0**。DANMU 是面向知识型主播的免费开源 B 站弹幕与直播监控工作台；当前只接入 B 站，不是播放器，也不替代 OBS。程序免费开源不代表外部 AI 模型免费：模型订阅、额度与联网搜索成本由所选宿主或服务商决定。

在线版本：<https://danmu.elazer.wang/guide/>　源码与下载：<https://github.com/rockythink/shisui-danmu>

首次使用可按章节顺序阅读；遇到异常直接前往[故障排查](#故障排查)，查找完整参数时前往[命令参考与进一步阅读](#命令参考与进一步阅读)。

## 安装与升级

### 选择安装方式

**前置条件：** 选择一种符合本机环境的渠道。所有渠道安装后的可执行命令都叫 `danmu`。

| 渠道 | 安装命令或入口 | 前置条件 |
| --- | --- | --- |
| npm | `npm install -g danmu-tui` | Node.js 18+ |
| Bun | `bun install -g danmu-tui` | Bun |
| Homebrew | `brew install rockythink/tap/danmu` | macOS 或 Linux |
| Cargo | `cargo install shisui-danmu --locked` | Rust 1.89+ |
| 安装脚本 | `curl -fsSL https://raw.githubusercontent.com/rockythink/shisui-danmu/main/script/install_release.sh \| bash` | macOS、Linux 或 Windows Git Bash |
| 预编译包 | [GitHub Releases](https://github.com/rockythink/shisui-danmu/releases/latest) | 按系统和 CPU 选择压缩包，并核对同名 `.sha256` |

npm 与 Bun 安装的是 Rust 原生程序。无须全局安装时，也可运行 `npx danmu-tui <房间号>` 或 `bunx danmu-tui <房间号>`。Windows 可下载 `shisui-danmu-windows-x86_64.zip`，校验摘要后把 `danmu.exe` 放入 `PATH`。

**成功表现：**

```bash
danmu --version
```

输出包含 `0.5.0`。若找不到命令，重新打开终端并检查安装目录是否在 `PATH`；若摘要不匹配，不要运行文件，重新从 [Release](https://github.com/rockythink/shisui-danmu/releases/latest) 下载。

### 升级

沿用原安装渠道：npm/Bun 重新全局安装，Homebrew 先 `brew update` 再 `brew upgrade danmu`，Cargo 重新运行安装命令，安装脚本或预编译包则重新下载最新 Release。升级后再次运行 `danmu --version`。请自行退出并重启所有旧的 DANMU 实例；新二进制不会替你停止正在直播的终端或 OBS。

DANMU 的基础 TUI 提供 macOS arm64、macOS x86_64、Linux arm64、Linux x86_64、Windows x86_64 五种构建。内置 ACP Runner 当前用于 Unix 环境；不要把 Windows 基础 TUI 的可用性理解为 Windows 已支持内置 AI Runner。

## 第一次使用

### 进入直播间

**前置条件：** 准备 B 站房间号；只看公开弹幕不需要登录。

```bash
danmu <房间号>
# 等价的显式形式
danmu --room <房间号>
```

启动自检会检查本地数据、直播间与可选的 OBS。检查失败时按 `Enter` 重试、按 `S` 明确跳过可跳过项，或按 `Ctrl+C` 退出。进入主界面并看到直播间标题或状态即表示成功。若提示房间不可用，先核对房间号和网络，不要靠删除历史绕过检查。

### 登录、发送与退出登录

**前置条件：** 只有发送弹幕、修改直播间信息等操作需要账号。

```bash
danmu --login
```

用哔哩哔哩客户端扫码。这登录的是 **DANMU 弹幕台账号**，不是浏览器账号；成功后重新进入直播间。底部输入文本，按 `Enter` 才发送；粘贴只插入草稿，不执行斜杠命令，也不自动发送。

发送标记中，`✓` 表示确认送达，`?` 表示送达尚未确认，`×` 表示发送被拒。`?` 不能当成失败自动重发，否则可能重复发送。退出 DANMU 自己保存的主账号：

```bash
danmu --logout
```

失败时按通知区的实际提示处理；登录失效应重新扫码，不要共享或手工复制 Cookie。

## 界面与快捷键

主界面顶部显示直播状态、标题、开播时长、主播身份、OBS 与麦克风状态；中部是历史和实时消息流；底部是通知、发送状态和输入框。重启后从当前房间恢复的历史以低对比度 `↶` 标记，只供回看。助手运行时，消息旁的 AI/处理标识表示处理状态，不等于已经发送；实际送达仍以发送结果为准。

| 操作 | 按键 | 成功表现或注意事项 |
| --- | --- | --- |
| 搜索全部指令 | `Ctrl+O` | 打开中文指令搜索；退出后保留草稿与光标 |
| 切换信息流/聊天布局 | `Tab` | 切换并保存布局；保存失败时不应用 |
| 浏览历史 | 滚轮或 `↑/↓` | 每次移动一条；新消息仍继续接收 |
| 选择回复对象 | `Shift+↑/↓` | 高亮目标消息 |
| 插入 `@用户名` | 选择后 `Enter` | 只插入输入框，仍须再次确认发送 |
| 返回实时 | 浏览或选择时 `Esc`/`End` | 恢复最新消息跟随 |
| 行尾 | 正常输入时 `End` | 没有浏览/选择状态时只移动输入光标到行尾 |
| 打开助手运行菜单 | `Ctrl+G` | 只打开面板，不自动启动模型 |
| 紧急暂停助手 | `Ctrl+P` | 暂停生成并撤销发送许可，保留候选和草稿 |
| 退出程序 | `Ctrl+C` | 不停止 OBS 推流 |

主界面可用鼠标原生拖选文字，再使用终端自己的复制快捷键；这不会进入复制模式，也不会改动草稿。菜单统一使用方向键、`Enter`、`Esc`，`F1` 或 `?` 展开当前项说明。若按键没有作用，先退出当前弹窗或编辑器，再看底部提示所处状态。

## 设置与日常操作

输入 `/settings` 打开“设置与操作”。五类长期入口依次为：**外观与显示、B站账号与直播间、OBS、AI助手、系统与关于**；同页还提供本场 AI、重点消息、关键词搜索归档、指令搜索和退出。方向键或 `Tab` 选择项目，`Enter` 执行，`Esc` 逐层返回，`F1` 或 `?` 查看说明。

- `/settings reading` 直达“外观与显示”：选择主题、列表/聊天布局、昵称、时间，以及浏览历史空闲多少秒后回到最新。`history_idle_seconds` 为 `0` 时只手动返回。
- `/display theme` 选择长期主题；`/display names on`、`/display names off`、`/display time on`、`/display time off`、`/display layout chat`、`/display layout list` 保存对应显示设置。
- `/pin` 标记当前选中消息；未选择时使用最新消息。成功后界面提示已设为重点。
- `/find [关键词]` 搜索已归档会话；省略关键词会打开编辑器，匹配结果显示房间与场次。没有结果时先核对关键词，再在 `/diag` 查看历史状态。

旧 `/history`、`/theme`、`/layout`、`/login` 等斜杠写法不是 v0.5.0 的现行教程；登录使用启动参数，长期设置使用上述菜单或当前 `/display` 命令。

## OBS与直播间管理

### 连接 OBS

**前置条件：** OBS 28+，已启用内置 WebSocket v5，并知道主机、端口、密码和要控制的麦克风输入名称。

```bash
danmu --configure-obs
```

向导会保存连接配置，密码通过隐藏编辑器输入。也可在 TUI 使用 `/settings obs`，或分别使用 `/obs config host`、`/obs config port`、`/obs config mic`、`/obs config password`。不要把真实密码写在命令参数、截图或公开文档中。

进入直播间后，`/obs status` 查看状态，`/obs connect` 检查连接。状态区 `OBS ●` 绿色表示已连接，红色表示未连接。连接失败时检查 OBS 是否运行、WebSocket 主机/端口/密码和防火墙；不要反复执行推流命令验证连接。

### 现场控制

- `/mute` 静音所配置麦克风；重复执行仍保持静音。
- `/unmute` 恢复麦克风，声音将公开。
- `/scene [名称]`：省略名称时查看场景，带名称时切换。
- `/obs start` **会真的开始推流**，只在本人确认准备完成后执行。
- `/obs stop` 打开停止确认，确认后进入 3 秒倒计时；倒计时内按 `Esc` 可取消。

操作成功以 OBS 回执和状态区变化为准。未连接、名称不匹配或权限错误时按实际错误修正配置，不要把“命令已输入”当成 OBS 已执行。

### 标题与封面

`/room title [标题]` 修改 B 站直播间标题；`/room cover [图片路径]` 上传封面。两者要求当前主账号已验证为该房间房主。平台受理封面不等于审核通过；失败时核对账号身份、房间归属、文件格式和平台返回，不要连续重复提交。

## AI助手入门

AI 由用户选择的原生宿主提供。安装、登录、订阅和模型费用遵循宿主自己的规则；DANMU 不替你自动安装宿主或登录。仅选择 AI 工具、读取模型或思考档位不会启动推理，也不会授权发送。

### 从逐条审核开始

1. 按 `Ctrl+G` 或输入 `/ai` 打开“本场 AI”；这一步只开面板。
2. 在 `/settings` →“AI助手”中依次检查“工作区”“模型”“发送策略”“人设与资料”“Agent配置”。“发送账号”会跳转到“B站账号与直播间”。
3. 选择 AI 工具，连接并同步该宿主已登录的模型与思考档位。
4. 输入 `/ai start` 或在菜单选择“启动 AI”。看到助手已连接并开始处理新弹幕后，才算启动成功。
5. 保持逐条审核，输入 `/review` 阅读候选并决定是否发送。
6. `/pause` 暂停生成并撤权；`/ai stop` 结束助手、释放 DANMU 自己启动的进程并撤权。

可用宿主适配列表为 OMP、Gemini、Claude Code、Codex、OpenCode、Pi、Amp、GitHub Copilot CLI、DeepSeek Harness。这里说的是当前适配入口，不声称九种宿主的所有真实模型都已逐一验收。准确入口、认证和限制见[九宿主接入与认证边界](https://github.com/rockythink/shisui-danmu/blob/main/docs/agent-onboarding.md#九宿主接入与认证边界)。

如果“读取模型”失败，先在宿主原生工具完成安装和认证，再重试连接；不要为了启动而静默换成另一个模型或档位。

## Pi接入示例

**前置条件：** Unix 环境、Node.js 20+、npm，以及另行安装并已认证的官方 Node 版 Pi。DANMU 不替代原生 Pi 安装和登录。

```bash
danmu setup pi
```

此命令只在 DANMU 私有位置准备核定的 `pi-acp@0.0.33` 适配器：不安装原生 Pi、不登录、不启动模型，也不加载用户全局扩展。重复执行会复用已经准备好的适配器；成功提示只代表适配器准备完成。

完整操作路径：

1. 运行 `danmu setup pi`。
2. 进入直播间，打开 `/settings` →“AI助手”→“模型”→“AI 工具”，选择 Pi。
3. 选择“读取模型”，同步 Pi 当前登录身份可见的模型与思考档位；此时仍未推理。
4. 选择明确可用的模型和档位，再回到“本场 AI”启动。
5. 保持自动发送关闭，收到候选后用 `/review` 完整审核。

Pi 的命令型凭据（如 `!command`）不受此受限接入支持；DANMU 不执行它，也不转发开发 API-key 环境变量。请使用受支持的原生认证存储。模型或思考档位不可用时明确失败，不会私自更换。若提示适配器未准备，运行 setup 并检查 Node/npm；认证或模型错误回原生 Pi 处理，不要删除 DANMU 历史。

## 审核、自动发送与账号

### 审核候选

打开 `/review` 后完整阅读正文，尤其留意长文分段和回复目标。

| 操作 | 按键 | 结果 |
| --- | --- | --- |
| 批准当前完整候选 | `Shift+Enter` | 进入串行发送队列；不等于已经送达 |
| 编辑候选 | `E` | 进入编辑；`Enter` 仅保存，保存后须重新确认 |
| 丢弃候选 | `Delete` | 不发送该候选 |
| 切换候选 | `Tab` | 查看下一条 |
| 滚动长正文 | `PgUp` / `PgDn` | 只滚动审核正文 |
| 批准当前待审批次 | `a` | 仅批准按下时已有的待审候选 |
| 丢弃当前待审批次 | `d` | 不影响在途或结果不确定的发送 |

短候选在主界面完整可见时，`Shift+Enter` 可直接批准；被截断或多段内容会先打开审核。批量批准不授权未来候选，也不会开启自动发送。

### 自动发送与账号边界

主号与独立助手号在 `/settings account` 的“B站账号与直播间”管理。独立助手号扫码、保存或校验失败时不会回退到主号。账号操作本身不启动模型、不测试发送、不授权。

首次开启自动发送必须由本人确认。已保存的自动偏好可在 **同一房间、同一发送 UID，且启动恢复检查通过** 时续用；不能理解为每次启动一定关闭，也不能理解为换房间或换身份仍有效。`/ai auto on` 申请自动发送，`/ai auto off` 立即撤销并回到逐条审核。

`Ctrl+P` 或 `/pause`、身份/房间变化、登录身份失效都会撤权。候选、编辑内容和人工草稿保留；不补发结果不确定的消息。重新登录后需按界面要求重新确认许可。

## 回复策略与调用成本

发送策略有三档基础积极性：**优先回应点名、也回答明确问题、允许主动补充**。麦克风未静音时可减少插话，但不会压住完整文字问答；DANMU 只读取麦克风状态，不检测、转写或理解主播说了什么。麦克风状态未知或过期时按保守策略处理。

普通空白、纯笑声，以及平台已识别为纯表情的消息可以跳过；有实质文本、问号或明确点名的消息保留。模型返回“零候选”与“原生轮次已结束但无回复正文”是两种不同结果：前者表示本轮选择不回复，后者表示宿主没有提供可发送正文；两者都不会重试旧消息。

首次空轮后，普通消息等待 5 秒合批；连续空轮最多延长到 10 秒。明确点名可绕过合批等待，但不能绕过单飞限制、逐条审核或自动发送授权。未知或重复的本批目标会让 **本批整体拒绝**，随后继续处理新消息，不鼓励重放旧消息。

联网搜索默认关闭；开启后只准入受控搜索，其他工具、任意文件读写和 shell 仍不因此开放。搜索和模型调用可能消耗订阅或额度。DANMU 不承诺固定省费百分比、不承诺永不漏答，也不承诺外部模型永不报错。

空轮只改变下一批普通消息的等待时间，不会替任何未来候选打开发送闸门。

## 工作区、数据与隐私

默认助手工作区是 `~/Projects/danmu-assistant/`：顶层放用户可编辑的人设和资料，`.danmu/` 放程序维护的副本与索引。私有原始 Journal 是恢复事实来源；工作区中的会话文件是维护副本，不应取代原件。配置、账号凭据、Bridge token 与实际私有数据目录以应用 `/diag` 显示为准，不要依赖手工猜测路径。

建议定期备份应用诊断所示的私有数据和自己的工作区。`/find [关键词]` 搜索已归档会话；`/diag repair` 会先备份，再重建程序派生副本，不删除原始历史，也不授予发送权限。

同一系统用户下运行不等于 OS 沙箱：宿主本身仍有它原来的系统权限。只选择可信宿主，不要把 token、账号文件或包含个人信息的历史放进人设、Skill、公开仓库或截图。外部 CLI/MCP Agent 的其他工具需在宿主侧另行限制。

## 故障排查

先输入 `/diag`；在诊断页按 `F1` 或 `?` 展开上轮摘要。不要把“等待合批”误判成停机，也不要用删除历史或重放旧消息作为通用修复。

| 现象 | 检查 | 安全处理 |
| --- | --- | --- |
| 无房间或进不去 | 房间号、网络、启动自检错误 | 更正房间号；`Enter` 重试；只对明确可跳过项按 `S` |
| 登录失败/已失效 | `/diag` 的账号状态、二维码是否过期 | 重新运行 `danmu --login`；不复制浏览器 Cookie |
| OBS 未连接 | OBS 是否运行、WebSocket v5、主机/端口/密码 | 用 `/settings obs` 修正，再 `/obs connect`；不要用开播测试连接 |
| Pi 未准备 | Node 20+、npm、setup 提示 | 运行 `danmu setup pi`；不把 setup 当作 Pi 安装或登录 |
| 模型/档位不可用 | 原生宿主登录、模型权限、同步结果 | 回宿主修复并重新读取；不静默换模型 |
| 助手未启动 | 是否只打开了 `/ai`，连接是否完成 | 明确执行 `/ai start`，查看连接错误 |
| 助手未授权 | 状态是否逐条审核、账号/房间是否变化 | 用 `/review` 逐条批准，或本人确认自动发送 |
| 有待审回复 | `/review` 中候选数和正文 | 审核后批准、编辑或丢弃；不要重复启动 |
| 等待合批 | 轮次摘要的下一等待时间 | 等待 5–10 秒；点名也仍受授权与单飞限制 |
| 零候选 | 摘要显示“零候选，仍在运行” | 等待新消息，不重试旧消息 |
| 原生无正文 | 摘要显示“原生无正文，未回答” | 检查宿主输出；等待新消息，不把它记为已回答 |
| 真正错误 | `/diag` 的错误、宿主退出或协议错误 | 修复明确原因后重连；不要把错误伪装成零候选 |
| 日志写盘失败 | 磁盘空间、权限、诊断路径 | 停止继续写入风险，修复磁盘/权限；保留现有文件 |
| 历史损坏或派生副本陈旧 | 原始 Journal 是否仍在、诊断历史告警 | 备份后运行 `/diag repair`；不要删除原始历史 |
| 本批目标未知或重复 | 摘要显示“本批目标拒绝” | 让该批拒绝并继续新消息；不重放旧批次 |

如果错误持续，记录 `/diag` 中不含凭据的摘要、DANMU 版本和复现步骤，再到 [GitHub Issues](https://github.com/rockythink/shisui-danmu/issues) 报告。不要把未知现场现象写成确定根因。

## 命令参考与进一步阅读

### 普通 CLI

```text
danmu [选项] [房间号]
```

| 形式 | 作用 |
| --- | --- |
| `danmu [房间号]` | 进入房间；公开监看不要求登录 |
| `--instance <绝对私有目录>` | 为普通 TUI 暴露该实例的 Agent/MCP 连接点；local/agent/mcp 必须显式使用，setup 不接受此参数 |
| `-r, --room <房间号>` | 显式房间号，优先于位置参数和保存配置 |
| `--replay <JSON路径>` | 无网络回放 JSON 数组；不读账号/OBS/默认配置，不连接直播 |
| `-l, --single-line <true\|false>` | 本次列表元信息同行；长正文仍可换行 |
| `-s, --show-time <true\|false>` | 本次显示或隐藏时间 |
| `--show-name <true\|false>` / `--hide-name` | 本次控制昵称；hide 优先 |
| `--history-idle-seconds <秒数>` | 本次历史空闲返回秒数；`0` 仅手动返回 |
| `-c, --config <路径>` | 使用指定 TOML 配置 |
| `--theme <主题名>` | 本次主题；长期主题在 `/settings` 修改 |
| `--login` / `--logout` | 登录或退出 DANMU 主账号，不改变浏览器登录 |
| `--configure-obs` | 交互配置 OBS，密码隐藏输入 |
| `--version` / `--help` | 查看版本或命令帮助 |

登录、退出登录与 OBS 向导建议单独执行；回放使用 `danmu --replay <JSON路径>`，不要把生产房间、账号、OBS 或显示参数混入隔离演练。

### TUI 主目录

| 命令 | 作用 |
| --- | --- |
| `/mute`、`/unmute`、`/scene [名称]` | 麦克风和场景控制 |
| `/commands` | 中文指令搜索，保留草稿 |
| `/more` | 打开 OBS、诊断、帮助、退出等更多操作 |
| `/settings` | 设置与操作总菜单 |
| `/diag`、`/diag repair` | 查看诊断；备份后重建历史派生副本 |
| `/pin`、`/find [关键词]` | 重点消息；搜索已归档会话 |
| `/obs` | 检查 OBS 连接并显示状态，与 `/obs status` 相同 |
| `/help`、`/about`、`/quit` | 帮助、关于、安全退出（不停止推流） |
| `/ai`、`/ai start`、`/review`、`/pause`、`/ai stop` | 打开、启动、审核、暂停、结束助手 |
| `/ai auto on`、`/ai auto off` | 本人授权自动发送；立即撤销为逐条审核 |
| `/ai range`、`/ai topic` | 本场回复范围；本场 AI 主题（不改直播间标题） |
| `/ai visible`、`/ai reset` | 处理可见消息并重建上下文；丢弃当前上下文重建 |
| `/ai model`、`/ai replies` | 模型/原生选项；发送策略 |
| `/ai materials`、`/ai advanced`、`/ai workspace [路径]` | 人设资料、Agent 配置、工作区 |
| `/room title [标题]`、`/room cover [图片路径]` | 修改标题；上传封面审核 |
| `/display theme` | 选择主题 |
| `/display names on`、`/display names off`、`/display time on`、`/display time off` | 保存昵称或时间显示 |
| `/display layout chat`、`/display layout list` | 保存聊天或列表布局 |

### OBS 子目录

`/obs config host`、`/obs config port`、`/obs config mic`、`/obs config password` 分别编辑连接地址、端口、麦克风与隐藏密码；`/obs status` 查看状态；`/obs connect` 检查连接；`/obs start` 立即开始推流；`/obs stop` 经确认和 3 秒倒计时停止推流。

### 设置子目录

`/settings reading`、`/settings account`、`/settings obs`、`/settings ai`、`/settings system` 分别直达外观与显示、B站账号与直播间、OBS、AI助手、系统与关于。

### 高级 CLI：私有实例、Agent 与 MCP

以下面向高级集成，不是普通用户启动路径：

```text
danmu setup pi
danmu --instance <绝对私有目录> local [--assistant]
danmu --instance <绝对私有目录> agent <status|messages|report|reply|result> '<JSON对象>'
danmu --instance <绝对私有目录> mcp
```

- `setup pi` 只能单独运行，不能混用 `--instance`、房间、账号、OBS、回放、配置或显示参数。
- `local` 是无房间、仅人工输入与 LocalTransport 的隔离终端；`--assistant` 只让它启动即打开助手设置面板，不启动模型或发送。
- `agent` 连接已经运行的实例，参数必须是 JSON 对象；`status/messages/report/reply/result` 使用同一 Bridge 业务契约。
- `mcp` 通过 stdio 连接实例，不另启 TUI，不登录，也不读取平台凭据。
- `local`、`agent`、`mcp` 必须显式使用绝对私有 `--instance` 目录；只有 `local` 要求新的空目录，`agent`/`mcp` 应指向已运行实例。它们不接受房间、登录、OBS、回放或生产配置混用；显示参数只用于普通 TUI，不用于连接工具。

协议、参数与取消语义见 [README 高级集成](https://github.com/rockythink/shisui-danmu#高级集成)；外部 Agent 可阅读[共享 danmu-duty Skill](https://github.com/rockythink/shisui-danmu/blob/main/agent-package/skills/danmu-duty/SKILL.md)。更多资料：[九宿主接入与认证边界](https://github.com/rockythink/shisui-danmu/blob/main/docs/agent-onboarding.md#九宿主接入与认证边界)、[贡献指南](https://github.com/rockythink/shisui-danmu/blob/main/CONTRIBUTING.md)、[安全策略](https://github.com/rockythink/shisui-danmu/blob/main/SECURITY.md)、[MPL-2.0 许可](https://github.com/rockythink/shisui-danmu/blob/main/LICENSE)。
