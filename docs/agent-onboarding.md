# 弹幕助手接入向导

当前正式版为 v0.5.1；当前接入章节描述未发布 HTTP/订阅源码，不是正式版行为。用户任务式操作见[使用手册](https://danmu.elazer.wang/guide/)；历史构建、测试数和 ACP/Pi 证据保留当轮事实，不适用于新链路。认证见[双 API 与官方 ChatGPT 订阅](#双-api-与官方-chatgpt-订阅)。

## 未发布：轻量 HTTP 助手与设置精简（当前源码）

唯一内置链路：本地选新消息和记忆 → 单飞 HTTP → 必要时一次 Exa 搜索和一次补全 → 整批校验 → 原 Bridge 审核/许可/发送。ACP Runner、九宿主发现、原生参数同步、`danmu setup pi`、`/ai reset`退役；不需 coding CLI、Pi、Node 或适配器。外部资料维护、五操作 CLI/MCP 和 `script/agent_config.py`保留，不是内置依赖。

- `/settings`仍为账号、OBS、AI助手、系统四类，外观走Tab与`/display`；AI助手为模型、发送策略、人设与资料、维护。本场范围/主题保留，维护仅处理可见弹幕/结束。
- 三方案一次保存十项，不改账号/模型/许可/续用意图、不启动推理；感谢默认关，感谢方案不含点赞，方案关闭联网搜索。开麦让位、有限修复、公开简介与PROJECT引用固定开启，旧false仅内存归一化。资料8KiB、显式选技能且只读文本不变。
- `assistant.json`改format=4，旧source/binary/native_preferences不再是运行时字段；不继承宿主模型、OAuth或自动发送授权。workspace/skills/preferences仍为文件级启动配置，外部修改后重启。

### 本次源码与验收边界

本文依据当前Runner/settings/api/subscription/responses/terminal源码描述接口，不将下方历史测试数、安装哈希、ACP/Pi实证当作本轮HTTP/订阅验收。真实模型、官方浏览器授权、Plus/Pro实际额度、公共Exa可用性以及Windows/Linux新设置与授权UI尚未在本文宣称验收。最新正式版仍为v0.5.1，本次未发布、不承诺下一版本号。

### 最新本机修复验收：订阅大SSE流与警告消退（2026-10-06）

- `RUST_TEST_THREADS=1 ./script/verify.sh`通过fmt、Clippy `-D warnings`、488项测试（1个既有订阅PTY入口默认ignored）、Release与CLI。证据：`/tmp/danmu-sse-notice-final-verify.log`。回归覆盖256KiB单事件、8MiB解压流、4096数据事件的含界限/超限、chunked/gzip、CRLF/BOM/UTF-8分块、完整EOF、断流、取消/截止和迟到非法事件；HTTP错误仍限256KiB。
- 安装路径两种API Key协议的Release真实PTY各9次模型请求，无失败、退出0；订阅使用同源码的隔离cfg(test)TUI入口及安装版CLI/MCP，合成OAuth+559093字节SSE正常产出小终态候选，10次模型请求、无失败、退出0。错误文字15秒后消退，worker仍暂停、许可撤销、诊断/Journal保留错误、Unicode草稿不变。结果：`/tmp/danmu-sse-notice-installed-openai_chat/results.json`、`/tmp/danmu-sse-notice-installed-anthropic_messages/results.json`、`/tmp/danmu-sse-notice-installed-chatgpt_subscription/results.json`；同目录保留VT画面。所有验证为loopback与合成凭据，不读取真实账号，不访问模型/直播平台；端点移除、进程回收及凭据泄露扫描通过。
- `~/.local/bin/danmu`已安装，与Release SHA-256一致：`ff637b94e27ec6b56bbc8b039a089fe1eb81d7a644fcec95a246eb88bb4a2e7d`；仍为未发布源码版本0.5.1，安装不更新运行中的TUI。
- 网站`npm run build`通过，Astro检查零错误/警告/hints；构建仍报告既有docs→404缺失警告。已在本地Chromium检查使用手册与changelog桌面/390px移动端，未见页面错误；网站未部署。

### 历史本机修复验收：大目录与自动接入（2026-10-06）

- `RUST_TEST_THREADS=1 ./script/verify.sh`通过fmt、Clippy `-D warnings`、476项测试（1个订阅PTY入口默认ignored）、Release与CLI；日志：`/tmp/danmu-catalog-auto-verify.log`。目录回归涵盖8MiB含界限/超限、解压、未知元数据非法Unicode及严格JSON、隐藏/可见项数量限制，推理和HTTP错误256KiB限制不放宽。
- 真实订阅PTY使用合成签名OAuth和大于256KiB的双模型目录：登录后直接选择、有效重登录保留、刷新、配额暂停、取消/换场、记忆与送达闭环通过；2次自动目录读取、10次模型请求，无失败、正常退出0。证据：`/tmp/danmu-catalog-auto-pty-02/results.json`及同目录VT画面。单独loopback回归覆盖目录失败手动再读、失效模型清空与迟到目录拒绝；不冒充真实官方账号验收。
- 已安装`~/.local/bin/danmu`，与Release SHA-256一致：`848c1dac854440ca80c2bf6737e84906faabf897d3a19df7ec826c811068610f`。仍为未发布源码构建`0.5.1`，安装不更新运行中的TUI。
- 安装路径两种API Key协议真实PTY均通过，各9次模型请求、零失败、退出0；endpoint移除、进程回收及合成凭据泄露扫描通过。证据：`/tmp/danmu-catalog-auto-installed-openai_chat/results.json`、`/tmp/danmu-catalog-auto-installed-anthropic_messages/results.json`。
- 官网`npm run build`通过，Astro检查零错误/警告/hints；已在本地Chromium核对新自动接入正文，无页面错误，并目视检查订阅PTY首次双模型列表和重登录“当前”标记。仅更新本地源码与构建，未部署。

### 历史本机门禁与安装：HTTP切换初次验收（2026-10-06）

- `RUST_TEST_THREADS=1 ./script/verify.sh`通过fmt、Clippy `-D warnings`、471项测试、Release和CLI；1个订阅真实PTY入口默认ignored，由smoke脚本显式运行。包括跨进程writer/reader-openai/reader-anthropic记忆、完整资料、实际分段送达与有限修复回归。门禁日志：`/tmp/danmu-http-final-verify.log`。
- `./script/install_cli.sh`安装至`~/.local/bin/danmu`；Release与安装版SHA-256相同：`b73bdf1bc8c83848de9e18fef02da10e0d7fef1e59d99db9a7cb10bcbb6f7644`。版本号仍为`0.5.1`，不是新正式发布。
- 最终Release与安装路径两协议真实PTY均通过，各9次模型HTTP，无失败；配置/键鼠入口/Key遮罩与保存失败保留/Unicode光标、前问与已确认回复、坏候选后继续、未知送达不重发、在途取消/换场、搜索不可用强制待审及外部caller边界已验证。Release证据：`/tmp/danmu-http-release-final-openai_chat/results.json`、`/tmp/danmu-http-release-final-anthropic_messages/results.json`；安装证据：`/tmp/danmu-http-final-openai_chat/results.json`、`/tmp/danmu-http-final-anthropic_messages/results.json`。
- 订阅使用同一产品代码的cfg(test)真实PTY入口与loopback签名/OAuth/Responses端点，通过初次授权、模型目录显式选择、按轮刷新并持久旋转、429共享额度暂停撤权且保Token/无API计费回退、本机退出但远端撤销失败如实报告、按原host/client/subject重新登录。证据：`/tmp/danmu-http-final-chatgpt_subscription/results.json`，10次模型请求、1次显式目录请求；不冒充真实官方账号验收。
- 上述实例均退出0、endpoint移除、自有进程回收，合成Key/Token泄露扫描通过。已目视检查实际VT导出的已配置两协议、秘密遮罩/保存失败、候选、搜索诊断与订阅配额/重登录画面。七类外部配置只运行生成的danmu stdio命令，不启动原生宿主。ANSI/HTML/cells、脱敏请求和本地送达事实保留在各证据目录；私有凭据文件不得对外打包。
- 官网Node.js 24下`npm run build`通过，Astro检查零错误/警告/hints；构建仍提示docs→404条目不存在。本地Chromium已查看首页、guide与未发布changelog，以及390px移动首页；35个内部链接/锚点检查无断链、无页面错误。仅本地构建与呈现，未部署。
- 验证均使用私有临时目录、合成凭据和loopback；未调用真实模型、官方账号额度、公共Exa或B站公开发送，未操作用户直播/OBS/终端。OAuth与Key为权限保护的明文文件，不是加密保险箱。安装不升级运行中进程，由用户完全退出并重新打开TUI。

### 历史：上一轮设置精简与 OMP 身份识别

上一轮OMP18.6.0回报ACP名称`omp`，当时修复接纳`omp`与`oh-my-pi`，其余身份拒绝。下方保留当时事实；原生链路现已退役，不作为当前接入说明。


### 历史本机验证（设置精简候选，非本次 HTTP 迁移）

- `RUST_TEST_THREADS=1 ./script/verify.sh` 通过格式、Clippy、374 项测试、Release 与 CLI：`/tmp/danmu-settings-final-verify.log`。`npm --prefix website run build` 通过，Astro 检查零错误、零警告；本地 Chromium 已检查四类设置与未发布说明的实际页面，未部署网站。
- `./script/install_cli.sh` 已安装候选；Release 与 `~/.local/bin/danmu` 的 SHA-256 均为 `f773b26cdb77198137dd1d6d375c29a49e661037c30e8797411acd3fa693e223`。版本号仍为 `0.5.1`，不冒充新正式发布。
- 安装版实际 PTY 验证四类设置、回复方案保存、本场范围不写回长期方案、维护页、主题及移除命令：`/tmp/danmu-settings-smoke-pwveq19x/` 的 ANSI 与逐页文本记录；正常退出 0。
- 安装版连接真实 OMP，初始化回执确认为 `omp 18.6.0`，创建 ACP 会话并显示原生模型选项；诊断为“助手未启用、已交模型 0”。证据：`/tmp/danmu-settings-smoke-znfox1l7/results.json`、同目录画面与 `/tmp/danmu-omp-smoke-bin/` 的方法/身份记录。没有 `session/prompt`；正常退出 0。
- 仅使用新建 local 私有实例；未连接真实直播间、未调用模型推理、未公开发送、未操作 OBS。临时验证脚本和依赖已清理，证据保留。由用户自行完全退出并重启旧 TUI；运行中实例不会因安装自动升级。

## 历史：Windows 终端启动与退出恢复（v0.5.1）

- 对应 [GitHub issue #1](https://github.com/rockythink/shisui-danmu/issues/1)：v0.5.0 Windows 启动报错 `Initial console modes not set`。源码定位到未开启鼠标捕获就执行关闭；Crossterm 0.29 的 Windows 后端此时尚未保存原始模式。该后端还明确拒绝 Kitty 键盘增强命令，不能只修鼠标后留下第二个启动错误。
- Windows 启动不再执行未配对的鼠标关闭或 Kitty Push/Pop；Unix 既有初始化保留。退出仅关闭当前持有的鼠标捕获，再恢复普通输入；粘贴、键盘和备用屏幕清理分别尝试，避免前一项失败阻断后续恢复。不吞运行期鼠标命令错误。
- `src/terminal/windows_tests.rs` 在全新子进程和独立 Windows 控制台内验证冷启动、反复开关捕获、捕获开启时退出，并检查输入模式和原屏幕恢复。现有 Windows CI 的 `cargo test --locked --all-targets --all-features` 会自动执行；也可单独运行 `cargo test --locked terminal::windows_tests::console_lifecycle -- --exact`。
- v0.5.1 最终本机门禁 `RUST_TEST_THREADS=1 ./script/verify.sh` 通过格式、Clippy、374 项测试、Release 构建和 CLI：`/tmp/danmu-v051-verify.log`。随后 `./script/install_cli.sh` 安装成功，版本为 `danmu 0.5.1`；Release 与 `~/.local/bin/danmu` 的 SHA-256 均为 `d784a2ee6be904d259977adcba2146013f38435603d29865c8fcc15b2a85f300`。
- 安装版 macOS 实际 PTY 验证三种场景全部退出 0、终端模式完整恢复，并检查鼠标、备用屏幕、光标、粘贴和键盘协议的清理输出。证据：`/var/folders/dn/4hk1m1lj33g38l1jzt3wj4140000gn/T/danmu-windows-fix-smoke-ktv9m2h3/results.json` 及同目录 ANSI 记录。仅使用全新 local 私有目录，不连接真实房间、模型或 OBS，不操作用户终端。
- 独立代码审核通过。提交 `9d421074d3379d60a48958afb1a5680afabdcc80` 的 [CI 35427994678](https://github.com/rockythink/shisui-danmu/actions/runs/35427994678) 全绿（macOS、Linux、Windows、官网）；Windows 的 310 项测试通过，日志明确包含 `terminal::windows_tests::console_lifecycle ... ok`，覆盖独立真实控制台的三种生命周期。日志：`/tmp/danmu-v051-windows-ci.log`。本机交叉检查曾因缺少 Windows SDK 的 `assert.h` 失败，不将其算作 Windows 运行证据。
- 验证边界：上述 Windows 证据来自 GitHub Windows runner，不等于已在 issue 报告者的终端环境回访确认。启动中途失败的统一回滚仍是既有局限，本轮未声称修复。临时类型检查代码已清理；用户自行重启旧 TUI，不操作用户终端、OBS 或历史库。
- 正式发布：标签 `v0.5.1` 对应上述审核提交；[Release 流水线 35428328279](https://github.com/rockythink/shisui-danmu/actions/runs/35428328279) 全绿，五平台安装包及校验文件齐备。npm 通过 OIDC 发布（隔离安装后实际输出 `danmu 0.5.1`）；crates.io 从该提交的干净临时工作树发布并可查询；Homebrew 配方提交 `6b0be48cb768c5c7f0d35d3df13eae2ee6f613f0`，macOS ARM 发布包 SHA-256 校验通过。
- 官网更新日志已部署，Cloudflare 版本 `31501841-b44f-4188-80ef-bac9ed612c85`。本地与线上独立 Chrome 验收版本提示、首页导航、Windows 搜索结果和 390px 手机无横向溢出；截图保留于 `/tmp/danmu-v051-site-{desktop,search,mobile}.png`。构建后用 Pagefind CLI 重新生成索引；自动化托管浏览器将 Worker 改成 blob URL 导致相对路径失败，已向工具报告并使用独立 Chrome 排除此工具故障，未为此修改产品搜索实现。

## 上轮：空回复可诊断，普通消息低成本合批

历史问题：曾观察到连续模型轮次没有公开回复。旧记录没有每轮原文与麦克风快照，不能追认开麦策略是确定根因；本次补足轮次诊断，不宣称自动重启解决了历史崩溃。
- 开麦让位仅减少主动插话，不把未静音当作正在讲话或停止问答依据。直接问助手及基础范围允许的完整公开文字问题仍可简短回答；明确给主播/他人的消息、未知口播与发送授权边界不变。这是提示策略，不是对真实模型理解效果的保证。
- 普通Danmu纯空白、hhh/哈哈及平台已识别纯表情在代码层跳过，model_reaction_skipped留在分流统计；不删原始历史，不按长度/问号过滤实质文本，保留SC、明确@、原生回复目标名及开头直呼。问号、未识别方括号文本保留；点名边界避免名字子串误判。
- 合法零候选与EndTurn无正文区分；不退役健康连接、不重试旧批次。首个空轮后普通消息等待5秒，连续空轮最多10秒合批，点名绕过等待但不绕过单飞/背压/授权；非空结果恢复2秒。上下文轮换保留节流状态，普通新启动清零，不新增付费分类器或模型重试。
- 当前统一autoReply/round_completed替代candidate_rejected专属事件，旧历史不改写。记录no_reply/no_message/queued/review/rejected/expired/stale/cancelled/error/not_queued、冻结轮次/原生会话/本批ID、耗时、应用输入字节、麦克风与策略、候选数、实际受理状态、待审原因及最多512字符原因；不存完整prompt/模型正文。/diag新增上轮摘要。日志失败仍暂停撤权，确认送达不改为失败，人工草稿保留。
- 专项真实Pi0.85.1+pi-acp0.0.33验证：/tmp/danmu-stability-debug-r2/results.json。连续空候选/无正文之后仍可生成待审候选；反应消息0次HTTP；10秒内3条普通问题合成1次请求并保留原ID；点名不等待合批。全部只走loopback假凭据，未公开发送；退出0、端点移除、自有子进程全部自然清退，已目视检查实际PTY画面。不据此声称真实模型选题正确或实际账单下降固定比例。

- 最终门禁：RUST_TEST_THREADS=1 ./script/verify.sh通过格式、Clippy、372项测试、Release和CLI：/tmp/danmu-stability-verify.log。随后./script/install_cli.sh已安装；Release与~/.local/bin/danmu的SHA-256均为d251290c09072e51fa85087f6de00f9cda78e70451ba522e5c528f82d63626f3。
- 安装版证据：/tmp/danmu-stability-installed/results.json，3条普通问题在约9.94秒合成1次请求，点名从终端输入到loopback请求约1.98秒（包含验证器按键等待，并非真实服务延迟）；全部候选主动丢弃，未授发送许可。/tmp/danmu-stability-pi-safety/results.json确认真实Pi的429/半段重试、重试耗尽明确报错、未知/重复目标整批拒绝后继续、取消后下一轮、read/write/bash拒绝等仍通过。

- 安装版实际画面复验：/tmp/danmu-stability-installed-visual/results.json全部通过；/diag按F1或?展开后，已目视检查stability-no-message-diagnostics.html与stability-diagnostics.html，分别显示原生无正文和候选待批准的上轮摘要。退出0、端点移除、自有进程自然清退。临时验证脚本与对应pycache已清理，证据目录保留；仅由用户自行重启旧TUI，不操作用户终端/OBS/直播。

## 上轮：错误候选目标只拒绝本批，不停止整个助手

历史问题：助手曾因“未知或重复本批消息ID”停机。旧记录未区分两者且不含该批原文，不能追认具体错误ID，也不能归因于送达回声缺失。
- 根因是已完整结束轮次中的目标校验错误沿ACP错误通路清退连接，再触发Runner全局暂停。现在用类型区分候选结果与目标拒绝：未知/重复目标整批不入队、逐消息Failed，不重试该批；保留旧候选及当前权限，继续新消息。提示要求仅复制untrusted_messages.id、每个目标一条候选，禁止混用author_id或历史ID。畸形JSON、旧round、跨会话、权限与真实连接错误仍安全停止。
- 拒绝诊断在界面及现有autoReply/candidate_rejected归档中保存：轮次、原生会话、本批ID、区分未知/重复的最多80字符转义目标；写盘失败仍暂停，不把失败当成功。真实发送结果不改写。
- 修复前真实Pi0.85.1/pi-acp0.0.33+旧安装版复现同一句停机原因；之后的新问题不再请求模型：/tmp/danmu-target-native-before/results.json及其instance会话日志。该旧版临时二进制已删除；原TUI/直播未操作。
- 新增Runner回归覆盖未知、重复及合法/非法混合候选的整批拒绝、原候选保留、已授权/未授权状态不变、无旧批重试和原会话下一题可用；旧严格故障回归保留。RUST_TEST_THREADS=1 ./script/verify.sh通过格式、Clippy、367项测试、Release和CLI，再./script/install_cli.sh成功：/tmp/danmu-target-verify.log。Release与~/.local/bin/danmu的SHA-256均为7254fdc12a5316ed3b9298ec8cb98cf62bf3dd7e3ecb6031d40f06cf2ac6af1e。
- 安装版真实Pi验证：/tmp/danmu-target-native-installed/results.json。连续未知/重复目标均整批拒绝且落盘，未手动重启即生成下一条候选（验证器主动丢弃、不公开发送）；既有429/半段重试、耗尽报错、取消及工具拒绝也通过。已目视检查实际VT拒绝提示；退出0、端点移除、自有子进程全部清退。仅loopback模型、不读真实凭据、不发B站消息；不能保证模型永不生成错误ID，但该类错误不再直接停掉整个助手。用户自行重启旧TUI生效。

## 上轮：主账号与独立助手发送队列

- TerminalApp分别持有人工与独立助手队列；enqueue依据冻结SendIdentity选择，等待回声、分段间隔与失败取消不跨队列。独立身份不可用仍留在独立队列并沿用身份检查，不回退主账号；明确复用人工账号时继续串行。没有修改身份撤权、安全校验、未知送达不重发等边界。
- 两项新增回归先红后绿：原来人工消息必须等独立助手全部分段完成；人工拒绝还会把已排队的独立助手取消。修复后验证双向分段互不阻塞、人工失败不取消独立助手、复用主账号仍串行。证据：/tmp/danmu-queue-red.log、/tmp/danmu-queue-green.log；使用真实enqueue/SendQueue和隔离LocalTransport，不访问真实账号。
- RUST_TEST_THREADS=1 ./script/verify.sh通过格式、Clippy、366项测试、Release构建和CLI；随后./script/install_cli.sh安装成功。日志：/tmp/danmu-queue-verify.log。Release与~/.local/bin/danmu的SHA-256均为e33c0a385695dbaabb9ba931b760634cbbd57afaaf6cfd9db601a2e640e07359。
- 安装版实际PTY证据：/tmp/danmu-queue-installed-smoke/results.json；复用身份分段串行、暂停AI后人工发送、人工被拒后的新输入恢复与草稿保留均通过，已目视检查VT画面。空目录local实例不能登录独立账号，独立双账号隔离由上述真实队列回归覆盖，不冒充此PTY实测。Ctrl+C退出0、端点移除；无真实模型或B站发送，未操作已有用户终端。临时/tmp/danmu-queue-smoke.py已删除，保留证据；用户自行重启旧TUI生效。

## 上轮：协议正文、送达回声与老观众记忆

- Pi 0.85.1 / pi-acp 0.0.33的重试、压缩通知原本进入agent_message_chunk，污染候选JSON。受限原生launcher现在以只读RPC过滤器按事件类型分离；仅缓冲text_delta、最多1MiB，重试/压缩清掉失败尝试，工具开始清掉工具前解说。错误终态通过typed error通知停机，保留有限原因，不当正常空回答；不禁用原生重试、不从散文里提取JSON、不改已安装上游依赖。
- 真实Pi+pi-acp与loopback模型已验证429后恢复、半段正文失败后恢复、四次耗尽明确失败、普通/空回复、取消后下一轮和read/write/bash拒绝：/private/tmp/danmu-pi-retry-final2-1789495119/results.json。真实免费搜索后返回候选也通过：/private/tmp/danmu-pi-search-final-1789495238/results.json，推理仍只走loopback；无真实模型凭据或B站发送。两组均退出0、endpoint移除、自有进程全部清退。
- TerminalTransport先等待UI安装PendingDelivery匹配器，再执行POST；原来跨channel抢先回声会漏认。真实loopback回归先移除屏障跑红（UI未注册时出现POST），恢复后跑绿：响应前注入极速UID回声，最终EchoConfirmed且只有一次POST。UID/昵称遮罩、时间窗、重复抑制、迟到确认和不重发边界不变。
- 旧现场journal只有接收/分流/停机记录，没有模型原文或发送诊断，故不能将每次历史JSON错误或EchoMissing都断言为上述软件缺陷。新增assistantStopped的160字符转义正文预览；autoReply/send_completed保留发送队列的请求、UID、平台诊断和确认/未确认分段。写盘失败暂停助手，真实Confirmed不改为失败，不重发并保留人工草稿。
- 老观众历史复用同房间SQLite和author/time索引，精确UID查询相关旧互动并补近期；每人最多3条，全批轮流分配最多8条，每条240字符。观众记忆最多3KiB，与话题检索共享4KiB，仍计入24KiB单轮/64KiB累计预算。保留时间与UID，缺身份/无历史不假装认识，不合并同名，不混其他房间或当前/未来消息。历史只作不可信片段，不作授权、永久偏好或臆测的个人资料。
- 12项历史回归与61项Runner回归通过；覆盖重开索引后旧相关事实、同名异UID、房间/时间边界、复用ID和按观众分配的字节预算。实际PTY跨场检索及单条审核发送落盘：/tmp/danmu-memory-debug-20260915/results.json，带入上场UID42互动、不混UID43，恰好一次LocalTransport发送且未授全场权限。诊断PTY：/tmp/danmu-diagnostic-debug-20260915/results.json，畸形候选停机且保留草稿。两者均正常清退，已目视检查实际VT画面。
- 修复前真实复现：/private/tmp/danmu-pi-retry-before-final-1789495458，隔离验证器以--unfiltered-before只绕过RPC过滤，429后第二次模型输出是合法JSON，但18:04:36Z的assistantStopped仍记录Retrying状态文字拼在JSON前、expected value at line 1 column 1。与上述成功链路组成红绿证据；不据此追认已清理的用户历史原始输出。新增离线Node实际过滤器回归纳入cargo test，覆盖重试、工具前解说、错误终态与压缩后的独立正文。
- 最终门禁：RUST_TEST_THREADS=1 ./script/verify.sh通过格式、Clippy、364项测试、Release构建和CLI检查；随后./script/install_cli.sh安装成功。日志：/tmp/danmu-fault-memory-final-20260915.log。target/release/danmu与~/.local/bin/danmu的SHA-256同为ac3b0b960bc250fbc632b645ade82b32c822446206f4895a8ce2658b99aaa95f。仅由用户自行重启旧TUI，不控制已有终端或直播。
- 安装版实际复验通过：/tmp/danmu-memory-installed-20260915/results.json（跨场精确UID记忆、恰好一次本地审核发送及autoReply落盘）、/tmp/danmu-diagnostic-installed-20260915/results.json（诊断停机、手工草稿保留）、/tmp/danmu-pi-installed-20260915/results.json（真实Pi0.85.1/pi-acp0.0.33、loopback模型，重试/部分输出/耗尽失败/取消/工具拒绝）。安装版PTY画面已目视检查；全部正常退出、端点移除、自有子进程清退，不触碰现有用户TUI或B站发送。临时演练脚本/tmp/danmu-diagnostic-smoke.py已移除，保留证据目录及现有script/verify_pi.py回归能力。

## 上轮：AI上下文累计预算

- 每个ACP上下文最多8轮，累计应用输入最多64KiB；问答与一次性改写均计入，按静态资料去重前UTF-8字节保守计数。每轮仍最多8条新消息、24KiB输入；开始下一轮前预留完整24KiB空间，剩余不足时先轮换，不取走待处理改写、不截断问题。不把应用输入预算称为模型总token或原生工具输出上限。
- 达到预算复用既有安全轮换路径：清退独占ACP进程、建立新上下文、重新载入规则和人设；保留本场队列、最多12条/4KiB近期对话、候选、在途发送与仍有效的授权。暂停取消续接、撤权不恢复，完整归档和原始512条窗口不变。新场、规则重建及原生新上下文成功后重置预算，普通暂停/续接不清零。
- Runner专项60项回归通过。新增12条长问题跨多个真实受控ACP进程的预算回归，验证原生输入累计不越界、全部问题按序且只投递一次、新上下文重新获得规则。旧轮换/背压用例等待真实prompt完成，不把轮换空档当作本轮完成。
- 调试版真实PTY：/tmp/danmu-context-debug-20260915-r2/results.json，12个问题分到4个上下文（4/3/3/2轮），ACP实际输入各为29713/34692/37332/26526字节；所有问题保留，发送未授权，退出0、endpoint移除、自有进程清退。通过产品设置菜单选择受控ACP，没有修改空目录启动护栏。已目视检查实际VT导出的HTML画面；没有真实模型调用、公开发送或用户终端操作。
- 完整门禁RUST_TEST_THREADS=1 ./script/verify.sh通过fmt、Clippy、358项测试和Release/CLI；随后./script/install_cli.sh成功。两份二进制SHA-256均为add7d2461a3c38598ede1e41b8afd9cf1fd43542fbddf489ef892fd843c12e62，日志/tmp/danmu-context-verify-20260915.log。安装版PTY同项复验通过：/tmp/danmu-context-installed-20260915/results.json，12个问题、4个上下文、未授权发送、退出与清退正常；一次性脚本已移除，保留JSON/ANSI/VT证据。重启现有TUI生效，不接管用户终端。

## 上轮：身份失效错误不得退出弹幕台

- 已定位路径：set_automatic_permission身份校验/记住授权失败，经command→submit→handle_key的?传播到生产主循环，导致错误退出；原菜单和local循环有拦截，普通命令入口没有。command现在单独捕获操作阶段错误、保留原消息，成功操作后的journal.snapshot错误仍向上传播，不吞磁盘错误。
- 身份校验及记住授权失败统一暂停runner、清除resume_pending与下次启用意图、撤销发送许可和记住的授权，并打开账号页。清理写入失败与原始身份错误一同显示；不更换账号、不清除主账号凭据。异步身份不可用通知同时取消待恢复状态。
- 回归先红后绿：同一句“助手身份已更改或登录态不可用”修复前从handle_key返回Err；修复后直接命令/命令搜索两条入口都留在原场次，授权关闭、搜索前Unicode草稿和光标保留，人工消息经真实LocalTransport完成。另以真实文件阻挡会话目录，验证当前journal写入错误仍传播且不覆盖已有文件。
- 真实PTY证据：/tmp/danmu-identity-1789228668714/results.json及两份HTML/ANSI画面。临时启动器直接使用生产handle_key、command、TerminalGuard和渲染，注入失效身份与隔离消息，不启动真实B站连接、OBS或外部Agent。错误后仍接收消息、退出账号页后继续编辑，只有主动/quit才退出0；endpoint移除。已目视确认。临时Rust启动器和Python驱动已删除。
- 最终验证：RUST_TEST_THREADS=1 ./script/verify.sh通过fmt、Clippy -D warnings、357项测试、Release和CLI；随后./script/install_cli.sh成功。构建及安装SHA-256均为8c37ead38bae0994d5a5427d72e297f2f1f5d86848b18f8b0de73f7b78863840；完整日志/tmp/danmu-identity-1789228668714/verify-install.log。没有绕过身份校验，没有访问用户真实直播/OBS/终端，安装后由用户重新打开TUI。

## 上轮：互动感谢多模板与逐人提及

- 点赞、关注、分享、礼物、上舰各有3套个人和3套集体模板，按本类实际生成轮换，查看待处理项不消耗轮换；不调用模型、不插入事件正文、不虚构数量。
- 同类收集至少5秒，1—5名不同观众逐人原生@，超过5人合并一条集体感谢；同一观众的多次动作不算多人。已开始批次冻结，后来的互动独立排队；全局至少15秒一条，同类批次完成后关注30秒、礼物/上舰15秒、点赞/分享60秒冷却。
- 感谢的@与问答mention_sender开关解耦；有效UID冻结到候选，复用event.id不串人；仅有昵称时做安全单行文本@，缺身份的匿名事件仅统计，不伪造对象。主账号排除、暂停清空、120秒过期、审核/本场授权与发送背压仍生效，模板失败不进入模型修复。
- 分享成为单独thank_shares设置，旧配置默认关闭；互动感谢预设开启分享，其余预设关闭。菜单、配置投影、模型边界和操作文档已迁移。原设置保持，不自动开启用户的感谢开关或发送授权。
- 候选实际运行：/tmp/danmu-thanks-candidates.json，五类个人感谢分别冻结UID 100—104，6位分享者生成集体候选；未授权自动提交被阻止，人工候选未批准不能出队。没有向真实直播间发送，也没有为模板调用模型。
- 安装版真实PTY：/tmp/danmu-thanks-final-1789227019129/results.json，四个感谢开关可用键盘保存、问答@仍关闭、发送权限未获得、正常退出且endpoint移除；已目视检查share-help.html对应的实际VT画面，帮助完整显示3+3模板、5人阈值、冷却和审核边界。不是用户Ghostty实例。一次性候选源码与PTY探针已删除，保留JSON和ANSI/VT证据。
- 最终门禁：RUST_TEST_THREADS=1 ./script/verify.sh通过fmt、Clippy -D warnings、355项测试、Release与CLI；随后./script/install_cli.sh成功。构建及安装二进制SHA-256一致：0d41ae49dda037b83df8436b1325031fb9dc13fced8289ffc76290afdc0bc6fd。日志/tmp/danmu-thanks-final-1789227019129/verify-install.log。回归直接验证5/6人数边界、名单冻结、同人多动作、轮换、暂停/过期/禁用、复用ID目标、缺UID提及和真实内容拒绝不进入模型修复，不以私有开关赋值代替行为断言。

## 上轮：AI启动反馈、主界面复制与退出清退

- 身份核验、ACP初始化及会话/偏好恢复期间显示每250ms轮转的“AI启动中”。只有真实session存在且connected就绪才显示原✦许可色；不是凭耗时或进程存在宣告成功。暂停可取消待恢复状态；真实连接错误停止动效并保留原错误。
- 主界面释放鼠标捕获，直接拖选任意文字后用终端复制快捷键（macOS ⌘C），不进入独立复制页、不更改草稿或发送权限。Ghostty等终端将滚轮转为↑↓；菜单/弹窗保留原鼠标处理。旧复制命令、菜单、专用状态/渲染及两个过时复制页测试已删除，现有演练已迁移。
- Archived预载仅以低对比度↶标识，Live无该标识；只改展示，不改预载上限、归档、计数或新场隔离。实际debug和安装版--replay均验证，输出见/tmp/danmu-ux-installed-1789224961502/history-replay.txt。
- 退出调查发现SIGTERM及主循环异常早返可能跳过异步清退。现生产与local入口统一处理SIGINT/SIGTERM/SIGHUP，主循环无论成功或错误都await Runner.shutdown；保留原错误，main返回ExitCode而非在runtime内部process::exit。重复start复用已有连接，重建等旧进程组清退再启动。
- 调试版真实PTY和自有ACP/sleep孙进程演练：/tmp/danmu-lifecycle-debug-1789224020414-{early,error,term,int,hup,repeat}/results.json。启动中SIGTERM、正常运行中SIGTERM/SIGINT/SIGHUP、受控journal写入错误、停止再启动均无自有后代残留；错误路径退出1并保留实际错误，其余退出0。未触碰用户正在运行的实例。
- 实际PTY接xterm原生选区验证：/tmp/danmu-ux-debug-1789224387881/native-copy.json与results.json。鼠标选择的“COPY_TARGET_ALPHA 中文”和复制事件内容一致，主界面mouseTrackingMode为none，人工草稿保留；已目视检查启动帧、原生选区和历史符号。没有写用户系统剪贴板；浏览器驱动的文字拖选存在超时，按实际已形成的选区核验，不将拖放工具状态冒充产品失败或整段emoji复制通过。
- 完整门禁通过：RUST_TEST_THREADS=1 ./script/verify.sh（fmt、Clippy -D warnings、350项测试、Release及CLI），随后./script/install_cli.sh。Release与~/.local/bin/danmu的SHA-256均为b45b14f46cd65308cfe844914ccf2e321b0a39e33565fb77fba7d7b28d0ea93e；日志/tmp/danmu-ux-debug-1789224387881/verify-install.log。
- 安装版实际PTY：/tmp/danmu-ux-installed-1789224961502/results.json，启动动效至真实ready、重复启动复用、旧组清退后再启动与最终退出均通过，4个自有ACP/孙进程PID全部退出，endpoint移除；已目视检查安装版启动画面。真实模型、公开发送、OBS和用户Ghostty均未操作。
- 清退仅管理danmu自有进程组，不影响用户独立启动的Agent。SIGKILL和主动脱离进程组的后代无法保证回收；不加入无关守护或扫描清理别人的进程。历史完整菜单/65轮演练不冒充本轮全量通过，版本仍为未发布0.4.5，以本节哈希区分构建。
- 旧长流程实际运行在按键/暂停焦点假设处未通过，不计为全量通过：先前用例把Repeat后附加的新Press也当成长按，并依赖释放事件。已迁移为Repeat不得发送、新Press可批准；跨层暂停使用既有Ctrl-P。安装版聚焦复验/tmp/danmu-keys-check-1789225585246/results.json三项通过：Repeat不发送、无Release的新Press可批准下一条、审核层Ctrl-P后外部直接发送被拒。未删除或放宽这些安全断言。
- 一次性PTY、暂停诊断和按键探针及下载的终端模拟器脚本已移除；保留JSON、ANSI、VT画面和门禁日志。已安装最新版，用户重启现有TUI生效，不接管现有终端或直播进程。

## 上轮：人工发送失败隔离与自动恢复

- 人工发送失败只结束本条未完成部分，并废弃当时已经排队的旧任务，不再永久暂停整条队列。后续新输入可以直接发送；未确认消息和长消息尾段不自动重试、补发或改写。登录失效仍须重新登录，不授予或恢复AI发送权限。
- 入队时冻结批次代数；旧任务完成取消时不能反向废弃新代数，避免刚输入的新消息再次收到Cancelled。串行发送、真实回显确认、身份检查、AI原撤权边界及SDK显式pause/resume保留。
- 人工提示保留实际失败诊断，将“可发新消息”放在原因前，避免恢复指引被单行截断；不再误导去助手查看人工发送详情。移除原手动恢复命令与菜单入口，内置帮助、README和既有菜单演练同步迁移。
- 扩展两条现有发送队列回归，修复前新消息仍返回Cancelled，修复后通过；覆盖未确认段与未发尾段停止、旧排队取消、之后新消息确认，以及旧任务取消时新任务已经排队的竞态。另两条原显式暂停/修复授权回归也通过。
- 完整门禁通过：RUST_TEST_THREADS=1 ./script/verify.sh（fmt、Clippy -D warnings、350项测试、Release、version/help），随后./script/install_cli.sh。Release与~/.local/bin/danmu的SHA-256均为0cd6e7087da2a965c2745f50a128ed99930505a6206d064af821c83a13cffd4c。日志：/tmp/shisui-manual-recovery-installed-1789222114500/verify-install.log。早先为调整提示顺序主动中止的一轮不计完成。
- 安装版实际PTY：/tmp/shisui-manual-recovery-installed-1789222114500/results.json。未确认和被拒后，新人工消息均不经恢复命令直接确认；4次受控发送中没有旧队列或长消息尾段重放，未开启AI权限。退出0，endpoint移除，自有进程回收；已目视检查实际VT错误提示与恢复指引。仅本地Transport，不是B站实发或真实模型验收。
- 一次性Python探针已移除，保留JSON、ANSI和VT画面证据；未控制用户终端、OBS或直播实例。开发版本仍为0.4.5，用户重启现有TUI加载本节哈希对应构建。

## 上轮：精简常用命令

- 输入/默认只显示10项常用操作；有已保存AI配置、运行连接或候选时再出现/ai、/review、/pause。低频配置留在/settings和/ai菜单，不再堆在默认列表；输入中文或前缀仍搜索完整功能，Ctrl-O保留草稿和光标。
- 静音/mute、恢复声音/unmute、切场景/scene、诊断/diag（修复为/diag repair）。旧长名称不保留别名；内部菜单、帮助、故障提示和现有agent_smoke调用全部迁移。/obs start与/obs stop保留明确命名，停播确认和3秒倒计时不变。
- /mute是设置静音，不是切换。回归覆盖连续静音不反向开麦、明确取消静音、OBS请求ACK成功但读回状态未变时仍报错；通过隔离WebSocket验证，不连接真人OBS。短命令仍受local禁止真实OBS的统一边界约束。
- 调试版实际PTY：/tmp/shisui-shortcuts-debug-1789217773666/results.json。10项默认列表、Tab补全、中文静音/模型搜索、低频页可达、草稿保留、诊断短名、OBS菜单标签、本地操作拒绝均通过；退出0、endpoint移除、自有进程回收。已目视检查实际VT默认列表。无真实OBS、模型或公开发送。
- 完整门禁与安装通过：RUST_TEST_THREADS=1 ./script/verify.sh（fmt、Clippy -D warnings、350项测试、Release、version/help），随后./script/install_cli.sh。构建与~/.local/bin/danmu的SHA-256均为ed1f6d853307c987b5224f7c0d21097a8a8fa86e72b409c57802735ecb91a9e2；下方旧哈希仅为历史证据。
- 安装版PTY同项验收通过：/tmp/shisui-shortcuts-installed-1789218087643/results.json，退出0、endpoint移除、自有进程回收；已目视检查安装版默认列表。一次性Python探针已移除，保留JSON、ANSI与VT画面证据；未控制用户终端或真实OBS，重启现有TUI加载新版。

## 上轮：代码致谢与高峰分流

- 关注、礼物、上舰、点赞使用原开关，由代码合并通用致谢；分享只统计。这些事件不进模型批次，代码模板也不调用模型改写。模板5秒收集、全局至少15秒；同类关注30秒、礼物/上舰15秒、点赞60秒。聚合不@单个观众，不把事件正文当模板或虚构数量；仍走原审核/本场授权和串行发送队列。
- 自然语言弹幕与SC保留模型语义判断，不用问号/关键词硬判是否回答；一轮同时判断与生成，可返回空候选，无独立分类请求。点名助手只影响排队优先级，不证明身份或收件人。
- 接收处建立独立队列：128条/256KiB、排队120秒，原始接收/归档不改。优先消息与普通消息按6+2组批、余量互补；过载先淘汰最旧普通项。同作者同类型/正文/回复目标30秒内去重；关注5分钟；去重只保留最近1024个键。不同作者的问题不合并，同名观众不被当作助手；缺可靠UID时昵称仅作有限去重线索，不认证身份。
- 已有8条助手待审/待发/在途回复时不开新轮，模板遇待审/待发/在途则暂缓；队列过期仍持续维护。冻结模型在途原消息，挤出512条原始窗口或出现复用ID时不改回复对象；过时轮次结果丢弃但继续新问题。原始CLI/MCP的512条窗口与gap契约保留。暂停和异常退出清理队列；正常64轮/规则上下文轮换不重放、不丢队列或恢复已撤权限。
- /diag显示队列、模型投递、代码致谢、合并、去重、过期、容量淘汰、点赞和分享计数。私有journal的assistantRouting每秒保存有变化的快照和最近64条原因；不是逐事件完整审计，原始归档仍是接收事实。写入失败显式提示并暂停助手。
- 隔离运行实测：1000条结构化互动没有任何模型prompt；两位观众相同问题保留，两条语义消息合为一次prompt；模型忙时再输入600条互动，原问题仍能进入待审。单条明确批准经过真实SendQueue和本地确认，不授全场许可。未调用真实模型或公开发送。
- 调试版实际PTY验证：/tmp/shisui-routing-ui-1789215276558/results.json。不同作者保留、同作者去重、分流日志写入、诊断页与点赞代码说明、人工草稿保持均通过；已目视检查两个VT画面，退出0、endpoint移除、自有ACP进程回收。完整菜单/65轮全量PTY属于历史证据，本轮未重跑。
- 当前代码隔离演练证据：/tmp/shisui-routing-runtime-final-1789215739221/results.json；耗时6.576秒，1条明确批准的模板经本地发送确认，0次真实模型/公开发送。一次性Rust运行探针已移除。
- 完整门禁：RUST_TEST_THREADS=1 ./script/verify.sh通过（fmt、Clippy -D warnings、349项测试、Release、version/help），随后./script/install_cli.sh安装成功。target/release/danmu与~/.local/bin/danmu的SHA-256一致：3e0f2a14457514e6188d3252c15885c87ba572c7f7c257737a0504d21b7df405。
- 安装版实际PTY：/tmp/shisui-routing-installed-1789216070917/results.json，退出0；代码说明、分流诊断、不同作者保留/同作者去重、人工草稿、assistantRouting归档均通过，两个原生prompt、无真实模型或公开发送。已目视检查安装版诊断与点赞说明的实际VT画面；endpoint移除，自有进程已回收，不干预用户终端。版本号仍是未发布0.4.5，请以本节哈希辨认构建。

## 上轮：单条发送隔离、批量待审与状态符号

- 只读核查当前助手专用会话发现echo_missing：接口返回但没有真实回显。此前Bridge.complete因此撤销全场许可。现在只终结该条和未发尾段，不重试、不生成修复，不撤销其他新消息的有效许可；真实传输未知、账号失效、换场及显式暂停仍按原安全边界处理。修复前/后回归分别失败/通过，覆盖后续新消息实际经过SendQueue并确认。
- Shift+Enter直接依赖Press/Repeat语义，不再维护可能因丢失Release而永久卡住的按住标志；长按重复不执行。待审页a全部发送、d全部丢弃，显式作用于当次待审批次；发送原子校验全部候选后再入原串行队列，不授全场权限、不批准后来候选。批量丢弃不触碰在途或不确定结果。冻结身份/场次、草稿/光标、编辑及覆盖层边界保留。
- 主界面仅保留公开回复前缀✦：绿为逐条确认，黄为自动偏好但无许可，红为自动且有许可；未开启/暂停时无内容无占位，Ctrl-G仍有完整说明。
- 完整门禁337项测试、fmt、Clippy、Release及CLI检查通过，已运行安装脚本。Release与 ~/.local/bin/danmu 的SHA-256一致：e57e1c9986bbc7a22acbea28ca18059b655fa720ac5bb69c2d88e3d8283c5d7a。
- 安装版PTY证据：/tmp/shisui-review-installed-1789172881478/results.json。批量批准进入实际LocalTransport并确认；全部丢弃不产生发送且保留不确定结果；丢失松键后的再次展开、重复事件忽略、草稿光标及绿/红/黄/隐藏四态均通过。已目视检查批量操作页和状态符号，退出0，endpoint移除，两个调试/安装验收的自有ACP进程均已回收，一次性探针已移除。同目录有真实VT HTML/ANSI和协议记录。未调用真实模型或公开发送；未控制用户现有终端。历史全套菜单smoke本轮未重跑，已迁移其中旧文字徽标的检查。

### 上轮方案的状态

上轮仅核查和提出分流方案；本轮已实现，当前行为与证据以本文最上方“代码致谢与高峰分流”为准。此前“关注/点赞进入LLM批次、落后512条会暂停”的说明不再代表当前内置助手调度；外部CLI/MCP原始窗口契约不变。

## 上轮：64轮上下文自动轮换

- 正常完成64轮后，只轮换独占ACP进程和模型上下文，不释放Bridge驱动所有权；因此保留消息游标、候选、正在发送的回复及当前有效许可。新上下文重新载入原生模型参数、所选规则与近期对话，不压缩旧会话、不重放旧消息。轮换时收到的新弹幕继续排队。
- 暂停取消自动续接；撤销许可不会被轮换恢复。新进程初始化、参数恢复、协议和资源上限等真实错误仍停机、撤权、显示原因，不自动重试。没有新消息不推理；持续值班仍消耗所选模型额度。
- 完整门禁通过：fmt、Clippy、333项测试、Release与CLI检查；随后安装到 ~/.local/bin/danmu。Release和安装版SHA-256一致：322ac1a84b31d51f3ff89dee1d8b38b8ea2e0158e299d73b32334ed6666937cb。4项轮换回归覆盖不重放、在途发送保留、暂停取消、撤权不恢复和初始化失败不重试。
- 安装版隔离PTY连续执行65轮：前64轮复用一个原生进程，第65轮使用新进程的全新上下文；模型fixture/b恢复、近期对话/草稿/许可保留，无重复处理、无空闲额外推理、无异常停机，退出0且endpoint移除。选中模型恢复允许宿主先创建过渡会话，因此按进程和实际prompt核对，不把session/new固定计数当作用户行为。
- 最终证据：/tmp/shisui-context-rollover-final-1789169971387/results.json；同目录保存协议记录与实际VT HTML/ANSI，已目视检查第65轮值班、轮换期间草稿和恢复后的模型选项。两个自有原生进程均已回收，一次性探针已移除。仅LocalTransport和模拟ACP，未调用真实模型、未公开发送、未操作用户现有终端。此前完整菜单/历史验收属于上一轮，不冒称本轮重跑。

## 上轮：启动历史、指令配色与功能总菜单

- 正常退出或异常中断后，读取当前房间最近一份含有效消息的归档，较新的空/仅临时活动会话和其他房间不会遮住历史。最多240条，标记为Archived，当时显示“历史”（当前已改为↶）；启动建立新会话，零新场计数，预载不进入Bridge或AI队列，不改写旧归档。B站History周期补漏路径保留；空/复用ID的不同历史正文不按ID删行。
- 54条指令显式归入外观、B站、OBS、AI、系统通用类别；危险项优先红色，选中背景不覆盖前景。中文搜索、全列表及OBS子列表一致，四套内置主题均已验证。
- 总菜单区分设置和当前操作，当时复制页Esc回到菜单（当前已移除独立复制页）；归档搜索必须输入关键词，取消不搜索、不影响Unicode草稿。/find 无参数也打开编辑器。当时保留了不重发、不授权AI的恢复入口，现已改为人工失败后接纳新输入并移除该入口；菜单退出沿用quit_requested传到主循环，不再只关闭菜单。新增退出回归修复前失败、修复后通过。

最终330项测试、fmt、Clippy、Release/CLI检查及安装全部通过。安装版完整PTY交互及生产入口四次重启均通过；整条验证/构建/安装/交互链耗时618.34秒。Release与安装版SHA-256一致：`283a36d001b82c2b35f136171d27e7e39bc6255ca2a9822f2ca4c4dd688ff3a2`。

最终证据：
- 完整PTY：`/tmp/shisui-pending-1789164323970/installed-final-1789166768519/results.json`，含四主题、菜单、关键词输入、五种尺寸、AI连续静默轮次、菜单退出；进程退出0，endpoint移除，自有子进程回收。
- 生产重启：`/tmp/shisui-pending-1789164323970/history-final-1789166768519/results.json`。隔离HOME，sandbox禁止所有出站网络；三次菜单正常退出为0，一次仅对自有测试实例SIGKILL后再启动。四次均有历史、新会话ID、Bridge游标0、无发送授权；旧归档字节不变。
- 同目录保存实际VT HTML/ANSI，已目视检查最终总菜单与崩溃后历史画面；此前还检查了窄屏退出、类别色与搜索反馈。一次性生产探针已移除，永久菜单smoke保留于script/agent_smoke.py。

未操作用户Ghostty、既有TUI或OBS；未调用真实模型、登录账号或公开发送。由用户重启正在运行的TUI使用新安装。

## 上轮：弹幕复制与AI停机诊断

- 当时的独立复制页（当前已移除）取当前选择/历史锚点，否则取最新弹幕，显示纯正文并释放鼠标捕获；复制期间不重绘，接收与助手后台继续。方向键/PgUp/PgDn/Home/End可读长文，Resize重排，Esc恢复捕获与原草稿光标；粘贴不执行。
- 完整ACP EndTurn且无assistant message不再被当作JSON错误；无答轮次继续复用连接。畸形输出、错误轮次与非完整终态仍拒绝。异常关闭一致撤权、保留首因并要求显式继续。该轮当时保留的64轮人工续接限制，已由当前自动轮换替代。
- 主界面和/diag显示当前原因，私有会话journal新增assistantStopped记录（时间、原生会话与原因）；失败如实提示，诊断记录不终结直播会话、不破坏恢复。手动继续后旧停机提示消失。既有日志没有停机原因，因此不能将历史每次停机都归因为空回复或64轮边界。

最终执行 `RUST_TEST_THREADS=1 ./script/verify.sh`：fmt、Clippy、326项测试、Release及CLI全部通过；随后 `./script/install_cli.sh` 安装到 `~/.local/bin/danmu`。最终安装版完整PTY smoke通过，耗时298.88秒，证据：`/tmp/shisui-copy-ai-smoke-1789162646295/results.json` 与同目录VT HTML。覆盖复制捕获释放/冻结/草稿、退出原因单次归档、两个连续静默轮次无重试/重启/重新授权、恢复后无旧停机提示及全部原有设置/安全路径；进程退出0、endpoint移除、自有子进程回收。已目视检查诊断与恢复后画面。

复制另在隔离真实PTY接入xterm.js 5.5.0，通过浏览器鼠标拖选及复制事件捕获到完整纯正文，确认选择保持、32×8长文尾部可达、Esc后滚轮恢复且复制历史锚点正确。记录：`/tmp/shisui-copy-ai-1789160741924/copy-ui-result.json`。未写用户系统剪贴板，也未操作用户Ghostty、TUI或OBS。原生Ghostty窗口本身未被自动化控制；完整smoke使用假ACP/LocalTransport，不代表真实模型或公开发送测试。

## 上轮：五类设置、精确指令与单次操作

本轮目标由 `script/agent_smoke.py` 在真实PTY与实际VT画面上走纯键盘路径；不使用左键或SGR Down/Up激活。覆盖Unicode草稿与光标恢复、五类设置、AI五个子组、本场运行、单次自动授权、候选全文/编辑/发送隔离、原生ACK与重选无操作、五种尺寸和退出清理。

本轮已通过 `RUST_TEST_THREADS=1 ./script/verify.sh`（fmt、Clippy、321项测试、Release及CLI检查），随后执行 `./script/install_cli.sh` 安装到 `~/.local/bin/danmu`。安装版真实PTY smoke通过，耗时288.59秒，证据为 `/tmp/shisui-five-settings-1789135456838/results.json` 及同目录VT HTML；进程退出0、自有子进程回收、endpoint移除。浏览器目视检查五类根菜单、120×36、80×24、50×24、32×16、16×6设置画面及32×16关于页；窄屏分类换行，极小尺寸截短提示但保留键盘操作。回归与验收共同覆盖显示指令显式保存、重复选择幂等、异步保存失败保留输入、草稿与发送授权隔离。

此前纯键盘版本的308项测试与244.80秒smoke记录位于 `/tmp/shisui-keyboard-1789085738909/`，仅属历史证据。

更早按钮版的215.54秒smoke证据位于 `/tmp/shisui-menu-buttons-1789074548434/`，同样仅属历史记录，不证明当前纯键盘版本或本次修订。

本轮未执行真实外部模型、宿主、B站登录/公开发送或直播间资料写入；真实直播间写操作未获授权。假OBS与LocalTransport只能证明隔离行为，不能冒充外部服务结果。脚本只回收自己拥有的进程，不触碰用户终端、OBS或真实网络。

## 上轮：历史恢复、名字颜色与统一AI设置

已执行 `RUST_TEST_THREADS=4 ./script/verify.sh`，fmt、Clippy、281项测试、Release及CLI检查全部通过，日志 `.local-archive/history-recovery-name-only/verify-final.log`；随后执行 `./script/install_cli.sh`。Release与安装版SHA-256一致：`947e73002fa4a9df90c062c21a6c42da71c2c89d0cb807e10b15eef53190db0f`。版本仍为0.4.5，由用户重启全部旧TUI进程后使用新结构；没有控制或终止用户实例，也没有直接修写用户正在使用的历史库。

安装版真实local PTY历史恢复证明：`.local-archive/history-recovery-name-only/installed-history/results.json`。空ID和复用ID的不同内容、冲突后的历史与新消息都保存；重复导入幂等，旧库和原归档未改。故意追加损坏JSON后仍索引健康归档并保存新弹幕，修复后可重试，最终10条记录、退出0、endpoint移除。没有真实B站发送或模型调用。

设置父子导航、返回选择、Home/End、120×36与40×20窄屏、运行菜单共享配置及不自动授权的真实PTY证据，连同一次性探针归档在 `.local-archive/history-recovery-name-only/ui-and-probes.tgz`。名字配色使用真实Crossterm渲染ANSI验证并目视检查：`assistant-name-only.html`及`.ansi`位于同目录，名字为#FFAF5F、正文为主题默认#F8FAFC；一次性捕获代码已移除。

Pi验收脚本已迁移到同一AI设置入口。安装版真实Pi0.85.1/适配器0.0.33验证 `.local-archive/history-recovery-name-only/pi-invalid-installed/results.json` 通过：选择不启动、不下载，无效原生默认不回退且零模型请求，全局扩展/技能未加载、原生文件不变；网络仅loopback，不使用真实凭据。

安装版完整交互复验通过：`.local-archive/history-recovery-name-only/installed-pty/results.json`。覆盖新目录中的模型回执、用途模板、回复策略、工作区历史、1209字节设置粘贴，及候选审核、人工草稿、发送授权、中文命令发现与通用显示设置。实际local PTY持续259秒，产品退出0，自有MCP/探针进程回收、endpoint移除；使用合成ACP和LocalTransport，不冒充真实公开发送。

门禁固定4个测试线程。首次默认高并发集成运行有8个Python ACP测试触发测试专用800ms请求超时；生产超时未改。首次门禁的281项测试已通过，但外层120秒命令等待在Release构建阶段到期；延长外层等待后的完整门禁通过，上述final日志才是交付证据。

## 历史构建与交互证明

最终执行 `RUST_TEST_THREADS=1 ./script/verify.sh`：fmt、Clippy、275项测试、Release及CLI启动检查全部通过，日志 `.local-archive/workspace-settings-final-verify-r2.log`。随后运行 `./script/install_cli.sh`；`target/release/danmu` 与 `~/.local/bin/danmu` 的SHA-256均为 `75873f5e0a710fe71790c1fdf56964022a9c920ca0f73b937c79072bfcf8b86f`。安装不替换已运行进程，由用户重启TUI生效。此轮保留了中文“搜索”补全入口的行为回归。

最终安装版完整隔离PTY也已通过：`.local-archive/workspace-settings-installed-75873f5e/results.json`。覆盖设置保存、1209字节中文粘贴、当前模型回执、长候选审核、草稿与权限隔离、旧归档导入、中文命令发现、分组设置入口及持久帮助的焦点保护，最后 `/quit` 正常退出0；所有自有进程回收、endpoint移除。该通用交互证明使用合成ACP/LocalTransport，不冒充下方真实Pi或公开发送证明。

实际PTY设置/命令/粘贴证明归档为 `.local-archive/workspace-settings-ui-proof-final.tgz`：包含完整10组设置场景、最新120/80/60/40列截图与导航、暂停仍在途时模板拒绝/轮次结束后显式保存、原生只读/排队/回执、工作区三区及1382字节设置粘贴。4096字节完整接收，4097字节整次拒绝并保留原输入；早期失败来自探针PTY短写，write_all修正后通过，未用缩小产品上限掩盖。均为隔离HOME/XDG及本地传输；不冒充付费模型、真实账号或B站发送。

最终安装版真实Pi0.85.1 / Node24.4.1 / pi-acp0.0.33的免费搜索、默认模型/档位、模型切换、取消与工具拒绝闭环通过：`.local-archive/pi-search-installed-75873f5e/results.json`。只有一次Exa为真实外网查询，推理为loopback。无效原生默认在主动消息后仍零模型请求，且清理完整：`.local-archive/pi-invalid-installed-75873f5e/results.json`。观察到的自有进程全部退出，endpoint删除并拒绝连接；不以短命进程来不及显示名称作为失败判据。

已登录GPT专项另归档为 `.local-archive/pi-options-gpt-proof.tgz`：真实Pi0.84.3/适配器0.0.33、虚构OAuth与257个loopback模型，加7个原生GPT共264项；启动前选择另一GPT及目录尾项均有完整回执、零推理/零授权，显式启动后仅新弹幕触发loopback候选。用户问题的直接阻断是旧联网策略拒绝Pi及缺少仅连接入口；128项上限是另一个会拒绝整份大目录的边界，不冒充原GPT缺失原因。

### 先前构建参考

以下1e94a1f3产物是上一次Pi接入的历史证据，不替代本轮最终验收：当时`RUST_TEST_THREADS=1 ./script/verify.sh`通过fmt、Clippy、230项测试、Release与CLI检查，日志`.local-archive/pi-support-complete-verify.log`；安装SHA-256为`1e94a1f36cf81518f8a50f9f68150791ef2de010441529d2a84c8767b2e93225`。

该历史安装版的通用隔离PTY证据为`.local-archive/pi-support-installed-1e94a1f3/results.json`，仅证明当时交互；使用fixture，不冒充真实宿主或B站公开发送。

历史原生Pi0.84.3 / Node24.4.1 / pi-acp0.0.33的12项闭环位于`.local-archive/pi-native-installed-1e94a1f3/results.json`；无效默认0HTTP证据为`.local-archive/pi-invalid-installed-1e94a1f3/results.json`。使用隔离HOME、虚构凭据、仅loopback网络；所有自有进程退出，endpoint移除。新版真实Pi0.85.1免费搜索链路另已通过`.local-archive/pi-search-native-proof-04/results.json`：仅Exa为真实外网搜索，推理仍为loopback，不读取用户凭据。

上述 verify_pi、verify-native-text-hosts 与 check_amp_native_policy 是已退役原生链路的历史重跑方法，不再作为当前源码入口。当前验证见下文；旧一次外网 Exa 查询不是新 HTTP 搜索验收。

安装版另验PATH中没有pi-acp、仅应用私有适配器存在时仍可发现并保存Pi选择，且不启动原生进程：`.local-archive/pi-private-discovery-installed-1e94a1f3/results.json`。本机已运行 `danmu setup pi`并验证重复调用只复用，无全局安装或Pi配置改写。

## 正常路径：在danmu里设置助手

1. `/settings`打开“设置与操作”，四类及`/settings account`、`/settings obs`、`/settings ai`、`/settings system`直达入口保留。方向键/Enter/Esc导航，F1或?说明；`/commands`或Ctrl-O搜索并保留草稿/光标，Ctrl-G开运行面板，Ctrl-P暂停。旧弹幕↶不进AI队列。
2. AI助手 → 模型或`/ai model`选连接。API Key协议填写基址/模型，用“设置/更换 Key”或`/ai key`打开隐藏输入；命令不收明文参数，旧Key不回填、不进普通字段/Journal。Enter只保存，Esc恢复Unicode草稿与光标；失败保留秘密输入，空值不是清除。“清除 Key”另选保留/清除。
3. 订阅页由本人完成Continue with ChatGPT浏览器授权；登录保存后或显式选已保存账号，自动单次读取目录并打开模型选择。首次明确选一次；同注册重登录保留仍可用的模型并定位当前项，已不可用的选择清空后再选。读取失败保留登录与已有选择，手动“读取账号模型”用于刷新/重读，无自动重试或计费回退；不自动选新模型、启动或授发送权。取消、换场、发送账号变化或许可门禁变化后的迟到目录不应用。操作待完成不换连接、不重复登录；取消只终止本地任务，不称远端取消。
4. `/ai start`准备本地资料/历史/凭据/worker，无新消息零模型HTTP、不隐式读模型列表、不做付费连接测试。“本机就绪/等待新消息”不表示远端鉴权已验证。`/ai visible`纳入未消费可见消息，不重放旧批；`/pause`取消轮次并撤权，`/ai stop`另释放驱动，清退完成后再启动。
5. 更换API配置、Key、订阅账号或联网开关要求已暂停、无flight、无清退、无账号/模型操作、无发送许可；TUI待续用也拒绝，先`/pause`。重选当前协议不重置地址/模型，换协议恢复官方基址并清空模型，不删除或跨作用域发送旧Key。普通偏好/方案保存不推理、不授权。
6. 发送模式或`/ai auto on`须本人明确允许公开发送，`/ai auto off`立即撤权。同房间/发送UID核验通过、automatic与resume_on_start允许才续用；旧迁移关闭两者，不继承授权。换房/身份变化/失效/暂停撤权；独立助手号失效不回退主号，B站发送身份与模型OAuth分离。
7. `/review`显示全文，E编辑、Delete丢弃、Tab下一条、Esc退出；编辑中Enter/Shift-Enter只保存，全文看完Shift-Enter批准当前版本。a/d仅当前待审批次、不授未来许可，不重发在途/不确定结果，批量先整批校验。人工草稿保留。
8. `/ai replies`仅方案/联网搜索/屏蔽词，`/ai materials`仅名字/人设，`/ai advanced`仅处理可见弹幕/结束；`/ai range`、`/ai topic`仅本场，`/ai workspace`与`/ai reset`删除。外观仍走Tab及`/display`，低频文件项修改后重启。
9. OBS独立：`/obs stop`默认取消、确认后3秒内可Esc取消；退出不停止推流。`/room title [文本]`和`/room cover [本地路径]`写入须核验B站主号为房主，平台受理不称已经展示，local拒绝真实写入。主界面终端原生拖选复制，不改变草稿或授权。

联网默认关闭，选方案后重置关闭。每轮最多一次Exa/三条来源/4000个Unicode标量/20秒且受全轮deadline约束；程序只发送冻结本批完整公开Danmu/SC正文，不扩写私有资料/历史，不带UID/昵称。工具参数只能message_id，额外query/多个调用/未知工具/旧ID整批拒绝、零搜索。Unavailable只补全一次，候选强制待审，不重试/付费回退，不承诺公共额度或永久免费。

有限修复固定开启：仅明确内容拒绝或已知词命中，最多一次、只改写未确认尾段，repair无搜索。未知送达/缺回显/POST超时不改写不重发，确认段仍为真实送达事实，保留原结果与retry_of/confirmed_segments/unconfirmed_segments/duplicate_risk；仍受原审核/许可约束，不给外部caller修复权。

## HTTP 协议、依赖与传输

使用现有 reqwest/tokio/serde_json 和普通enum分派，不依赖 ACP/供应商coding SDK或通用Agent框架。Rust≥1.89；本节未重新实编MSRV。模型/搜索客户端独立于B站Cookie客户端，禁重定向、底层重试，loopback禁代理，不转发Cookie/Bridge token/宿主认证/开发环境Key。

| 连接 | 契约 |
| --- | --- |
| `openai_chat` | 基址追加`/chat/completions`，Bearer Key、stream:false、system=应用CONTRACT/user=本轮JSON；只发选定max_completion_tokens或max_tokens。要求object=chat.completion、唯一choice index=0/assistant，stop正文准入；length/content_filter/refusal整批拒绝。不含Responses、Azure认证或所有厂商变体 |
| `anthropic_messages` | 基址追加`/messages`，x-api-key/anthropic-version:2023-06-01、stream:false、顶层system/user本轮JSON、固定max_tokens；要求type=message/assistant，end_turn合并text；max_tokens/refusal整批拒绝，无thinking/beta/server tools |
| `chatgpt_subscription` | 固定官方`https://api.openai.com/v1/responses`、OAuth Bearer、instructions=CONTRACT/input=user本轮JSON、store:false/stream:true；无previous_response_id，不发preview不支持的输出上限/temperature等字段。只有正确归属完整response.completed终态整体准入，delta不入队，reasoning不当候选 |

CONTRACT+已序列化本轮JSON合计≤24KiB UTF-8字节，不是token量；推理HTTP请求另限48KiB，两种非流式API及HTTP错误解压响应≤256KiB。订阅成功SSE逐chunk/逐行处理，仅保存当前事件和待EOF终态：单事件含字段/注释/换行≤256KiB、全流≤8MiB、最多4096个数据事件（含终态与[DONE]），不将重复增量全流误当单条正文；终态后非法事件、断流或取消仍不准入。账号模型目录成功响应独立限8MiB解压后字节、1024项/256可见项，严格单遍投影。连接10秒；普通轮次共用最早原消息120秒与180秒操作上限，含搜索和补全；普通到期expired并继续新消息，repair超时暂停撤权。

无正文/零候选正常结束，空轮5–10秒合批保留。完整可归属envelope中候选格式/轮次/目标/数量/正文错误或越权工具整批rejected，不重试，保留旧候选及有效许可。HTTP非2xx、DNS/TLS/断流、外层JSON/协议不匹配、超限、worker归属/真实存储故障明确暂停撤权，处理后显式启动；不吞成无答，不自动重启/模型或协议回退。诊断只有稳定类别/有界安全摘要，不回显原始模型正文、headers、prompt或错误响应。

底部Info/Success提示3秒、Warning/Error警告或错误15秒消退，Progress保留到操作完成；expire_notice_at仅清临时文字，不清runner.note、/diag、Journal、待审或Unicode草稿，不恢复已停worker/发送许可/续用意图，也不跳过OBS停止确认。

Exa MCP固定`https://mcp.exa.ai/mcp?tools=web_search_exa`，initialize → initialized → web_search_exa（numResults=3）。每响应96KiB，JSON/SSE按完整事件/请求ID解析，不把网络chunk当事件；失败、无结果、超限为Unavailable。唯一工具只允许message_id，模型不提供query；关闭/无合格目标/repair不提供工具，最多一次搜索与第二模型请求，第二次移除工具。tool result是非可信数据、不升为system指令。

官方参考：[Chat Completions](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)、[Anthropic Messages](https://platform.claude.com/docs/en/api/messages/create)、[Sign in with ChatGPT OSS/local](https://developers.openai.com/siwc/token-sharing-open-source)。

## 动态配置与上下文边界

非敏感`assistant.json`与实际`config.toml`同目录，当前format=4。最小合法文件如下（模型/凭据未配齐，不能启动；其余字段采用默认，不将Key写入本文件）：

```json
{
  "format": 4,
  "api": {
    "protocol": "openai_chat",
    "base_url": "https://api.openai.com/v1",
    "model": "",
    "max_output_tokens": 4096,
    "openai_token_limit": "max_completion_tokens",
    "chatgpt_account": null
  }
}
```

协议另可为anthropic_messages/chatgpt_subscription；Anthropic官方默认基址为`https://api.anthropic.com/v1`。基址含版本路径，trim后经url库唯一规范化host/IDNA/默认端口/IPv6/尾斜线，不丢前缀；≤2048字节，禁止认证/query/fragment/控制字符，仅HTTPS或127.0.0.0/8、::1字面量HTTP，localhost不例外。模型trim后1..256字节/无控制字符；分步保存可空地址/模型，启动要求齐备。max_output_tokens=256..8192，OpenAI只发选定token字段、不遇400偷换；Anthropic固定max_tokens。订阅基址固定官方，chatgpt_account仅本地注册ID，模型须显式选账号目录；两个输出字段保留结构但订阅不发。

加载≤32KiB、不写盘，未知版本/字段/嵌套字段、错类型或损坏拒绝，不静默覆盖。无format/format2/3按各自严格schema迁移：无版本style/scope沿旧规则合并preferences，format2旧topic丢弃，format2/3业务字段按规定逐项保留。host/binary/source/native_preferences、模型/thinking/OAuth仅解析后退休，不发现宿主、不读原生规则。automatic/resume_on_start/web_search迁移为false，format4日常加载不重置三者。首次显式保存先将原始字节存`assistant.pre-api.json`，同内容可复用，不同内容拒绝覆盖；备份失败不提交，设置私有原子写入成功才更新内存。旧备份/Journal/SQLite/资料保留。

workspace/skills/preferences与输出项为文件级启动读取，外部修改后重启。use_profile（公开简介，不是OMP profile）/use_project/dynamic_host_priority/repair_blocked固定true，旧false仅内存归一化。必要原生规则由本人放入既有SYSTEM/人设/PROJECT/preferences，不自动扫描复制。自动许可仍独立按房间/发送UID核验，不由配置观察恢复；普通偏好本轮冻结/下轮生效，自动排队须当前策略与冻结快照均准许且permit有效。


### 用户可编辑项目与能力边界

默认工作区为`~/Projects/danmu-assistant/`，实际路径及状态见`/diag`；路径改为由`assistant.json`的`workspace`在启动时读取，不再提供“工作区与记忆”菜单或`/ai workspace`。AGENTS.md自动维护带标记规则块并保留自定义区，拒绝损坏标记、链接/硬链接与越界，不覆写用户资料；准备自有工作区的原权限不变，WORKSPACE.md为人类入口。SYSTEM、选定人设及已有PROJECT默认引用，已有公开简介由程序按原有边界提供；`use_project`、`use_profile`固定为true。选中文本总量8KiB、技能最多16项；技能仅按`assistant.json`的`skills`明确选中相应`skills/<名称>/SKILL.md`，默认空，未选技能不加载、不执行代码。不读`.danmu`作为指令、不扫描其他文件；缺失、超限及读取错误仍按原边界处理，不回退旧资料。

外部维护窗口先读工作区AGENTS，只改本人允许资料，聊天不生效。1秒核对选中文件变化，取消旧轮HTTP/搜索、丢旧在途结果，旧自动待发/permit失效；待审/人工稿保留需重审、自动须重新授权。下一新批重新带完整资料；已POST按真实平台结果收束、未发尾段停，缺失/错误不沿用旧值，修复后核对。“已载入”只证明应用输入，不证明模型效果。

`.danmu/history/<房间>/history.sqlite`为持久索引；旧私有库按SQLite事务导入已提交WAL数据。平台ID仅是源标识，以原ID、秒级时间、作者UID、昵称和正文的完整tuple生成独立存储身份；空ID或复用ID的不同版本分别保留，完全相同记录幂等。旧id主键模式在IMMEDIATE事务中迁移，保留原始字段和归档导入进度，重建FTS；失败事务回滚。检索按内部rowid去重，返回/排除仍使用原始ID，不擅自判定冲突哪版正确。

全量历史索引与副本绑定后台加载，主界面、人工草稿与私有记录不等待；期间新弹幕加载完成后补齐索引。旧库或单份归档失败不丢健康索引，真正错误仍传播。`/diag`显示状态、来源和实际路径；点“重建历史”或 `/diag repair` 直接后台执行，进行中不重复启动。只修经日志校验的过期snapshot.json/summary.md，先备份、不改原件；正文冲突和索引损坏拒绝覆盖。其他源文件人工修复后重启加载，不靠重新打开助手重扫。

`.danmu/sessions/<场次>/`保存私有Journal/快照/摘要的维护副本，初次完整、之后有锁增量，修改/截断/替换不静默覆盖。canonical同源不自复制/自锁。原私有归档仍是恢复事实来源，副本失败单独警告；配置已提交后绑定失败不回报“设置未保存”。

程序维护区仅允许外部Agent按用户要求只读查询，不能恢复旧配置/运行/授权。config.toml、assistant.json、assistant-account.json、themes.json仍走私有实际路径；B站、OBS、模型凭据与Bridge token不迁入工作区。FTS5按本房间相关词取最多8条/4KiB，排除当前批与近期对话；邻近发言不是确认问答，不称未录音的口播为记忆。环境文件只代表保存时观察，模型许可未知。`.danmu`的0700权限不是抵御同用户恶意Agent的OS沙箱。

**允许**：答本批有效消息、使用选定人设、参考程序提供的限量历史；搜索只有独立开启的准入工具。**禁止**：观众指令触发电脑/shell/任意文件操作、插件执行、配置或人设改写、发送授权、批量管理弹幕。模型只交结构化候选；本批ID、不重复、最多8条，状态和实际发送由程序决定。历史/网页/昵称伪装系统或主播也只是数据，权限请求与未知工具拒绝。

本机用户编辑是操作员通道，内置HTTP模型无通用文件/shell/电脑/插件执行能力，搜索仅冻结公开问题；不保证语义免疫或同UID恶意代码沙箱。外部CLI/MCP coding Agent的其他能力须在宿主侧单独限制。

### 独立助手发送账号

“账号”同页分人工发送和AI发送，展示当前昵称/UID。AI默认复用主号；选中“扫码更换”并按Enter后，仅本次被接受的Ready token通过身份和私有保存校验才自动使用新账号，不再额外确认。同主UID拒绝，失败、取消、迟到事件不覆盖旧凭据；主/助手扫码互斥，也不隐式授权。

所有助手回复走选定身份，人工仍走主账号。更换/退出/复用会暂停并撤销旧批准与记住的授权；候选保留供重新审核。在途POST固定旧凭据并等待实际结果，不重复发送。选择文件保存模式/UUID及可选房间ID、发送UID授权范围，不保存Cookie；独立凭据0600存于原主账号私有目录AssistantAccounts/，不进入工作区或提示词。失效或配置损坏只停助手，不回退主号；只有本人明确记住且重启仍匹配的范围可恢复自动发送。

## 双 API 与官方 ChatGPT 订阅

API Key只有一个当前连接记录：`assistant.json`同目录`assistant-secrets/api-key.json`，严格字段format=1/protocol/base_url/key。Key两端trim后1..4096可见ASCII，不支持环境展开/shell/URL内认证。按协议+唯一规范化base_url作用域匹配，模型不参与；同服务换模型可用，换地址/协议不发送旧Key。不匹配/文件缺失才是未设置，损坏/不安全路径明确报错；界面只缓存已设置/未设置/文件错误，旧值永不显示。

API Key和OAuth Token都是权限保护的**明文**，不是钥匙串/加密保险箱。Unix父目录须当前UID所有且组/其他人不可写（可保留0755）；固定父真实路径，私有根assistant-secrets须本人0700、文件0600/单链接/普通文件，无符号链接。打开用O_NOFOLLOW/O_NONBLOCK并fstat复核、有界读，写用私有原子替换、失败保旧；宽权限拒绝，不自动chmod。Windows沿既有安全能力，不声称Unix等价ACL，不防同UID恶意进程。禁止提交/同步/给外部Agent读凭据，不进工作区或Journal。

保存/更换Key替换唯一记录，清除只删该文件，不删目录/旧宿主认证/订阅账号；成功失效旧idle handle，下次启动重新读取。运行中Key为启动快照，手工编辑/删除不实时改变已发请求：先暂停清退后启动，服务端撤销另行处理。取消本机future不证明远端推理或收费停止。

ChatGPT采用官方OSS/local **Sign in with ChatGPT preview**，不是旧Codex/ACP OAuth、不是读取用户原生宿主凭据。当前官方资格面向Plus/Pro，实际以账号授权`chatgpt.tokens.use.direct`及服务返回为准：登录可保存但缺该scope不能推理，不保证所有账号/地区/模型可用。模型由本人显式GET官方`/v1/models`选visibility=list目录，不捏造目录、不自动付费fallback；账号模型缓存不跨账号。额度与ChatGPT共享并受每应用限制，见[Usage](https://chatgpt.com/settings/usage)；不承诺无限额度、永久preview或读取旧聊天历史。

OAuth固定官方auth/OpenID/JWKS端点，loopback随机端口回调、fresh state/nonce、PKCE S256、签名/issuer/aud/exp/subject验证，保留issued client与账号绑定。稳定host ID和隔离多注册明文存在`assistant-secrets/chatgpt.json`；跨进程锁串行refresh及旋转Token原子替换，不从环境读Token，Token/授权URL（含id_token_hint）不进日志。本人在系统浏览器授权，应用不替用户同意；登录任务完成后先生命周期门禁再提交。失败/取消保旧；若Token已提交但设置保存失败需说明部分落盘，不能假称全回滚。

退出当前账号清本地Token但保注册映射/host ID，远端revocation是否确认按实际结果显示，可另在ChatGPT设置管理授权。不把“本机退出”写成“官方已撤销”。401/403/429/503、刷新/资格/共享额度等错误安全暂停撤权，保留原始稳定错误和清理失败；额度错误保Token，提示Usage，刷新失效提示重新登录。只有白名单code/HTTP/request ID安全摘要，无raw detail，无同批重试/模型回退/API计费 fallback。

## 无状态轮次、取消与恢复

一条应用driver、单飞有界worker，未发新消息不请求模型，不保存供应商会话或跨轮messages；只有同轮搜索续接携带唯一已准入tool call/result。取消包围整个HTTP/搜索/刷新future，排队取消零HTTP，迟到结果、代次/场次错归属、暂停后结果不入队。pause先停分流、递增generation并撤权，再停止自有worker；保留业务场次、待审与人工草稿，stop另释放driver，shutdown等待自有任务，无原生进程树或“重建上下文”要求。不能宣称远端推理/计费已停止。

每轮完整operator_context、名字/preferences、人设、topic、audio_available=false、本批问题、房间公开信息、近期对话、相关历史与actual_delivery_results；不做静态资料unchanged省略。资料8KiB、本批≤8条/12KiB、近期≤12条/4KiB、相关历史共4KiB（观众≤3KiB，每UID≤3条、全批≤8条、每条240字符），均受24KiB应用预算，优先去最旧近期参考，不截断当前问题凑预算。UID/房间精确隔离，只取本批之前；数据库损坏/读取失败不伪装空记忆。

Confirmed段无论整体状态Confirmed或部分Rejected都是真实已发，Uncertain/未确认段不能说送达；待审/丢弃候选不当assistant发言。Journal和SQLite继续本地保存，不迁到供应商会话。history::import_journal仅索引SessionStarted/EventReceived，不导入AutoReply；跨进程既有观众/场次历史与Journal回复保留，但不保证所有旧助手发言进检索。仅在原生宿主未归档的聊天不迁移。

round_completed写api_protocol/requested_model/returned_model/search_state及原轮次/outcome/耗时/输入字节/受理事实，错误未知值null，旧Journal不改写；assistantStopped新记录只有reason，不写native_session。配置观察仅白名单协议/模型/输出项，不含base_url/Key/Token/凭据路径；不能据观察恢复授权。候选先round journal落盘再take_ready发送，写失败显式停撤权，不改真实已确认结果。

## 业务通道与独立CLI/MCP

worker实际Started本轮后才将选中消息标Processing；自己/本地回流、未选中/禁用事件不标，无答灰/真实生成失败红。候选/在途/实际结果优先，取消不跨场污染，只有真实发送确认才能绿。

候选与修复均进入原Bridge和共享发送队列。Confirmed才算确认，网络不确定不重试；有依据的可修复失败才新建一次候选，原终态不改写，附diagnosis、retry_of、分段记录与重复风险。外部caller不能获得Runner模型修复权，真人仍独立。

Runner持有时`status.driver=runner`；只排斥同一内置助手caller `danmu-assistant`的外部report/reply，避免双驱动。不同独立caller仍按既有权限合法回复，不做全局封禁；不得换caller绕过同一助手互斥。只读与结果查询保留。用户切换同一助手前先停旧外部值班，danmu不抢停其他进程。

独立接入读取[danmu-duty Skill](../agent-package/skills/danmu-duty/SKILL.md)。`python3 script/agent_config.py --host HOST --binary /实际固定/danmu --instance /明确私有实例`仅打印原生配置。Amp保留amp.mcpServers，Pi保留CLI例子；不假造MCP，不改共享角色，不恢复旧OMP定时唤醒扩展。

## 本地验证与真人待验

获本人授权后：`/实际固定/danmu --instance /全新绝对私有目录 local --assistant`仅打开面板。本人输入`/event 姓名 正文`、`/local confirmed|uncertain|rejected`、`/session new|end`，`/quit`或SIGINT正常退出；禁止房间、登录、OBS、生产配置混用。

当前验证应运行真实loopback HTTP/搜索/OAuth/Responses回归及真实PTY，再统一门禁/安装哈希；没有本轮完整证据前不记通过。官方登录/真实推理与搜索仅本人另行明确授权后做，不读或消耗既有原生账号额度。以下04/FIX01仅历史，不是当前HTTP链路验收；开发自动化不是真人直播验收。

## 历史 FIX01：G5现有入口事实与独立验证边界

**不存在现成的固定产品非键鼠ACP完整控制接缝，G5及相关独立L继续阻塞。**

- `src/config.rs::Command`仅Local/Agent/Mcp；`src/main.rs::run`将agent操作解析成Bridge Request。Request与MCP工具仅status/messages/report/reply/result，没有runner start、事件注入、批准或transport选择。
- `src/terminal/agent.rs::run_local`要求新的空私有目录；`--assistant`只打开详情。`/event`、`/local`、`/session`及助手菜单中的开始/批准属于TUI输入处理，不是外部CLI/MCP操作。
- `src/runner/tests.rs`、`src/terminal/agent_tests.rs`通过cfg(test)直接使用Runner/TerminalApp和受控peer；只证明开发者执行的回归，不编入fixed danmu，不要求QA编译，不新增隐藏控制口、预置配置绕过或键盘注入。

现有合法零输入路径只能启动空local、观察默认/详情及查询只读状态。须全新0700私有root、隔离HOME/TMP、准确fixed绝对路径及独立可用TTY（探针PTY不得写任何字节，包括DSR回应）。创建ROOT及其home/tmp后启动：

`env -i HOME="$ROOT/home" TMPDIR="$ROOT/tmp" PATH=/usr/bin:/bin LANG=en_US.UTF-8 TERM=xterm-256color "$BIN" --instance "$ROOT/instance" local`

可另选`local --assistant`只打开详情；同一明确实例查询`"$BIN" --instance "$ROOT/instance" agent status '{}'`，预期local_transport=true、sending_enabled=false、cursor=0；`mcp`可列五工具及查询，无生成控制能力。只对本次PID发SIGINT退出，确认退出码、端点删除、端口关闭和锁可重取。

本轮F01/F02由Dev生产SDK受控回归及Ratatui buffer证明；buffer状态切换不是真人实际表面U。独立QA可达的空实例L不扩展为SDK动态链路L；C16/R-C16、九原生宿主/模型、Rust1.89实编继续未验。真实非生产键鼠由Lead另行组织。

