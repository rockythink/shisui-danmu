---
title: DANMU 使用手册
description: DANMU 中文使用手册，正式版仍为 v0.5.1；设置与 AI 配置另注明未发布源码行为。
head:
  - tag: script
    attrs:
      type: application/ld+json
    content: |
      {"@context":"https://schema.org","@type":"TechArticle","headline":"DANMU 使用手册","description":"DANMU 中文使用手册，正式版仍为 v0.5.1；设置与 AI 配置另注明未发布源码行为。","url":"https://danmu.elazer.wang/guide/","isPartOf":{"@type":"WebSite","name":"DANMU","url":"https://danmu.elazer.wang/"}}
---

当前最新正式版仍为 **DANMU v0.5.1**。DANMU 是面向知识型主播的免费开源 B 站弹幕与直播监控工作台；当前只接入 B 站，不是播放器，也不替代 OBS。程序免费开源不代表模型或搜索免费：API 计费、ChatGPT 订阅资格与额度、搜索可用性遵循各服务规则，没有默认免费模型或自动付费回退。

:::note[正式版与未发布源码]
本手册的设置菜单、轻量 HTTP 助手、双 API Key 协议、官方 ChatGPT 订阅 preview、format 4 配置与资料引用说明按**当前未发布源码**更新，不是 v0.5.1 已发布功能。安装渠道目前仍提供 v0.5.1，其设置入口与上述源码行为可能不同；不要仅凭 `danmu --version` 输出 `0.5.1` 判断已获得这些改动。正式版历史见[更新日志](/changelog/)，未发布说明不承诺下个版本号或发布日期，也不代表真实模型、订阅额度或公网搜索已验收。
:::

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

