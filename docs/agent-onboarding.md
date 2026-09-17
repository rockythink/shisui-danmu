# 弹幕助手接入向导

当前适用 v0.5.0。面向用户的任务式操作见[使用手册](https://danmu.elazer.wang/guide/)；本文保留开发接入与历史验证记录，历史构建号及旧轮次行为不代表当前版本。`/settings` 是“设置与操作”总菜单；主界面原生拖选后用终端快捷键复制，旧弹幕标识为↶。当前宿主与认证约束见[九宿主接入与认证边界](#九宿主接入与认证边界)。

## 本轮：空回复可诊断，普通消息低成本合批

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

重跑：`python3 -B script/verify_pi.py --binary ~/.local/bin/danmu --adapter <pi-acp入口> --output <新证据目录>`。`--invalid-default-only`验证未知默认不回退；`--search-live`只允许内置搜索、一次真实免费Exa查询，推理仍用loopback。脚本不安装依赖、不读用户凭据；Pi/Node不在PATH时用`--pi`/`--node`指定。其他原生安全检查见`script/verify-native-text-hosts.py`与`script/check_amp_native_policy.py`。

安装版另验PATH中没有pi-acp、仅应用私有适配器存在时仍可发现并保存Pi选择，且不启动原生进程：`.local-archive/pi-private-discovery-installed-1e94a1f3/results.json`。本机已运行 `danmu setup pi`并验证重复调用只复用，无全局安装或Pi配置改写。

## 正常路径：在danmu里设置助手

1. 输入 /settings 打开“设置与操作”：设置分组为外观与显示、B站账号与直播间、OBS、AI助手、系统与关于；当前操作可进入本场 AI、重点消息、关键词搜索归档、指令搜索和退出。方向键、Enter、Esc导航，F1或?说明；/settings reading、account、obs、ai、system仍可直达。/commands或Ctrl-O中文搜索并保留人工草稿和光标，Ctrl-G/P/C仍可用。
   历史索引支持Swift v1与现行Rust归档，包括恢复旧场次后追加的混合记录；保留旧主播UID并按房间隔离。不需要删除历史文件来解决`missing field roomId`。
2. “AI助手”分工作区、模型、发送策略、人设与资料、Agent配置；`/ai workspace [路径]`、`/ai model`、`/ai replies`、`/ai materials`、`/ai advanced`可直达。只在进入模型工具选择时扫描宿主；连接仅读取实际原生选项，不推理、不值班、不授权。本场启动/暂停、范围、主题和待审回复仍由`/ai`管理，不写入下次默认。
3. “发送账号”只跳转“B站账号与直播间”，不复制账号设置。“系统与关于”提供诊断、帮助、版本0.5.0、创始人Elazer及官网https://danmu.elazer.wang；退出位于总菜单，OBS拥有独立分类。公开助手回复每段带标记且不超过40字素。
直达显示命令为 `/display theme`、`/display names on`、`/display names off`、`/display time on`、`/display time off`、`/display layout chat`、`/display layout list`；直播间资料命令为 `/room title [文本]`、`/room cover [本地路径]`；OBS连接配置增加 `/obs config host [主机]`、`/obs config port [端口]`，并保留现有mic/password/scene与连接、静音、推流控制；`/about`直达关于。可选参数省略时打开编辑器或选择器，给出参数时直接提交，但只有校验通过且收到真实保存/服务回执后才显示成功。标题和封面写入前必须验证当前B站主账号就是房主；平台受理后仍可能待审核，不称已经展示。
4. OMP默认使用私有设置目录与执行目录，仅复用原生认证，不继承全局规则或聊天；显式选中profile才主动增加其人格/模型上下文。除单独允许的搜索，其他工具、hooks、插件、自动skills、记忆扩展与MCP禁用。用户项目中明确选中的技能仅作为文本读取，不执行代码。
5. OMP也可选择现有UTF-8 system规则文件绝对路径：读取非空且最多32KiB内容，生成本场私有快照并经`--system-prompt`加载，不把路径当提示词、不执行脚本或目录。底层加载效果未获模型实证，其他宿主不伪造该能力。
6. 默认逐条发送。进入“发送模式”显式选择自动/逐条，或用 `/ai auto on|off`；自动选项旁固定展示公开发送及同房间同账号记住的后果，无额外确认。有效身份校验后才保存范围授权；不会暗中启动模型。相同房间/发送UID重启后台核验后恢复，换房间或账号须重新授权。关闭先撤权再保存；显式暂停、账号变化和登出清除记录，旧回复不跨场补发。
7. 普通切换选中后按Enter直接保存；选中当前值不重复提交。原生选项仅完整ACK后更新，高亮不是当前值，未知/不支持项只读。方案一次保存十项偏好，F1“说明”可展开差异；仅暂停且无在途轮次可用，不改账号、模型、自动模式和续用意图。文本编辑用Enter保存、Esc取消，保存失败保留内容。
8. 待审页显示全文。E编辑，编辑中Enter或Shift-Enter只保存；Delete丢弃，Tab下一条，Esc退出。完整看过正文后Shift-Enter批准当前版本；长文未看完时只展开。另有 a 全部发送、d 全部丢弃：当前批次显式批准或丢弃，不授全场权限，不包括后来新到候选，不重发在途/不确定结果。批次审批先完整校验，失败不部分批准；身份/场次变化需重新查看。按Press/Repeat区分新按下与长按，不以丢失的Release永久锁键；覆盖层与编辑焦点隔离底层操作。
9. 无有效范围授权的旧续用意图不再阻塞启动，显示“AI已暂停”和“继续AI”；执行 /ai start 一次启动，但不把旧automatic偏好当许可。有效同范围授权仍后台核验后恢复。主界面核验和连接阶段显示轮转“AI启动中”，真正就绪后显示 ✦：绿色逐条确认、黄色偏好自动但未授权、红色已授权自动；未启动/暂停则无符号和占位，Ctrl-G仍显示完整说明。
10. 只有 `/obs stop` 二次确认，默认取消，确认后仍有3秒可取消倒计时；退出弹幕台不会停止推流。其他启动、结束、重建、退出账号单次执行，危险动作有红色动词和后果说明。`/help`持久可滚动，Ctrl-P/Ctrl-C仍有效；粘贴不执行或发送，扫码/停止确认不收粘贴。人工失败不再需要手动解锁；旧任务和未发尾段不重放，后续新输入仍须通过身份检查。

联网在“发送策略”单独开启，默认关闭。OMP/Gemini使用各自服务与额度；Pi内置web_search使用Exa免费公共入口，每轮一次、三条、4000字符、20秒；失败不重试、不跳转、不转付费。保存联网选项不会开启自动发送。

有限改写只允许明确内容拒绝或用户已知词命中，最多一次，只改写未确认尾段。接口返回但缺少真实回显属于结果不确定，不自动改写或重发；仅隔离这一条及未发送尾段，保留后续新消息的现有发送许可。AI的POST传输未知、账号失效、换场、手动暂停等其他安全停发条件保持。原对象、场次、许可与最低审核要求保留；先审后发仍需重新批准。提示按发送生命周期更新，原Result及诊断保留用于核对。

### 回复积极性：允许不参与

在“助手回复”切换积极性：谨慎（默认，明确叫助手/追问为主）、均衡（也答明确独立知识问题）、积极（可有用补充）。不是固定比例或速度。“开麦让位”“静音补位”默认都开，可各自关闭，保留基础档位。

所有档位都允许不回；观众可能在与主播语音交流，闲聊接话、情绪表达或意图不明优先留给主播。没有合适对象返回空candidates，正常结束本轮，不报生成失败、不凑答案、不补发。档位保存后下一轮生效，不回放已跳过弹幕、不改在途快照、不授权自动发送；感谢仍受独立互动开关控制。语境判断取决于所选模型，不声称程序能听到主播或保证每次判断准确。

每轮附带当前批及最多12条/4KiB之前对话，用UID区分已知主播、助手和观众；昵称不能认证，原生回复目标仅为名称，不伪造@/被回复消息ID。要求区分助手、主播、其他观众或未知对象；上下文不回放成新候选。Mic只采用5秒内OBS静音观察，未知/过期不猜；未静音不是正在说话，没有ASR或说话人识别。对未知口播不补全事实。动态策略在途改变时，旧候选只能待审，不自动发送。模型语义效果仍需本人实际使用评估。

## 官方SDK、依赖与传输

使用官方Rust crate `agent-client-protocol = "=2.1.0"`，Apache-2.0，默认features关闭、解析feature集合为空；配套schema 1.7.0，稳定ACP协议v1。SDK负责JSON-RPC请求关联、类型化请求/响应/通知、错误与连接调度，不再保留手写JSON-RPC客户端，也不回退print/exec/HTTP内部助手。

项目MSRV保持1.89；SDK声明MSRV1.88。实际本机编译使用rustc/cargo 1.98.0；依赖metadata未发现声明MSRV高于1.89的包，但**没有用1.89编译实测**。新增29个锁定包及许可证/MSRV信息见04依赖证据；不自动安装工具链。`tokio-util = "=0.7.19"`的codec用于边界，`futures-util`启用sink。

使用SDK Channel/TransportFrame接自有有界tokio行codec，而非SDK无总量上限的ByteStreams：单行256KiB、连接输入4MiB/4096帧（进入SDK队列前限制）、输出4MiB、聚合正文256KiB、stderr 2MiB。超限、坏帧、EOF、写入超时均退役连接，不吞成空候选；资源上限故障不伪装成正常上下文轮换。

不广告文件/terminal能力，所有原生权限请求仍返回cancelled。只有开启搜索时，已核实的OMP Fetch或Gemini Search通知才可按本轮ID/参数/状态接纳；未知工具、换身份、跨轮重放、未完成搜索上的候选均拒绝。工具前解释不拼进最终候选JSON。通知校验不是执行沙箱：原生注册表与拒绝默认策略才是执行前边界。

官方来源：[Rust SDK](https://github.com/agentclientprotocol/rust-sdk)、[crate 2.1.0](https://crates.io/crates/agent-client-protocol/2.1.0)、[协议](https://agentclientprotocol.com/protocol/overview)、[注册表](https://github.com/agentclientprotocol/registry)。具体版本调查是03的来源证据，04实证是锁定依赖编译和受控管道，不冒充真实宿主模型验证。

## 动态配置与上下文边界

非敏感assistant.json位于私有配置旁，format=3；保存长期偏好、两个动态开关、续用意图和按宿主/程序/来源分组的native_preferences，本场主题不持久化。旧配置的两个动态开关默认true；旧格式备份、损坏/未知字段不覆盖。模型与thinking按完整回执保存和依赖顺序恢复，失效不降档。自动发送只有本人明确保存过的同房间/发送UID授权且恢复检查通过时才续用，不从历史快照恢复权限。`.danmu/config/`仅白名单观察，锁内记录历史、原子更新current；相同内容去重，不含认证/端点token，审计失败不否认已成功保存的设置。

- `configOptions: []`表示明确无选项，不回退旧mode；仅字段缺失才允许Agent报告的旧mode。category只决定展示，不代表权限。
- `session/config_option_update`以及设置完整回执替换整份选项列表；不合并旧依赖。请求值须仍在最新选项中。Boolean使用布尔值而非字符串。
- 原生ID逐宿主映射：OMP model/thinking；Claude与OpenCode model/effort；Codex、Copilot、DSH model/reasoning_effort；Pi model/thought_level；Amp amp-mode。模型改变重建上下文，思考强度原地更新，避免恢复思考参数时重置模型。仅允许已核实选项；权限/mode/Boolean不会因被报告就获准修改。Gemini旧模型接口会改写全局配置，当前只继承原生默认。
- 设置请求单飞，当前轮结束才执行；回执前不称生效。回执当前值仅为Agent报告，不等于底层provider已证实；请求被拒绝或回执不匹配则暂停，不静默降档。
- 换模型先新建ACP会话再验证/设置；恢复时先模型后思考强度。宿主/程序/profile/system/搜索变更在本轮结束后重建；候选、草稿、同场主题保留。人设及其他偏好影响后续轮，已知词还在发送前检查。
- 默认先审后发；自动排队要求当前策略与本轮快照都允许自动发送、本场显式许可仍有效且本轮选项未变化，不把Agent报告冒充provider证明。

### 用户可编辑项目与能力边界

“工作区与记忆”显示默认`~/Projects/danmu-assistant/`及实际状态。AGENTS.md自动维护带标记规则块并保留自定义区，拒绝损坏标记、链接/硬链接与越界，不覆写用户资料；WORKSPACE.md为人类入口。SYSTEM、选定人设默认加载，PROJECT/公开简介/skills显式启用，技能默认空。选中文本总量8KiB、技能16项，不读`.danmu`作为指令、不扫描其他文件。

第二个Agent窗口先读AGENTS.md，将用户要求写入选中文件，聊天本身不生效。运行中1秒轮询，发现受调度影响；下一批新消息才用新ACP会话，旧在途结果丢弃，旧自动待发撤销/permit失效，保留待审和人工编辑并需重新审核；用户须重新允许自动发送。已POST仍按真实结果收束，不虚假取消，分段的未发尾段停止。读取失败阻止新prompt，不偷偷回退或补建资料，修复后自动核对。“本轮已载入”只证明ACP输入，不证明模型效果。

`.danmu/history/<房间>/history.sqlite`为持久索引；旧私有库按SQLite事务导入已提交WAL数据。平台ID仅是源标识，以原ID、秒级时间、作者UID、昵称和正文的完整tuple生成独立存储身份；空ID或复用ID的不同版本分别保留，完全相同记录幂等。旧id主键模式在IMMEDIATE事务中迁移，保留原始字段和归档导入进度，重建FTS；失败事务回滚。检索按内部rowid去重，返回/排除仍使用原始ID，不擅自判定冲突哪版正确。

全量历史索引与副本绑定后台加载，主界面、人工草稿与私有记录不等待；期间新弹幕加载完成后补齐索引。旧库或单份归档失败不丢健康索引，真正错误仍传播。`/diag`显示状态、来源和实际路径；点“重建历史”或 `/diag repair` 直接后台执行，进行中不重复启动。只修经日志校验的过期snapshot.json/summary.md，先备份、不改原件；正文冲突和索引损坏拒绝覆盖。其他源文件人工修复后重启加载，不靠重新打开助手重扫。

`.danmu/sessions/<场次>/`保存私有Journal/快照/摘要的维护副本，初次完整、之后有锁增量，修改/截断/替换不静默覆盖。canonical同源不自复制/自锁。原私有归档仍是恢复事实来源，副本失败单独警告；配置已提交后绑定失败不回报“设置未保存”。

程序维护区仅允许外部Agent按用户要求只读查询，不能恢复旧配置/运行/授权。config.toml、assistant.json、assistant-account.json、themes.json仍走私有实际路径；B站、OBS、模型凭据与Bridge token不迁入工作区。FTS5按本房间相关词取最多8条/4KiB，排除当前批与近期对话；邻近发言不是确认问答，不称未录音的口播为记忆。环境文件只代表保存时观察，模型许可未知。`.danmu`的0700权限不是抵御同用户恶意Agent的OS沙箱。

**允许**：答本批有效消息、使用选定人设、参考程序提供的限量历史；搜索只有独立开启的准入工具。**禁止**：观众指令触发电脑/shell/任意文件操作、插件执行、配置或人设改写、发送授权、批量管理弹幕。模型只交结构化候选；本批ID、不重复、最多8条，状态和实际发送由程序决定。历史/网页/昵称伪装系统或主播也只是数据，权限请求与未知工具拒绝。

本机用户编辑是操作员通道，直播助手没有通用文件写权限。多数接入依赖原生工具/协议限制；Pi另用Node权限，Amp另用macOS进程限制，均不等于完整文件系统/网络沙箱或模型永不误导的保证。外部CLI/MCP Agent的其他能力仍须在宿主侧单独禁用。

### 独立助手发送账号

“账号”同页分人工发送和AI发送，展示当前昵称/UID。AI默认复用主号；选中“扫码更换”并按Enter后，仅本次被接受的Ready token通过身份和私有保存校验才自动使用新账号，不再额外确认。同主UID拒绝，失败、取消、迟到事件不覆盖旧凭据；主/助手扫码互斥，也不隐式授权。

所有助手回复走选定身份，人工仍走主账号。更换/退出/复用会暂停并撤销旧批准与记住的授权；候选保留供重新审核。在途POST固定旧凭据并等待实际结果，不重复发送。选择文件保存模式/UUID及可选房间ID、发送UID授权范围，不保存Cookie；独立凭据0600存于原主账号私有目录AssistantAccounts/，不进入工作区或提示词。失效或配置损坏只停助手，不回退主号；只有本人明确记住且重启仍匹配的范围可恢复自动发送。

## 九宿主接入与认证边界

支持OMP、Gemini、Claude Code、Codex、OpenCode、Pi、Amp、GitHub Copilot CLI、DeepSeek Harness。Cursor经用户明确选择，本轮排除；不把它展示为已适配。现有Cursor配置不静默改成其他宿主，启动会明确报错。

“CLI已安装”“ACP入口已发现”“已连接”是不同状态。选择仅保存偏好，不启动、不安装、不登录。需要适配器的宿主不会把普通claude/codex/pi/amp当ACP程序执行。

| 宿主 | 已核入口/版本 | 原生安全与认证边界 |
| --- | --- | --- |
| OMP | `omp acp`；18.1.15 | 无工具或仅web_search；禁扩展、规则、skills、LSP、PTY等；原生认证与可选profile不变 |
| Gemini | `gemini --acp`；0.35.3 | 明确system-settings覆盖，不依赖错误的临时cwd；hooks/skills关闭，MCP互补允许/拒绝列表封闭所有名称，admin-policy默认拒绝。使用私有上下文文件名，避免空数组回退GEMINI.md。存在企业系统策略不覆盖 |
| Claude Code | `@agentclientprotocol/claude-agent-acp@0.75.1` | SAFE_MODE及会话`tools:[]`、无settingSources、strictMcpConfig、禁hooks。保留SDK原生API/console认证；当前上游不支持claude.ai订阅认证，不能承诺订阅通用 |
| Codex | `@agentclientprotocol/codex-acp@1.10.0` + 原生`codex-cli 0.153.4` | 官方ACP不变；私有原生app-server安全代理强制`environments:[]`、read-only/never，关闭skills/MCP/hooks等，防止上游read-only模式实际映射workspaceWrite。仅链接原auth.json；不承诺不同CODEX_HOME下的keychain-only认证 |
| OpenCode | 原生`opencode acp`；1.2.15、1.18.30+的1.x | 私有HOME/XDG和只读配置阻止安装，默认danmu agent全工具deny；仅链接个人api/oauth认证，不带账户数据库或组织引导。wellknown/系统管理配置拒绝。旧版依赖下载型认证插件的登录需升级 |
| Pi | `pi-acp@0.0.33` + 官方Node版Pi；新增实测Pi0.85.1 / Node24.4.1 | 私有启动器禁文件/shell、全局扩展/模板/自动上下文，Node禁止子进程；原auth引用，安全快照models/defaults；仅明确联网时加载内置免费搜索扩展，不执行命令型凭据 |
| Amp | `amp-acp@0.9.0` + 原生Amp；当前仅macOS | `tools.disable:["*"]`，不能用会恢复默认工具的空enable列表。私有HOME/XDG；macOS禁止fork、浏览器/插件子进程与AppleEvents，阻止远程global-skills物化。缺既存认证时直接提示本人登录 |
| GitHub Copilot CLI | 原生`@github/copilot@1.0.83 --acp --stdio` | 独立CLI，不是VS Code扩展。私有设置禁hooks/plugins/MCP；随机保留名称的原生白名单产生空工具集，不能用空参数。复用原生gh已存认证，不继承混存认证与执行设置的旧config.json |
| DeepSeek Harness | 官方`@deepseek-ai/dsh@0.1.5-alpha.1`；`dsh --profile shisui-text` | 私有DSH_HOME，bundles为空且明确最小官方插件组合，无文件/shell/MCP工具实现；credentials插件引用原`~/.dsh/.credentials.yaml`，不启动Web UI |

关键npm入口在执行前核对真实package.json、bin路径和已验证版本：Claude、Codex、Pi、Amp、DSH不接受普通CLI或任意shell包装器。它防止入口误选，不是发布者签名验证。Claude/Codex/Copilot握手版本也限定上述已核契约；Codex原生版本在代理中校验，旧版可能静默忽略无环境设置，不能放行。OMP/Gemini沿用协议和功能契约；新选项仍须单独审核权限语义。

适配器与认证需显式准备，不自动全局安装或改变账号。Pi可运行 `danmu setup pi`：通过npm显式安装已核定的pi-acp@0.0.33到应用私有 `adapters/pi/`，禁安装脚本；完整匹配版本直接复用，旧版/损坏/越界链接不覆盖。发现优先PATH，再查私有入口；仍须安装官方Pi与兼容Node，原生CLI和适配器缺失分开提示。选择或启动助手不会下载依赖，不改Pi原配置或登录态。

Pi读取原生 `~/.pi/agent/`（或显式绝对路径PI_CODING_AGENT_DIR）的声明式provider/model定义、modelOverrides及defaultProvider/defaultModel/defaultThinkingLevel；支持原生行注释、尾逗号与BOM。TUI已保存的该宿主模型/档位优先。私有快照不带全局packages/extensions/skills/模板/上下文；apiKey和headers中的`!command`、动态OAuth配置及任意请求体覆盖拒绝，不转发父进程API-key环境变量。原生auth存储仍由Pi负责；依赖扩展注册的provider不在此文本接入承诺内。

对Pi显式默认或TUI已保存模型，启动前在Rust内存固定预期；建会话时要求原生当前模型属于实际可用列表且匹配预期。不能以Pi可回写的settings文件作为校验基准，也不放行Pi按未知ID合成的模型。无效默认的真实回归在主动注入事件后保持0次模型请求。

Pi原生选项来自真实ACP回执。当前适配器提供off/minimal/low/medium/high/xhigh六档，不按模型裁剪列表；非推理模型原生回落off。请求与实际档位不一致时不保存该请求；原生默认max明确拒绝，不假报medium。其他宿主仍仅显示其回报选项；未保存时使用安全接入下的原生默认，不承诺继承隔离的模型/插件配置。旧vs_code迁移为独立copilot时清空编辑器路径与旧原生参数，首次保存备份assistant.pre-hosts.json。

原生证据：OMP真实initialize/new成功；Gemini真实initialize及认证缺失错误，实际Config初始化验证无工具/仅google_web_search、hooks/skills关闭、MCP全拒绝及不读取GEMINI.md。DSH/OpenCode真实initialize/new通过。Pi/Claude/Codex/Copilot使用虚构凭据和loopback模型服务，验证真实原生文本、空工具集、强制读写拒绝、取消与EOF清退；没有调用付费模型或公开发送。Pi额外验证声明式provider、原生默认high、切换非推理模型后的off、失败档位不保存及空候选；强制工具可能先收到原生“Tool … not found”并产生一次错误续轮，产品退役该轮、无候选与工具副作用，不宣称原生不会续轮。Amp事件后不再运行其原生登录/工具枚举，仅执行真实二进制纯策略函数和无害OS拒绝探针，未声称真实模型往返已验。

来源：[Claude ACP](https://github.com/agentclientprotocol/claude-agent-acp)、[Codex ACP](https://github.com/agentclientprotocol/codex-acp)、[OpenCode](https://opencode.ai/docs/acp/)、[Pi ACP](https://github.com/svkozak/pi-acp)、[Amp ACP](https://github.com/tao12345666333/amp-acp)、[Amp插件](https://ampcode.com/docs/customize/plugins)、[Copilot CLI](https://github.com/github/copilot-cli)、[官方DSH](https://github.com/deepseek-ai/deepseek-harness)。

## 长连接、取消与恢复

一个助手驱动、一个专用ACP连接；initialize协商v1，session/new给出明确ID，单飞复用。每轮最多8条新消息/24KiB、180秒；每个上下文最多8轮、累计应用输入最多64KiB，下一轮预留完整24KiB，不足时安全轮换。问答与一次性改写均计入，不把应用输入字节等同模型总token。驱动所有权、本场队列、候选、在途发送与仍有效授权保留，已处理弹幕不重放；暂停或撤权不会被轮换覆盖。完整end_turn无正文与合法零候选分别诊断；有正文时严格校验本轮/本批ID，不认模型自述送达。未知或重复本批目标整批拒绝后继续新消息；真实协议、连接与资源错误仍停止。

取消使用`session/cancel`，等待原prompt终态，最长6秒宽限；未收敛清退本次独占进程组。取消请求已发不等于模型已停止；UI先显示处理中。取消/失败轮退役连接且须确认新上下文，结果代次失效。普通空闲暂停可继续同会话。只在前后Agent共同声明能力时精确resume或load；load回放不入候选，resume不接收回放，不用latest或当前开发会话。

进程只继承HOME/PATH/语言/代理等基础环境，不转发开发API-key/会话变量。需要隔离时仅链接或引用原生认证存储，不复制凭据进配置项目；宿主负责认证刷新。项目与房间索引持久保存，私有运行目录退出清理；OpenCode只读目录先恢复自有目录权限，不删除用户宿主历史。进程树清退要求POSIX；Amp额外要求macOS，Windows不宣称通过。

## 业务通道与独立CLI/MCP

SDK发起本轮后才对选中弹幕或已启用互动标Processing黄；自己/本地回流、未选中与禁用事件不标。无答灰、生成失败红，候选/在途/实际结果优先；暂停取消收束，不跨场次污染。只有真实发送确认才能绿。

候选与修复均进入原Bridge和共享发送队列。Confirmed才算确认，网络不确定不重试；有依据的可修复失败才新建一次候选，原终态不改写，附diagnosis、retry_of、分段记录与重复风险。外部caller不能获得Runner模型修复权，真人仍独立。

Runner持有时`status.driver=runner`；只排斥同一内置助手caller `danmu-assistant`的外部report/reply，避免双驱动。不同独立caller仍按既有权限合法回复，不做全局封禁；不得换caller绕过同一助手互斥。只读与结果查询保留。用户切换同一助手前先停旧外部值班，danmu不抢停其他进程。

独立接入读取[danmu-duty Skill](../agent-package/skills/danmu-duty/SKILL.md)。`python3 script/agent_config.py --host HOST --binary /实际固定/danmu --instance /明确私有实例`仅打印原生配置。Amp保留amp.mcpServers，Pi保留CLI例子；不假造MCP，不改共享角色，不恢复旧OMP定时唤醒扩展。

## 本地验证与真人待验

获本人授权后：`/实际固定/danmu --instance /全新绝对私有目录 local --assistant`仅打开面板。本人输入`/event 姓名 正文`、`/local confirmed|uncertain|rejected`、`/session new|end`，`/quit`或SIGINT正常退出；禁止房间、登录、OBS、生产配置混用。

以下04/FIX01记录为历史证据，不代表当前二进制。当前额外通过受控ACP搜索生命周期、11场景有限修复、真实隔离PTY的快捷审核/开关授权/原生参数回执；仍未调用真实模型、真实搜索或B站发送。开发者自动化不是用户本人直播验收。

## FIX01：G5现有入口事实与独立验证边界

**不存在现成的固定产品非键鼠ACP完整控制接缝，G5及相关独立L继续阻塞。**

- `src/config.rs::Command`仅Local/Agent/Mcp；`src/main.rs::run`将agent操作解析成Bridge Request。Request与MCP工具仅status/messages/report/reply/result，没有runner start、事件注入、批准或transport选择。
- `src/terminal/agent.rs::run_local`要求新的空私有目录；`--assistant`只打开详情。`/event`、`/local`、`/session`及助手菜单中的开始/批准属于TUI输入处理，不是外部CLI/MCP操作。
- `src/runner/tests.rs`、`src/terminal/agent_tests.rs`通过cfg(test)直接使用Runner/TerminalApp和受控peer；只证明开发者执行的回归，不编入fixed danmu，不要求QA编译，不新增隐藏控制口、预置配置绕过或键盘注入。

现有合法零输入路径只能启动空local、观察默认/详情及查询只读状态。须全新0700私有root、隔离HOME/TMP、准确fixed绝对路径及独立可用TTY（探针PTY不得写任何字节，包括DSR回应）。创建ROOT及其home/tmp后启动：

`env -i HOME="$ROOT/home" TMPDIR="$ROOT/tmp" PATH=/usr/bin:/bin LANG=en_US.UTF-8 TERM=xterm-256color "$BIN" --instance "$ROOT/instance" local`

可另选`local --assistant`只打开详情；同一明确实例查询`"$BIN" --instance "$ROOT/instance" agent status '{}'`，预期local_transport=true、sending_enabled=false、cursor=0；`mcp`可列五工具及查询，无生成控制能力。只对本次PID发SIGINT退出，确认退出码、端点删除、端口关闭和锁可重取。

本轮F01/F02由Dev生产SDK受控回归及Ratatui buffer证明；buffer状态切换不是真人实际表面U。独立QA可达的空实例L不扩展为SDK动态链路L；C16/R-C16、九原生宿主/模型、Rust1.89实编继续未验。真实非生产键鼠由Lead另行组织。

