# 终端舞台功能基线

日期：2026-08-21
上游行为参考：[`yaocccc/bilibili_live_tui`](https://github.com/yaocccc/bilibili_live_tui)
数据口径参考：[Bilibili API Collect · 直播间基本信息](https://socialsisteryi.github.io/bilibili-API-collect/docs/live/info.html)、[Bilibili API Collect · 直播间信息流](https://socialsisteryi.github.io/bilibili-API-collect/docs/live/message_stream.html)

> 上游仓库未提供 LICENSE。本项目只按公开界面和外部行为独立实现，不复制其源码。B 站网页接口不是公开稳定 SDK；Adapter 通过严格字段解析和契约测试显式暴露漂移，不用默认值掩盖未知状态。

## 产品边界

DANMU 是独立的免费终端产品。平台协议由 Rust `bilibili` Adapter 负责；标准事件、会话投影和日志由 `domain` 与 `persistence` Module 负责。Cookie、CSRF 和原始平台 payload 不进入终端配置、领域事件或归档。

## Rust TUI 验收矩阵

| 能力 | 验收标准 |
| --- | --- |
| 房间连接 | 位置参数、`--room` 或 TOML 配置均可进入；短号解析为真实房间号 |
| 历史弹幕 | 连接前拉取最近弹幕，连接期间每 5 秒用历史窗口补偿 WebSocket 漏包；history/live 按平台事件 ID 及作者、内容、类型去重，补偿消息按服务端时间进入实时视图；主动发送以主播身份回流确认，并抑制延迟到达的遮罩昵称副本 |
| 实时事件 | 覆盖弹幕、礼物、大航海、SC、进场、点赞、关注、分享、PK、抽奖、房管、开停播和系统事件；礼物同时兼容 JSON `SEND_GIFT` 与 protobuf `SEND_GIFT_V2`，盲盒显示“原盲盒 → 开出礼物”；同一 `batch_combo_id` 的 `SEND_GIFT` / `COMBO_SEND` 就地更新为累计数量，不重复展示或记账，不同批次保持独立 |
| 自动重连 | 使用 1、2、4、8、15、30 秒有界退避；不递归、不 panic；状态栏显示下次延迟 |
| 房间信息 | `getRoomBaseInfo` 首次加载并每 30 秒刷新：请求失败自动重试一次，字段级合并新响应，缺失或解析失败字段沿用上次成功值；`room_id` 是真实房间号，`uid`/`uname` 是主播身份，`title` 是直播间标题，`parent_area_name`/`area_name` 是父子分区，`live_time` 是中国标准时间的本次开播时刻；`live_status` 严格映射 0=未开播、1=直播、2=轮播，未知值直接报契约变化；刷新失败不终止弹幕连接 |
| 实时计数 | ◉ 只采用 `WATCHED_CHANGE.data.num`，♥ 只采用 `LIKE_INFO_V3_UPDATE.data.click_count`，▤ 是本次运行收到的实时弹幕数，● 采用登录态 `getOnlineRank.data.onlineNum`；在线人数请求失败自动重试一次并沿用上次成功值，首次无值或未登录时显示 `--`。`getRoomBaseInfo.online` 与心跳 op=3 均视为人气值，不作为在线人数 |
| 登录 | TUI 内 `/login` 使用低纠错紧凑二维码并按当前终端空间居中显示；无法完整容纳时不裁切、不折行，提示调整窗口并在 resize 后自动恢复二维码；`danmu --login` 保持标准终端二维码。登录成功后 Cookie/CSRF 保存到 TUI 独立 `BilibiliAccount/session.json`，Unix 权限为 `0600`，不使用系统钥匙串；`/logout` 删除该登录态 |
| 发送弹幕 | 平台单段限制 22 个 Unicode 字素；长消息按 20 个字素上限，优先在标点、其次在空白处分段，无合适断点时硬拆；分段间隔一秒，失败停止后续分段 |
| 文本编辑 | 输入框按终端显示宽度自动换行，最后一行始终保持 `╰─ 输入内容  ─╯` 形态，内容后以空白填充、仅两端保留短横线；最多显示四行并跟随光标滚动；支持左右方向键、Home/End、删除、`Ctrl+A/E/U/W`；`Ctrl+C` 或 `/quit` 安全退出并恢复终端状态 |
| 键盘消息选择 | `Shift+↑/↓` 移动回复目标并使用低刺激前景色强调；选择期间新消息不改变目标；`Enter` 在光标处插入 `@用户名 `；`Esc` 取消 |
| 鼠标历史浏览 | 滚轮每次按当前可见页翻动历史；只改变视口，不选中回复目标；`End` 或向下滚回底部返回实时视图 |
| 应用通知 | 操作提示、进度、警告和错误统一显示在弹幕框下方、输入框上方，不进入弹幕内容区域，也不参与弹幕滚动与回复选择 |
| 外观 | 内置 `shisui`、`catppuccin-mocha`、`tokyo-night`、`gruvbox-dark` 高对比 True Color 主题；输入 `/theme` 显示主题列表，方向键选择并按 `Enter` 即时切换；`themes.json` 可新增或修改主题并支持热重载；主题不改变功能与布局 |
| 单/多行 | `single_line` 配置与 `--single-line` 控制元信息和正文是否同行；正文按终端宽度换行 |
| 显示开关 | `show_time`、`show_name`、`--hide-name`、`/time`、`/names` 控制时间与用户名 |
| 消息布局 | `Tab` 在信息流与聊天布局间即时切换；聊天布局将元信息、正文分行并保留消息间距 |
| 主播身份 | 依据直播间主播 UID 使用独立用户名颜色；正文保持事件语义色 |
| CJK 与 Emoji | 按 Unicode 字素编辑和分段，按终端显示宽度定位光标；过滤常见 IME 格式字符；已知 B 站短码映射语义 Emoji，平台图片表情使用通用 Unicode Emoji 降级显示 |
| 实时状态分层 | 顶部固定两行：第一行左侧为 LIVE/OFFLINE/ROTATING 操作状态，直播间标题使用主题 `rank` 语义色绝对居中，右侧为已开播时长；第二行左侧显示主播名称，右侧整体右对齐显示 OBS 与 MIC 状态。OMP Box 上边框左侧发送状态使用主题 `success`、`info`、`warning` 语义色；右侧仅显示 ◉、♥、▤、● 及数值。主播弹幕名称前的 `♚` 使用 `rank`，主播名使用 `host` |
| 会话归档 | 每个会话写独立 `journal.jsonl`；结束时生成 `summary.md` 与 `snapshot.json`；`/archive [关键词]` 搜索历史 |
| OBS 配置 | `danmu --configure-obs` 配置主机、端口、场景、麦克风和密码；`/obs config password` 可在 TUI 内隐藏输入并更新密码；密码写入独立 `obs-password` 文件，Unix 权限 `0600`，不使用系统钥匙串；`OBS_API_PASSWORD` 可临时覆盖；`/obs config mic` 持久化输入名 |
| OBS 控制 | `/obs`、`/obs status|connect|mute|unmute|scene|start|stop`；每次异步操作立即显示进度，认证、密码和连接失败给出可执行提示；场景和输入候选读取 OBS 真相；停播需 `/obs confirm`，可 `/obs cancel` |
| 麦克风电平 | 原生 OBS 事件约每 50 毫秒输入一次，Adapter 聚合为 10 Hz；显示限制在 -60 至 0 dBFS，快速上升、平滑回落；输入框顶栏宽度充足时显示数值和彩色区间，窄屏降级为短条，静音显示 `MIC MUTE`，断连时隐藏 |

## 架构门禁

- TUI 与商业 GUI 使用完全隔离的配置、凭据和会话日志命名空间。
- TUI 只写自己的 JSONL Journal，不读取或修改商业 GUI 的日志。
- TUI 独立拥有直播连接；商业 GUI 不安装、包含或启动 TUI。
- TUI 退出、终端崩溃或 OBS 不可用不能删除既有会话日志。
- 所有新增平台字段先在 Adapter 中按已核验的原始 key 严格解析并转换为标准模型，再进入平台无关领域层；字段缺失或枚举未知必须暴露契约错误，不得猜测或静默置零。
- TUI 仅显示所配置麦克风的单路电平，不包含播放器、完整直播画布、音频混音、录制、Replay Buffer 或 OBS 场景编辑能力。