输出包含 `0.5.1`。若找不到命令，重新打开终端并检查安装目录是否在 `PATH`；若摘要不匹配，不要运行文件，重新从 [Release](https://github.com/rockythink/shisui-danmu/releases/latest) 下载。

### 升级

沿用原安装渠道：npm/Bun 重新全局安装，Homebrew 先 `brew update` 再 `brew upgrade danmu`，Cargo 重新运行安装命令，安装脚本或预编译包则重新下载最新 Release。升级后再次运行 `danmu --version`。请自行退出并重启所有旧的 DANMU 实例；新二进制不会替你停止正在直播的终端或 OBS。

DANMU 的基础 TUI 提供 macOS arm64、macOS x86_64、Linux arm64、Linux x86_64、Windows x86_64 五种构建。未发布源码的内置助手改为应用直接发起 HTTP，不再需要本地 coding CLI、Node 适配器或 Unix 子进程宿主；这不代表 Windows/Linux 的新助手界面和真实服务均已完成本轮验收。

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

普通提示与成功提示3秒后消退，警告/错误15秒后消退，进行中的操作提示保留到完成。文字消失不代表错误已解决，也不会恢复暂停的助手或发送权；用`/diag`检查实际状态，处理原因后显式启动。

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

以下为**未发布源码行为**。输入 `/settings` 打开“设置与操作”。四类长期入口依次为：**B站账号与直播间、OBS、AI助手、系统与关于**；同页还提供本场 AI、重点消息、关键词搜索归档、指令搜索和退出。方向键或 `Tab` 选择项目，`Enter` 执行，`Esc` 逐层返回，`F1` 或 `?` 查看说明。

- “外观与显示”类别及 `/settings reading` 已移除；主界面 `Tab` 和 `/display` 命令仍保留。历史空闲自动返回降为 TOML 的 `history_idle_seconds`，为 `0` 时只手动返回；启动参数 `--history-idle-seconds` 可作本次覆盖。
- `/display theme` 选择长期主题；`/display names on`、`/display names off`、`/display time on`、`/display time off`、`/display layout chat`、`/display layout list` 保存对应显示设置。
- `/pin` 标记当前选中消息；未选择时使用最新消息。成功后界面提示已设为重点。
- `/find [关键词]` 搜索已归档会话；省略关键词会打开编辑器，匹配结果显示房间与场次。没有结果时先核对关键词，再在 `/diag` 查看历史状态。

旧 `/history`、`/theme`、`/layout`、`/login` 等斜杠写法不是当前教程；登录使用启动参数，长期设置使用上述菜单、当前 `/display` 命令或对应配置文件。

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

以下为**未发布源码行为**。内置值班助手由 DANMU 直接连接模型 HTTP 服务，不再启动 ACP、Pi、Codex 或其他本地 coding CLI。可选择 OpenAI 兼容 Chat Completions、Anthropic Messages 两种 API Key 协议，或官方 ChatGPT 订阅 preview。配置、启动生成、审核候选与授权发送是不同操作；保存设置不调用模型、不授发送权。

### 从逐条审核开始

1. 按 `Ctrl+G` 或输入 `/ai` 打开“本场 AI”；这一步只开面板。
2. 在 `/settings` →“AI助手”中检查“模型”“发送策略”“人设与资料”“维护”。“发送账号”跳转到“B站账号与直播间”。
3. 打开 `/ai model`，按下文配置 API Key，或由本人完成官方 ChatGPT 授权并选择账号模型。
4. 输入 `/ai start` 或在菜单选择“启动 AI”。“本机就绪，等待新消息”只表示本地配置与 worker 已准备好，**不表示远端已鉴权**；无新消息时不请求模型列表或进行付费连接测试。
5. 保持逐条审核，输入 `/review` 阅读候选并决定是否发送。
6. `/pause` 或 `Ctrl+P` 暂停本地生成、取消在途请求并撤权，保留候选和人工草稿；`/ai stop` 另结束助手的本场占用。取消本地请求**不保证服务端推理或收费已经停止**。恢复需显式启动，不重放已消费的旧问题。

### 简化后的 AI 页面（未发布源码）

- `/ai model` 管理连接协议、API 基址、模型与隐藏 Key 输入；订阅连接显示账号、官方授权、退出和账号模型选择。
- `/ai replies` 的“发送策略”只保留回复方案、联网搜索、屏蔽词；方案差异见[回复策略与调用成本](#回复策略与调用成本)。
- `/ai materials` 的“人设与资料”只保留助手名字与人设选择；资料文件仍可在工作区编辑。
- `/ai advanced` 的“维护”只保留处理可见弹幕、结束助手；`/ai reset` 与“重建上下文”已移除。每轮由应用重新装配资料与历史，不维护供应商会话。
- `/ai workspace`（含路径形式）已移除；低频配置改为文件编辑，见[工作区、数据与隐私](#工作区数据与隐私)。本场 `/ai range` 与 `/ai topic` 仍保留。

## 模型连接与密钥

### API Key：两种明确协议

1. 先按 `Ctrl+P` 暂停并撤销发送权，等待在途请求与停止过程结束；待续用授权或账号操作进行中也不能改连接。
2. 在 `/settings` →“AI助手”→“模型”（或 `/ai model`）选择“连接方式”。OpenAI 兼容使用 Chat Completions；Anthropic 使用 Messages，不自动探测或互相回退。
3. 填写包含版本路径的服务根与明确模型名：官方默认基址分别为 `https://api.openai.com/v1`、`https://api.anthropic.com/v1`；模型不预填。基址后由程序追加 `/chat/completions` 或 `/messages`，不要填写完整请求端点。仅允许 HTTPS，或用于本机服务的 loopback IP HTTP；禁止 URL 中的认证、query 和 fragment。
4. 选择“设置/更换 Key”，或执行不带参数的 `/ai key` 打开**隐藏输入**。粘贴只插入；`Enter` 仅保存，`Esc` 取消并恢复原 Unicode 草稿与光标。保存失败保留秘密输入，不转为公开弹幕；不要把 Key 写进斜杠命令、普通模型字段或截图。
5. 回到本场 AI 显式启动，先逐条审核真实候选。清除 Key 使用模型页“保留/清除”确认，空输入不代表清除。

更换协议会保存该协议官方基址并清空模型；重选当前协议不重置自定义基址或模型。仅有一个当前 API Key 记录，按**协议 + 规范化基址**匹配，换地址/协议不发送旧 Key，同服务换模型可复用。换 Key 会替换唯一记录，不建立多服务密钥库。OpenAI 兼容仅指本手册的 Chat Completions 契约，不包含 Azure 专用认证、任意厂商变体或 API Key Responses。

`assistant.json` 的 `api.max_output_tokens`（256–8192，默认 4096）与 `api.openai_token_limit` 为文件级设置；外部编辑后需重启读取。OpenAI 只发送选中的 `max_completion_tokens`（默认）或 `max_tokens`，不会收到错误后偷偷换字段重试；Anthropic 固定使用 `max_tokens`。订阅 preview 不使用这两个输出上限选项。

### ChatGPT订阅：官方preview

此连接使用 OpenAI 官方 **OSS/local Sign in with ChatGPT** 流程，不是旧 Codex/ACP OAuth，不读取 coding CLI 的认证文件。当前官方 preview 面向具备资格的 ChatGPT **Plus/Pro** 用户；以官方实际资格、授权 scope、账号模型目录与可用额度为准，登录成功不等于已获得订阅推理权限。preview 可变更或终止，不承诺永久支持、无限额度或真实账号已验收。

1. 同样先暂停、撤权并等待停止完成，在模型页选择“ChatGPT订阅（官方 preview）”。该连接固定官方 `https://api.openai.com/v1`，不能把 OAuth Token 发往自定义服务。
2. 选择 **Continue with ChatGPT**，由本人在系统浏览器核对并完成官方授权；DANMU 使用本机 loopback 回调接收结果，不替你接受授权。取消或失败不替换当前账号，不发送草稿；授权待完成时不能重复登录、换连接或启动助手。
3. 授权完成后，DANMU **自动读取账号模型并打开选择列表**；首次只需明确选择一次，不必再点击“读取账号模型”和“选择账号模型”。选择已保存账号也会自动读取；同注册重新登录时，仍可用的已选模型会保留并标为“当前”，不自动换模型。读取失败保留登录与已有选择，可用“读取账号模型”手动刷新/重读。目录读取会联网，但不是推理或发送测试，不自动启动助手或授权发送。
4. 显式启动，收到候选后审核。此路径逐事件解析官方Responses流，只在完整终态且HTTP正常结束后准入整批候选；中间增量和推理文本不发送。单事件（含字段/注释/换行）限256KiB，整个解压流限8MiB、最多4096个数据事件；不再把所有重复增量累计限制为256KiB。每轮完整重装应用资料与历史，使用`store:false`，不复用供应商会话，也**不读取或导入ChatGPT旧聊天历史**。

额度与 ChatGPT 共享，并受每应用限制。额度耗尽、资格不足、429/503 或用量服务不可用会明确暂停并撤权；不重试该批、不自动切模型或回退到付费 API。可在 [ChatGPT Usage](https://chatgpt.com/settings/usage) 查看用量，并在 ChatGPT 设置管理/撤销应用授权。模型页“退出当前账号”停止本机使用、清除所选账号 Token 并尝试官方撤销，保留注册映射；远端撤销是否成功以实际回报为准，不能仅凭本地退出声称已撤销。

官方说明：[OSS/local token sharing](https://developers.openai.com/siwc/token-sharing-open-source)、[preview 限制](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)。真实授权、订阅额度、推理效果与公网搜索不作为本文已完成的验收。

### 凭据存储与修改边界

API Key 保存在实际 `assistant.json` 同目录的 `assistant-secrets/api-key.json`，OAuth Token 保存在 `assistant-secrets/chatgpt.json`。它们是**本机私有权限保护的明文文件，不是 Keychain、加密保险箱或同 UID 恶意进程沙箱**。Unix 新建秘密目录为 `0700`、文件为 `0600`；既有宽权限、符号链接或不安全路径会被拒绝，不自动 chmod 修复。Windows 不宣称具有 Unix 等价 ACL 保护。不要同步到公开仓库、工作区副本、Journal 或截图。

运行、在途请求、停止过程、账号操作或已有发送许可时不能修改连接、Key、订阅账号或联网开关；TUI 待续用许可时也拒绝提交。运行中的 API Key 是启动快照，外部手工编辑或删除文件不会即时改变已发请求；先暂停本地 worker，再显式启动读取新文件。服务端撤销 Key、OAuth 授权和收费状态需在服务商处另行核对。

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

**未发布源码**将常用偏好合并为三个回复方案，选中时一次保存十项回复设置：

| 方案 | 基础范围与普通问答 @ | 静音时协助 | 主动感谢 |
| --- | --- | --- | --- |
| 少主动回复（Quiet） | 优先回应点名，不 @ 提问者 | 关闭 | 全部关闭 |
| 侧重回答问题（TextQa） | 也回答明确问题，@ 提问者 | 开启 | 全部关闭 |
| 感谢礼物、关注和分享（Thanks） | 优先回应点名，不 @ 提问者 | 开启 | 感谢礼物（含上舰）、关注和分享；不感谢点赞 |

这十项是基础积极性、四类感谢、普通问答 @、联网搜索、有限修复、开麦主播优先、静音时协助。所有方案都会把**联网搜索重置为关闭**，需要时再到 `/ai replies` 显式开启；开麦主播优先与有限修复固定启用。方案不会改自动发送许可、账号、模型，也不会启动助手。感谢默认全部关闭；Thanks 中感谢是否逐人 @ 不受普通问答 @ 开关影响。“少主动回复”不是完全不回复；完全不回复请暂停助手。

本场 `/ai range` 仍可选择三档基础积极性：**优先回应点名、也回答明确问题、允许主动补充**，`/ai topic` 编辑本场主题。麦克风未静音时固定为主播优先、减少插话，但不会压住完整文字问答；DANMU 只读取麦克风状态，不检测、转写或理解主播说了什么。麦克风状态未知或过期时按保守策略处理。

普通空白、纯笑声，以及平台已识别为纯表情的消息可以跳过；有实质文本、问号或明确点名的消息保留。模型返回“零候选”与完整正常响应没有回复正文是两种不同结果：前者表示本轮选择不回复，后者表示没有可发送正文；两者都不会重试旧消息。

首次空轮后，普通消息等待 5 秒合批；连续空轮最多延长到 10 秒。明确点名可绕过合批等待，但不能绕过单飞限制、逐条审核或自动发送授权。未知或重复的本批目标会让 **本批整体拒绝**，随后继续处理新消息，不鼓励重放旧消息。

联网搜索默认关闭，只在用户显式开启且本批有合格公开问题时提供。程序只把被选中的**本批完整公开弹幕/SC 正文**发送到 Exa，不扩写查询，不附加 UID、昵称、私有资料或历史；问题须为 1–240 个 Unicode 字符且无控制字符，过长问题不截断搜索。每轮最多一次搜索、最多 3 条来源、一次后续模型补全，不抓取任意 URL，也不开放文件、shell 或其他 Agent 工具。失败、限流、无结果或不可用会显示未核实；若仍生成候选，**强制逐条审核，不能自动发送**。不承诺 Exa 公共额度稳定或永久免费，也不假报已核实。模型和搜索可能消耗额度；DANMU 不承诺固定省费比例、不漏答或永不报错。

空轮只改变下一批普通消息的等待时间，不会替任何未来候选打开发送闸门。

## 工作区、数据与隐私

默认助手工作区是 `~/Projects/danmu-assistant/`：顶层放用户可编辑的人设和资料，`.danmu/` 放程序维护的副本与索引。私有原始 Journal 是恢复事实来源；工作区中的会话文件是维护副本，不应取代原件。配置、账号凭据、Bridge token 与实际私有数据目录以应用 `/diag` 显示为准，不要依赖手工猜测路径。

**未发布源码的低频配置：** 工作区路径 `workspace`、选中技能 `skills` 和补充要求 `preferences` 在实际 `config.toml` 同目录的 `assistant.json` 中配置；当前格式为 `format: 4`，模型连接放在 `api` 对象。外部编辑需重启读取，修改前备份，不把 JSON 写进 TOML。`host`、`binary`、`source`、`native_preferences` 已退休，不再读取原生配置、提示文件或宿主 OAuth。首次准备或迁移自有工作区仍遵循原有权限边界。

旧无版本/format 2/3 文件加载时不覆盖原件，保留规定的资料、业务偏好和归档，但关闭 `automatic`、`resume_on_start`、`web_search`。第一次显式保存前逐字备份为 `assistant.pre-api.json`；已有备份内容不同或备份失败则拒绝保存，不覆盖旧数据。需重新配置 API Key 或官方订阅授权、显式启动并重新授予发送许可；旧原生模型、规则和认证不推测迁移。请本人把必要规则放入既有 `SYSTEM.md`、人设、`PROJECT.md` 或 `preferences`，程序不自动扫描或复制原生宿主资料。

`Settings::load` 固定将 `use_profile`、`use_project`、`dynamic_host_priority`、`repair_blocked` 置为 `true`：旧文件里的 `false` 会在启动时归一化为内存中的 `true`，**仅加载不会覆盖原文件**，后续正常保存才写入归一化结果。公开简介与 `PROJECT.md` 存在时按原有可用性与隐私边界自动引用，不再要求逐项开启。`SYSTEM.md`、选定人设、项目资料与选中技能的合计 **8KiB** 上限不变；技能仍须在 `skills` 中显式选中，未选技能不加载、不执行，选中也不执行附带代码或全局插件。资料引用不会开放任意读写、联网或发送权限。

建议定期备份应用诊断所示的私有数据和自己的工作区。`/find [关键词]` 搜索已归档会话；`/diag repair` 会先备份，再重建程序派生副本，不删除原始历史，也不授予发送权限。

每次模型请求都包含当前资料、助手名字/偏好、人设、本场主题、公开房间信息、当前问题、近期对话、真实送达结果与相关历史；应用上下文（system + 本轮 JSON）合计最多 24KiB，完整 HTTP 请求最多 48KiB，不靠截断当前问题凑预算。近期对话最多 12 条/4KiB，历史合计最多 4KiB；按房间与准确 UID 检索本批之前的记录，不把同名观众混为一人。已确认分段是真实已发送事实，未确认或不确定段不能当成功；待审/丢弃候选不会被写成已说过的话。

Journal、SQLite 历史和用户工作区仍留在应用侧，不迁到供应商会话。既有历史索引导入观众事件与场次，**不把所有旧助手回复导入历史检索**；旧回复事实仍在原 Journal。只存在于原生宿主且未归档的聊天状态不声称可迁移；历史损坏或读取失败不伪装成空记忆。资料不足、依赖未知口播或主播私人立场/承诺时可不回答，原问题仍在弹幕流和归档，不另建主播待办队列；提示词也不保证模型判断正确。

HTTP 接入没有为外部 CLI/MCP Agent 建立 OS 沙箱：其其他工具需在外部宿主侧另行限制。不要把 Token、账号文件或敏感历史放进人设、Skill、公开仓库或截图。下播资料整理继续使用已有外部 Agent/MCP 接口，不新增自动复盘流程。

## 故障排查

先输入 `/diag`；在诊断页按 `F1` 或 `?` 展开上轮摘要。不要把“等待合批”误判成停机，也不要用删除历史或重放旧消息作为通用修复。

| 现象 | 检查 | 安全处理 |
| --- | --- | --- |
| 无房间或进不去 | 房间号、网络、启动自检错误 | 更正房间号；`Enter` 重试；只对明确可跳过项按 `S` |
| 登录失败/已失效 | `/diag` 的账号状态、二维码是否过期 | 重新运行 `danmu --login`；不复制浏览器 Cookie |
| OBS 未连接 | OBS 是否运行、WebSocket v5、主机/端口/密码 | 用 `/settings obs` 修正，再 `/obs connect`；不要用开播测试连接 |
| 连接未配齐/Key 文件错误 | `/ai model` 中协议、基址、模型与 Key 状态 | 暂停撤权后修正；不绕过私有文件权限检查，不删除历史 |
| 订阅账号/模型不可用 | 官方授权资格、推理权限、所选账号及账号模型目录 | 由本人重新授权或显式读取该账号模型；不猜模型或改用付费 API |
| 助手未启动 | 是否只打开了 `/ai`，本地配置是否齐备 | 明确执行 `/ai start`；等待新消息不代表远端鉴权成功 |
| 助手未授权 | 状态是否逐条审核、账号/房间是否变化 | 用 `/review` 逐条批准，或本人确认自动发送 |
| 有待审回复 | `/review` 中候选数和正文 | 审核后批准、编辑或丢弃；不要重复启动 |
| 等待合批 | 轮次摘要的下一等待时间 | 等待 5–10 秒；点名也仍受授权与单飞限制 |
| 零候选 | 摘要显示“零候选，仍在运行” | 等待新消息，不重试旧消息 |
| 正常响应无正文 | 上轮摘要没有可发送正文 | 等待新消息，不重试旧批，不记为已回答 |
| HTTP/协议/响应中断错误 | `/diag` 的安全错误摘要，401/403/429/5xx 等状态 | 助手会暂停并撤权；修复原因后显式启动，无自动重试、换服务或模型回退 |
| 订阅额度/用量不可用 | 已知额度错误摘要、[ChatGPT Usage](https://chatgpt.com/settings/usage) | 核对共享额度与每应用限制，保留账号 Token；不付费回退 |
| 搜索不可用 | 本轮搜索状态为 unavailable，未核实 | 只审核本轮候选或关闭搜索；不当作已联网核实 |
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
| `--instance <绝对私有目录>` | 为普通 TUI 暴露该实例的 Agent/MCP 连接点；local/agent/mcp 必须显式使用 |
| `-r, --room <房间号>` | 显式房间号，优先于位置参数和保存配置 |
| `--replay <JSON路径>` | 无网络回放 JSON 数组；不读账号/OBS/默认配置，不连接直播 |
| `-l, --single-line <true\|false>` | 本次列表元信息同行；长正文仍可换行 |
| `-s, --show-time <true\|false>` | 本次显示或隐藏时间 |
| `--show-name <true\|false>` / `--hide-name` | 本次控制昵称；hide 优先 |
| `--history-idle-seconds <秒数>` | 本次历史空闲返回秒数；`0` 仅手动返回 |
| `-c, --config <路径>` | 使用指定 TOML 配置 |
| `--theme <主题名>` | 本次主题；未发布源码的长期主题用 `/display theme` 修改 |
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
| `/ai visible` | 显式纳入当前可见消息，不重放已消费的旧批 |
| `/ai model`、`/ai key`、`/ai replies` | 模型连接；仅打开隐藏 Key 输入（无明文参数）；回复方案、联网搜索和屏蔽词 |
| `/ai materials`、`/ai advanced` | 名字与人设；可见弹幕和结束助手 |
| `/room title [标题]`、`/room cover [图片路径]` | 修改标题；上传封面审核 |
| `/display theme` | 选择主题 |
| `/display names on`、`/display names off`、`/display time on`、`/display time off` | 保存昵称或时间显示 |
| `/display layout chat`、`/display layout list` | 保存聊天或列表布局 |

### OBS 子目录

`/obs config host`、`/obs config port`、`/obs config mic`、`/obs config password` 分别编辑连接地址、端口、麦克风与隐藏密码；`/obs status` 查看状态；`/obs connect` 检查连接；`/obs start` 立即开始推流；`/obs stop` 经确认和 3 秒倒计时停止推流。

### 设置子目录

未发布源码只保留 `/settings account`、`/settings obs`、`/settings ai`、`/settings system`，分别直达B站账号与直播间、OBS、AI助手、系统与关于。`/settings reading` 与 `/ai workspace` 已移除，无旧命令兼容入口。

### 高级 CLI：私有实例、Agent 与 MCP

以下面向高级集成，不是普通用户启动路径：

```text
danmu --instance <绝对私有目录> local [--assistant]
danmu --instance <绝对私有目录> agent <status|messages|report|reply|result> '<JSON对象>'
danmu --instance <绝对私有目录> mcp
```

- `local` 是无房间、仅人工输入与 LocalTransport 的隔离终端；`--assistant` 只让它启动即打开助手设置面板，不启动模型或发送。
- `agent` 连接已经运行的实例，参数必须是 JSON 对象；`status/messages/report/reply/result` 使用同一 Bridge 业务契约。
- `mcp` 通过 stdio 连接实例，不另启 TUI，不登录，也不读取平台凭据。
- `local`、`agent`、`mcp` 必须显式使用绝对私有 `--instance` 目录；只有 `local` 要求新的空目录，`agent`/`mcp` 应指向已运行实例。它们不接受房间、登录、OBS、回放或生产配置混用；显示参数只用于普通 TUI，不用于连接工具。

协议、参数与取消语义见 [README 高级集成](https://github.com/rockythink/shisui-danmu#高级集成)；外部 Agent 可阅读[共享 danmu-duty Skill](https://github.com/rockythink/shisui-danmu/blob/main/agent-package/skills/danmu-duty/SKILL.md)。内置 runner 与同 caller 外部写入互斥，其他独立 caller 仍受各自原授权边界，不因切换 HTTP 开放发送权。更多资料：[助手接入与认证边界](https://github.com/rockythink/shisui-danmu/blob/main/docs/agent-onboarding.md)、[贡献指南](https://github.com/rockythink/shisui-danmu/blob/main/CONTRIBUTING.md)、[安全策略](https://github.com/rockythink/shisui-danmu/blob/main/SECURITY.md)、[MPL-2.0 许可](https://github.com/rockythink/shisui-danmu/blob/main/LICENSE)。
