---
title: 更新日志
description: DANMU 各版本的新增功能、体验改进、问题修复与升级提示，从早期 TUI 到 v0.5.1。
tableOfContents:
  minHeadingLevel: 2
  maxHeadingLevel: 2
---

每次更新，具体改了什么。

这里按版本从新到旧整理公开发布记录，保留重要的升级提醒。内容依据 [GitHub Releases](https://github.com/rockythink/shisui-danmu/releases) 与对应版本的源码记录；未发布的本地改动不计入正式版本。日期统一使用 **UTC**，`0.3.2` 单独标注为标签记录。

:::tip[准备升级？]
当前最新正式版为 **v0.5.1**。npm 用户运行 `npm install -g danmu-tui@0.5.1`，其他用户沿用原安装渠道更新；再运行 `danmu --version` 核对版本，并自行退出、重启旧的 DANMU 实例。**不要删除历史库，无需停止 OBS。** 具体说明见[使用手册 · 升级](/guide/#升级)。
:::

[返回官网首页](/) · [使用手册](/guide/) · [下载最新版本](https://github.com/rockythink/shisui-danmu/releases/latest)

## v0.5.1

**2026-09-19 · 最新正式版**  
修复 Windows 首次启动与退出时的终端状态处理。

### 修复

- 修复 Windows 启动时的 `Initial console modes not set`：尚未启用鼠标捕获时，不再执行依赖已保存控制台状态的关闭操作。
- Windows 使用 Crossterm 支持的标准键盘输入路径，不再启用其不支持的 Kitty 键盘协议。
- 退出时先释放当前已启用的鼠标捕获，再恢复普通键盘输入；各项清理独立执行，避免一项失败阻断后续备用屏幕与光标恢复。
- 对应 [issue #1](https://github.com/rockythink/shisui-danmu/issues/1)。已通过 [macOS、Linux、Windows CI](https://github.com/rockythink/shisui-danmu/actions/runs/35427994678)；Windows 独立控制台回归覆盖冷启动、鼠标捕获切换和退出恢复。

### 升级

npm 用户运行 `npm install -g danmu-tui@0.5.1`；其他渠道沿原渠道升级。升级无需删除历史库，也无需停止 OBS。

[原始发布说明与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.5.1) · [完整代码变更](https://github.com/rockythink/shisui-danmu/compare/v0.5.0...v0.5.1)

## v0.5.0

**2026-09-17 · 正式版**  
本机 AI 助手、统一设置与可持续使用的直播工作区。

### 新增与改进

- **本机 ACP 助手**：连接用户已安装、已认证的原生 AI 工具，分别管理模型连接、助手启动、候选审核与自动发送授权。新增 `danmu setup pi`，显式准备私有 `pi-acp@0.0.33`；不会替用户安装原生 Pi、登录或启动模型。
- **可编辑工作区与历史检索**：管理助手人设和参考资料，按直播间建立历史索引并跨场检索。历史在后台加载；新增 `/diag repair`，先备份再重建派生副本，不删除原始 Journal。
- **统一设置入口**：`/settings` 集中管理外观、B 站账号与直播间、OBS、AI 助手、系统五类设置。主账号与独立助手账号分设发送队列；独立账号失败不会回退到主账号发送。
- **完整候选审核**：支持查看、编辑和显式批量批准。保存候选不等于发送，批量批准不授予未来候选发送权，人工草稿保持不变。
- **低成本合批与轮次诊断**：区分模型返回零候选和原生轮次无正文；跳过普通空白、纯笑声及已识别的纯表情。连续空轮后，普通消息按 5–10 秒合批；点名不等待合批，但仍受单飞、背压与发送授权约束。
- **站内使用手册**：新增 Astro Starlight 手册，支持目录、搜索与移动端阅读；README 调整为产品与安装入口。

### 修复与安全边界

- 候选引用未知或重复的本批消息 ID 时，整批拒绝、不发送、不重试该批，继续处理新消息；真实协议、连接和磁盘错误仍明确暴露。
- 自动发送必须由本人授权；已保存的自动偏好仅在同房间、同发送 UID 且启动恢复检查通过后续用。暂停、身份或房间变化、身份失效都会撤权；送达不确定不自动重发。
- 基础 TUI 提供 macOS arm64/x86_64、Linux arm64/x86_64、Windows x86_64 五种构建；**内置 ACP Runner 当前要求 Unix**，部分宿主限制更窄。程序免费开源不代表外部模型和搜索免费。

[原始发布说明与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.5.0) · [完整代码变更](https://github.com/rockythink/shisui-danmu/compare/v0.4.5...v0.5.0)

## v0.4.5

**2026-09-06 · 正式版**  
历史浏览提醒与可选自动返回。

- 历史模式增加高对比状态条和醒目边框，持续提示“浏览历史 · 已暂停跟随 · Esc 返回实时”，显示新增消息数与可选倒计时；窄屏保留返回提示。
- 默认仅手动返回实时。`/history 60` 启用空闲 60 秒自动返回，`/history off` 关闭，`/history` 查看设置；开启后仍可手动返回。
- 按键、滚轮操作重置空闲计时，新消息到达不重置。自动返回保留草稿，登录二维码、密码或停播弹窗期间暂停计时。
- Esc 只取消当前操作或返回实时，连续按键不退出 TUI。退出使用 `Ctrl+C` 或 `/quit`；保留 End 和向下滚回最新一条恢复跟随。

**该版本配置提示：** TUI 内的历史自动返回设置仅当前会话生效；持久启用可在 TOML 中设置 `history_idle_seconds = 60`。启动参数 `--history-idle-seconds 60` 优先，设为 `0` 关闭。

[原始发布说明与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.4.5) · [完整代码变更](https://github.com/rockythink/shisui-danmu/compare/v0.4.4...v0.4.5)

## v0.4.4

**2026-09-06 · 正式版**  
逐条浏览历史与更谨慎的停播确认。

- 历史弹幕改为逐条滚动：每个滚轮事件移动一条消息，不再整页跳动；从回复选择接续当前位置，新消息到达时保留阅读锚点，End 返回实时。
- OBS 连接状态统一为单个圆点：绿色表示已连接，红色表示未连接。
- `/obs stop` 打开停播弹窗，默认选中“返回”；方向键确认后倒计时 3 秒，Esc 可取消。移除 `/obs confirm` 和 `/obs cancel`。

[原始发布说明与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.4.4) · [完整代码变更](https://github.com/rockythink/shisui-danmu/compare/v0.4.3...v0.4.4)

## v0.4.3

**2026-09-05 · 正式版**  
让进场、点赞提醒不再挤动弹幕正文。

- 进场、点赞移到 Ghost Stage 底边。临时互动与普通消息分别保留最多 240 条，通知不再占用普通消息的历史缓存；历史翻页跳过临时互动。
- 姓名优先的批次提示至少稳定展示 2 秒，同批按宽度展示最多 3 个完整名字，多余用“等”；进场优先，空间足够时附加点赞姓名摘要。
- 同批重复互动仅按可靠用户 ID 合并，不按昵称合并不同观众。只保留当前批次与下一批摘要，不建立长队；过期或已替换提示不补播。
- 关闭姓名显示时，提示同步隐藏名字。展示合并不减少原始事件日志和计数。

[原始发布说明与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.4.3) · [完整代码变更](https://github.com/rockythink/shisui-danmu/compare/v0.4.2...v0.4.3)

## v0.4.2

**2026-09-04 · 正式版**  
修复礼物连击重复统计，调整直播状态栏。

- 修正新版 `SEND_GIFT_V2` 的交易 ID 与连击批次 ID 解析。同一作者、批次和礼物在原消息上累计数量，不再出现多条 ×1 再加一条汇总。
- 逐笔数量按交易 ID 去重，与批次累计数量取较大值，不重复相加。重复明细、较旧汇总和恢复后重放不会让计数倒退；保留礼物消息位置并同步更新重点消息。
- 不同批次、不同盲盒结果保持独立。**旧版本已归档的重复行不追溯修改。**
- 红色 LIVE 标志每 500 ms 同步亮起、熄灭，移除红色底块；右侧计时移除蓝底，保留白字与计时逻辑。
- README 补齐官网同款 Logo、官方链接与实机录屏。

[原始发布说明与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.4.2) · [完整代码变更](https://github.com/rockythink/shisui-danmu/compare/v0.4.1...v0.4.2)

## v0.4.1

**2026-09-04 · 正式版**  
官网上线，完善启动自检与 OBS 密码保存。

- 官网 `danmu.elazer.wang` 正式上线。
- 启动动画与本地会话恢复、网络客户端初始化并发执行，减少动画前的空白等待；启动页增加作者信息。
- 启动自检覆盖本地数据、B 站登录、直播间、指标和 OBS；检查失败停留在启动页，可重试、跳过或配置 OBS 密码。
- OBS 密码改为应用私有文件保存，不再访问系统钥匙串；macOS/Linux 文件权限为 `0600`，支持 `OBS_API_PASSWORD` 临时覆盖。
- 礼物连击按同批次合并并更新累计数量，不同批次保持独立；改进 B 站实时消息解析，以及瞬时 HTTP 连接失败、超时的重连错误分类。

**升级提醒：** 旧版保存在系统钥匙串的 OBS 密码不会自动迁移。如提示缺少密码，可在启动页按 Enter 配置，或进入 TUI 后执行 `/obs config password`；无需将密码发给任何人。

[原始发布说明与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.4.1) · [完整代码变更](https://github.com/rockythink/shisui-danmu/compare/v0.4.0...v0.4.1)

## v0.4.0

**2026-09-02 · 正式版**  
Rust 重写与多渠道分发。

该版本的原始 Release 仅提供代码比较链接，以下依据对应提交记录整理：

- 使用 Rust 重建 TUI，加入原生 OBS 电平采集，改进终端交互与 OBS 状态显示。
- 调整 DANMU 品牌展示与电平布局。
- 建立多包管理器分发，完善 Linux 发布依赖、可重复执行的发布流程与 npm Trusted Publishing。
- 修正测试中的平台存储目录约定。

[原始 Release 与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.4.0) · [原始提交范围](https://github.com/rockythink/shisui-danmu/compare/0.3.2...v0.4.0)

## 0.3.2

**2026-08-29 · Git 标签记录，非独立 GitHub Release**

- 新增 B 站专属表情的接收与显示。

此日期为标签所指向提交的 UTC 日期，不作为安装包发布日期。仓库标签名为 `0.3.2`，没有 `v` 前缀；公开 Release 列表中没有对应条目。

[查看版本源码](https://github.com/rockythink/shisui-danmu/tree/0.3.2) · [原始提交范围](https://github.com/rockythink/shisui-danmu/compare/v0.3.1...0.3.2)

## v0.3.1

**2026-08-28 · 早期正式版**  
面向 macOS 的 Swift TUI。

该版本的原始 Release 没有逐项更新说明，以下为随版本归档的 README 所列能力，不表示这些功能都首次加入于本版：

- 监看 B 站直播互动，登录后发送弹幕，管理提问状态与重点互动，记录和恢复会话。
- 提供多套 True Color 主题与有限的 OBS 遥控。
- 当时要求 macOS 14+，支持 Apple Silicon 与 Intel Mac；源码构建使用 Swift 6，OBS 遥控依赖 `obs-cli`。
- 提供带 SHA-256 校验的 Universal 预编译程序；卸载只删除可执行文件，不删除配置、钥匙串凭据与会话日志。

这些是**历史版本的能力与限制**，当前安装要求请以[使用手册](/guide/)为准。

[原始 Release 与下载](https://github.com/rockythink/shisui-danmu/releases/tag/v0.3.1) · [该版本 README](https://github.com/rockythink/shisui-danmu/blob/v0.3.1/README.md) · [提交记录](https://github.com/rockythink/shisui-danmu/commits/v0.3.1)
