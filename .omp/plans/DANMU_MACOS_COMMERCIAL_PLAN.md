# DANMU 私有商业 Mac App 执行计划

## Context

将当前 Rust TUI 的全部用户可见能力迁入原生 Mac App，以“半透明弹幕面板 + 完整工作台”提供左右贴边收纳、悬停展开、红色未读角标和 macOS 系统通知，TUI 继续保留。用户明确选择：GUI、交互与商业授权代码保持私有；复用的 MPL 核心继续开源；升级已有私有工程“拾穗弹幕台”。用户明确选择以当前 TUI 为完整功能基线，退役旧 GUI 独有的提问分类、优先级及待回答/回答中/已回答/跳过操作，旧原始档案保留可读。执行交付为完整本地 App、隔离验收与可复现打包入口，不授权推送、公开发布、生产支付/授权操作、真实模型消费、公开弹幕发送或 OBS 直播操作。

## Approach

### 1. 固定仓库、商业许可和构建边界

定义三个根目录，后文 R/G/K 均指以下实际路径：

- **R，公开 Rust**：`/Users/elazer/Desktop/Workspace/90_Dev/danmu`。已读基线 HEAD `d8cf9f935d127d64cd22111ad9b406e2fb68779d`，仅供识别，不回退/切分支/覆盖用户改动。现为 Rust 2024、MSRV 1.89、包 `shisui-danmu`、bin `danmu`、版本 0.5.1。
- **G，私有 App**：`/Users/elazer/Desktop/Workspace/90_Dev/拾穗弹幕台`。现为 SwiftPM、Swift 6、macOS 14、executable `ShisuiDanmuDesk`；保留正式 bundle ID `cc.ss-data.ShisuiDanmuDesk`、App 名称与 `Vendor/ElazerLicenseKit`。不新建产品仓库。
- **K，旧公开 Swift kit**：`/Users/elazer/Desktop/Workspace/90_Dev/shisui-danmu-kit`。G 现精确依赖 0.3.2。仅作为旧格式/API 的只读参考，本次不修改 K，也不在 `.build/checkouts` 改源码。

R 保留 MPL-2.0。新增无界面运行层、FFI、生成绑定、通用存储适配和它们的修改全部放 R；窗口、布局、通知策略、商业授权门控全部放 G。依据已读 `R/LICENSE` §3.2–3.4，对分发的 MPL 部分提供与二进制对应的源码及许可证，不要求公开私有 GUI；把 MPL 文件复制到私有目录或改成静态链接不能消除义务。G 的商业使用条款不能限制收件人对 MPL 部分的权利；不改公开 TUI 的免费使用方式。首次正式销售前核对商业条款，但工程不能把“收费”与“全部源码闭源”混为一谈。

保留 G 现有 `ShisuiDanmuLicenseStore` 的 productID、服务配置、试用/离线授权与定价逻辑，不把 README 的历史价格重新当作本次定价决策；不新建支付、账号或云端服务。旧 `website` 已有其他产品内容，禁止作为本次 App 的发布源、appcast 或下载站使用。

先修改 `G/script/build_and_run.sh`：删除无条件 `pkill -x ShisuiDanmuDesk`，新增纯构建模式 `--build-only`，复用其组装 bundle/资源逻辑，不再移除 Sparkle 自带 XPC services。新建 `G/script/build_app.sh` 作为唯一打包实现，原脚本转调用该实现并保留已有运行参数；纯构建绝不启动/关闭 App。所有输出限定在项目 `.build`/`dist` 的自有路径，不覆盖 `/Applications`。

构建契约：

- `./script/build_app.sh --configuration debug --variant qa --arch arm64` → `dist/qa/拾穗弹幕台 QA.app`，bundle `cc.ss-data.ShisuiDanmuDesk.qa`。
- `./script/build_app.sh --configuration release --variant production --arch universal` → `dist/release/拾穗弹幕台.app`，正式 bundle ID；本地无 Developer ID 时只生成明确标注的未公证候选，不声称可正式分发。
- `qa` 必须是 debug；release + qa/test-support 组合构建失败。缺核心产物/资源/架构/版本匹配信息时失败，不临时退回旧 Swift 核心。
- QA 的数据根、UserDefaults suite、LicenseStore 根和 Keychain service 全部独立；开发模式不调用生产授权服务和 Sparkle 更新源。

### 2. 先完成原生窗口状态机，独立于业务迁移

修改 G 现有 `DanmuMonitorPanelController.swift`、`DanmuMonitorWindowBehavior.swift`、`DanmuMonitorInteractionRouter.swift`、`DanmuMonitorWindow.swift`、`DanmuDesignSystem.swift`；复用现有 nonactivating NSPanel、window identity、frame 保存和原生材质，不另造第二套窗口管理器。新增 `EdgeDockState.swift` 和 `EdgeTabPanel.swift`，因为当前没有贴边收纳/悬停状态机或独立边缘把手。

本步骤可先以现有事件视图/合成事件验证；最终必须接第 5 步真实 Rust façade，不把纯回放窗口作为最终产品。

| Before | After | What this changes |
| --- | --- | --- |
| 旧面板默认 440×720pt | 默认 380×600pt，最小 320×360pt；主工作台默认 1040×720pt、最小 900×600pt | 日常窗口紧凑，需要完整操作时仍有足够空间 |
| 旧独立窗与主窗偏展示/提问管理 | 贴边面板用于实时阅读/人工发送/待审摘要；完整工作台包含直播、助手审核、历史与重点 | 不把全部配置塞进窄面板 |
| 旧材质/皮肤散落在视图 | regularMaterial 作底、16pt 连续圆角、0.5pt 语义边框、轻阴影；正文区加 controlBackgroundColor 语义遮罩，默认14pt、可调12–24pt | 模糊背景但不把文字一起变淡，代码/视频背后仍可读 |
| 没有贴边行为 | 距左右可用边缘16pt内松开拖动后吸附；悬停200ms展开，离开800ms收起，动画180ms | 减少误触，窗口不会无故吸到边缘 |
| 没有角标 | 收起后显示28×88pt边缘小窗，内含12pt握柄、最多99+红色角标；红色同时配数字/可访问标签 | 把手有可点击空间，不依赖颜色或1px热区 |

状态与规则固定为：

- `enum EdgeDockState { case floating; case expanded(DockAnchor); case collapsed(DockAnchor); case hidden }`；`DockAnchor` 保存显示器 ID、`edge: left/right`、归一化纵向中心 `0...1`。`isPinned` 是独立偏好，开启时强制 expanded，禁止 pinned+collapsed。
- 使用 `NSScreen.visibleFrame`；只对未与相邻屏幕重叠的外侧左右边缘自动吸附，顶部/底部不吸附。拖离吸附边缘超过24pt即回 floating。屏幕布局变化重新 clamp；原屏幕消失则移至主屏幕同侧，保持用户可见。无屏幕时保留状态，下一次屏幕通知恢复，不忙等。
- 收起时主面板 `orderOut`，只显示 `EdgeTabPanel`，不把大窗口移到屏幕外，避免“藏在另一块显示器上”。两窗共享一个 anchor 与偏好，任一可见时另一窗不能截获输入。
- 两个NSPanel均设置 `hidesOnDeactivate=false`；交互面板仅在需要键盘输入时成为key。把手用 `.activeAlways` tracking area 接收悬停，不申请 Accessibility/Input Monitoring 权限。悬停只 `orderFront` 展示，不调用 `NSApp.activate`、不 makeKey、不自动聚焦发送框。点击面板交互控件才激活；自由拖动/悬停不抢键盘。
- 鼠标在面板、菜单/Popover 打开、文本选择、IME marked text、发送框焦点、候选编辑/审核、登录或确认 sheet 可见期间不自动收起。焦点离开发送框但有草稿时可收起，草稿保留；图钉始终可以固定。
- `Esc` 先取消当前编辑/菜单/确认，再退出历史选择；只有这些均不存在且无未提交编辑时才收起未固定的贴边窗；不退出 App、不触发发送。
- 保存 expanded/free frame，绝不把28pt把手尺寸写成下次面板尺寸。`hidden` 只由用户关闭窗口/显式隐藏进入；收到消息不自动展开。Dock reopen 与菜单栏“显示弹幕面板”必须能恢复。
- 对“所有桌面”“全屏上方显示”提供独立显式选项；新用户默认 false，保留旧用户已保存的 `danmuMonitor.showsOnAllSpaces`。`.floating` 只在用户启用置顶时使用，不使用屏保/系统警告级别。全屏显示采用 auxiliary behavior，并实际验收，不承诺覆盖安全系统界面。
- 尊重 Reduce Transparency/Increase Contrast/Reduce Motion；降低透明度时改为不透明语义底色，降低动态时无滑入动画。图钉、关闭、工作台、菜单均有文字标签/tooltip/AX 名称；主工作台保留标准红黄绿。
- 全局唤出使用 `RegisterEventHotKey`/`UnregisterEventHotKey` 注册，生命周期由面板控制器持有，不用全局键盘监听；默认不占键，由用户设置。注册失败显示冲突并保留旧值；菜单入口永远可用。App内 `⌘K` 命令搜索，`⌘,` 设置，`⌘1/2/3` 切直播/审核/历史，`⌘W` 关闭当前窗口，`⌘Q` 真正退出。

### 3. 抽离无界面 Rust 运行层，并让 TUI 先接回同一实现

不整体移动已有 domain/bilibili/runner 等目录到另一份 crate，避免无收益的大规模改名。保留 R 当前 library 为共享核心：

1. `R/Cargo.toml` 新增 `tui` 默认 feature；`clap`、`crossterm`、`ratatui`、`qrcode`、`rpassword`、`tui-input`、`unicode-width` 只在 `tui` 启用。`[[bin]] danmu` 增加 `required-features = ["tui"]`。`src/lib.rs` 中 `terminal/config/theme` 按该 feature 编译。`--no-default-features --lib` 必须不依赖终端栈；其他公共模块路径不改，不加兼容 re-export。
2. 新增 `src/runtime/{mod,commands,events,accounts,send,assistant,settings}.rs`，把现有业务方法移动进去，而非复制实现。现有库没有统一无界面 host，因此此处必须新增。
3. 从 `TerminalApp` 迁出 `client/account/assistant_accounts`、两个 SendQueue、Bridge、Runner、journal/session、房间/计数/连接、OBS、电平、送达匹配器、身份/设置/OAuth operation generation、待续用验证及自有任务。TUI 只保留输入/草稿光标、菜单/搜索、候选可见性与滚动、历史锚点、色彩和终端生命周期。
4. 迁移 `handle_client_event`、`handle_ui_event` 的业务分支、`ingest_event` 的去重/历史/送达/归档部分、`advance_bridge`、`enqueue`、`set_automatic_permission`、`restore_automatic`、`pause_assistant`、账号世代、设置提交和 OAuth begin/finish。窗口操作/渲染不进入核心。`UiEvent::LoginQr { lines }` 分拆成核心二维码 URL/状态和 TUI 的二维码字符绘制；OAuth 打开浏览器改成操作返回的专用 effect，由宿主打开，不把授权 URL 放日志/常规快照。
5. `TerminalTransport` 移到 runtime/send 并改名 `RuntimeTransport`。matcher、oneshot 和 POST 都由核心处理：先注册 matcher并确认，再 POST；发送前再查身份、队列 generation、job permit。Swift/TUI 观察慢、无窗口或订阅丢失均不能影响 matcher 注册或送达确认。
6. 移动业务测试到 runtime 对应模块；涉及键盘、草稿、菜单、真实 PTY 的测试留在 terminal。`run/local/replay` 改为使用同一 runtime；CLI 参数、默认配置、主题、现有外部工具协议与输出不变。LocalTransport 的合成结果仍只能用于 local，不冒充平台发送。

新的公共 Rust 入口约定（`Result` 使用当前 anyhow，FFI 再转结构化错误）：

```rust
pub async fn open(options: RuntimeOptions) -> anyhow::Result<RuntimeHandle>;
impl RuntimeHandle {
    pub async fn execute(&self, command: RuntimeCommand) -> anyhow::Result<CommandResult>;
    pub async fn query(&self, query: RuntimeQuery) -> anyhow::Result<QueryResult>;
    pub async fn observe(&self) -> anyhow::Result<RuntimeSubscription>;
    pub async fn shutdown(&self) -> anyhow::Result<()>;
}
impl RuntimeSubscription {
    pub async fn next_batch(&self) -> anyhow::Result<Option<RuntimeBatch>>;
    pub fn close(&self);
}
```

- `RuntimeOptions` 接收显式 `StoragePaths`、`workspace_default: PathBuf`、`mode: Live|Local|Replay`、可选外部实例目录和 `Arc<dyn ObsPasswordStore>`；open 只准备本地状态，不自动登录、连接房间、调用模型或授权。TUI 的 main 显式提交 Connect；GUI 等待用户连接/许可。Replay 不访问任何网络和默认用户路径。
- 一个 Tokio task 串行持有业务状态，长网络操作以受管子任务完成，再按冻结的 session/generation 收束；不持锁跨 await，不在每次 UI 回调创建一个 runtime，不使用 `block_on` 阻塞 Swift main actor。
- command/query 返回错误必须保留稳定原因，不能用空列表/成功默认值掩盖存储、鉴权、协议错误。每个操作在最终结果前有 `operation_id`；UI 的“已受理”不是“完成/送达”。取消旧操作不接纳迟到结果。
- 初次 observe 原子返回带快照的首批，随后仅增量：事件 upsert/remove、状态字段变化、候选/送达变化、notice。runtime 给每个首次进入显示集合的逻辑行分配 `message_key: u64`，在来源归并/礼物更新/重同步中保持不变；不把可变 origin/content 当 SwiftUI identity。原始 event.id 仍用于业务与外部协议，显示键仅在当前 runtime 内有效。批次包含递增 sequence 和当前场次新消息累计值；最多64项合批、最长33ms合并 UI 通知，观察环缓冲上限2048项变化，观察者落后时发 `ResyncRequired`，重新原子订阅一致快照与后续变化，不阻塞采集/发送。重同步不重播 toast；累计值用于恢复角标差值，不丢失已收新消息的计数。
- 核心按摄入上下文区分启动历史、归档恢复、实时/运行期历史补偿；仅实时阶段首次接纳的逻辑 Danmu/SC 增加新消息累计值，礼物累计更新和重复补偿不增加。TUI 自己原有历史阅读计数可继续按其展示规则计算，不把 GUI 已读状态写进公共会话业务。
- shutdown 可重复等待同一结果；先关闭新操作/撤销未来发送、停止分流和自有 OAuth/Runner，再等待在途 POST 按现有有界超时收束，取消未发尾段、结束 Bridge/连接任务/外部端点，最后写会话结束。任何错误按原真实结果返回；不把已确认段改成失败；不触碰 OBS 推流或外部 Agent。

`RuntimeCommand` 使用类型化枚举，按以下完整动作集建立子枚举，不以任意 shell/命令字符串作为 GUI API：

- `Connect { room_id }`、`Disconnect`、`CancelOperation { operation_id }`。
- `SendManual { text, reply_to_message_id, draft_revision }`；分段由 `bilibili::segment_message` 执行，40字素/段、确认后1200ms继续，不接受 Swift 自算段落。
- `Account(AccountCommand)`：`BeginLogin { identity: Main|Assistant }`、`CancelLogin`、`SignOut { identity }`、`UseMainForAssistant`、`UseIndependentAssistant`，保留两账号 UID 校验与 generation。
- `Room(RoomCommand)`：`SetTitle { text }`、`SetCover { path }`；复用房主检查与平台真实回执，不把“受理/待审”写成已展示。
- `Assistant(AssistantCommand)`：`Start`、`Pause`、`Stop`、`ProcessVisible { message_ids }`、`SetAutomatic { enabled }`、`CancelPendingResume`、`SetRange { activity }`、`SetTopic { text }`、`ApplyPreset { preset: Quiet|TextQa|Thanks }`、`SaveSettings { settings }`、`SetApiKey { value }`、`ClearApiKey`、`BeginChatgptLogin { account_id }`、`SignOutChatgpt { account_id }`、`SelectChatgptAccount { account_id }`、`RefreshModels`、`SelectModel { model_id }`、`RepairHistory`。
- `Review(ReviewCommand)`：`Edit { expected, text }`、`Approve { expected: Vec<ReviewedCandidate> }`、`Discard { expected }`。`ReviewedCandidate` 包含 session/caller/request_id/revision 与显示时的发送身份 generation；复用 Bridge 的原子冻结校验。打开面板不是批准，批量只处理所见批次，编辑保存不是发送。
- `Obs(ObsCommand)`：`Connect`、`Refresh`、`SaveConfiguration`、`SetPassword`、`SetMicrophone`、`SetMuted { muted }`、`SetScene { name }`、`StartStream`、`RequestStop`、`ConfirmStop { token }`、`CancelStop { token }`。停播状态/3秒倒计时归核心，UI 仅展示；UI关闭或取消在请求发出前必须取消倒计时。
- `FeatureMessage { message_id: Option<String> }` 使用现有 session.feature；`SetExternalEndpoint { root: Option<PathBuf> }` 默认 None；`ExportSession { session_id, format: Markdown|Json, destination }` 作为写操作放在 command，目标存在时必须已有原生保存面板的覆盖确认，失败保留原文件。
- `RuntimeQuery`：`Snapshot`、`Diagnostics`、`Settings`、`Candidates`、`SubscriptionAccounts`、`Models`、`ObsScenes`、`ObsInputs`、`SearchArchive { query, room_id, from, to }`、`LoadSession { session_id }`。复用原检索/导出，不增加跨房间观众记忆。

Settings 记录逐字段映射 `runner/settings.rs` 的 format4/ApiConfig/Settings，保留原 schema/迁移规则；配置路径/Key/Token不进入观察快照，配置编辑通过专用 Settings query。文件级 workspace/skills/preferences/输出上限仍保持明确保存后重启或原有热更新契约，不擅自新增即时切换行为。`SaveSettings` 不可借 automatic/resume_on_start 字段开启权限：授权只能通过 SetAutomatic 并走身份检查。

关键安全行为不重设计：双队列隔离、独立账号失效不回退主号、HTTP受理不等于送达、unknown不重发、内容候选整批拒绝但继续新消息、真实传输/凭据/额度故障暂停撤权、原始磁盘错误保持可见、有限修复不处理未知送达、自动续用必须同房间/同UID且核验通过。不得重新引入 ACP/Pi/coding CLI 作为内置助手依赖。

调用迁移范围已从源码确认：R 的 `src/main.rs`、`src/terminal/{mod,agent,assistant,accounts,send,replay,settings}.rs` 与相应 tests。执行时优先 LSP references；本轮 rust-analyzer 对两处已知 TerminalApp 使用返回空，已上报，若仍失效，用专用 grep 搜索 `TerminalApp|TerminalTransport|UiEvent|ObsController::new|Runner::load` 于 `R/src` 得到完整文本调用集后逐项迁移。完成后不保留旧 terminal 业务实现或兼容别名。

### 4. 建立公开 UniFFI 边界与确定性的 Swift 包产物

R 增加 workspace member `crates/danmu-ffi`，根包保留 default member；库名 `danmu_ffi`，类型 `staticlib` + `cdylib`，依赖根 `shisui-danmu` 时 `default-features=false`。本次核实 UniFFI 0.32.2，固定 `=0.32.2`、`default-features=false`、`features=["tokio"]`；生成工具同版本，单独 `bindgen` feature 启用 `uniffi/cli`，不得把生成器/Clap 带进 App 链接。参考已读取的 https://docs.rs/crate/uniffi/0.32.2/source/Cargo.toml 与 https://docs.rs/crate/uniffi/0.32.2/source/uniffi-bindgen.rs 。

- 新增 `crates/danmu-ffi/src/{lib,types}.rs` 和 `src/bin/danmu-bindgen.rs`；生成器入口调用 `uniffi::uniffi_bindgen_main()`。
- UniFFI 对象 `DesktopRuntime` 包装 RuntimeHandle，导出 async `open(options: DesktopOptions, obs_passwords: Arc<dyn DesktopObsPasswordStore>) -> Result<Arc<DesktopRuntime>, CoreError>`、`execute(command: DesktopCommand) -> Result<DesktopCommandResult, CoreError>`、`query(query: DesktopQuery) -> Result<DesktopQueryResult, CoreError>`、`observe() -> Result<Arc<DesktopSubscription>, CoreError>`、`shutdown() -> Result<(), CoreError>`。DesktopCommand/Query逐项映射第3步枚举，不引入另一套命令语义。DesktopOptions明确包含 `support_root: String`、`config_path: String`、`account_path: String`、`obs_config_path: String`、`sessions_root: String`、`workspace_default: String`、`external_instance: Option<String>`、`mode: Live|Local|Replay`；所有路径须绝对且通过已有安全路径检查。`DesktopSubscription.next_batch() -> Result<Option<DesktopBatch>, CoreError>`、`close()` 保持一个 Swift consumer。DesktopObsPasswordStore为 `Send + Sync` foreign trait（不是已软弃用、要求Box的callback_interface），签名 `read(host: String, port: u16) -> Result<Option<String>, CoreError>`、`write(host: String, port: u16, password: String) -> Result<(), CoreError>`；包装到Rust ObsPasswordStore，磁盘/Keychain访问不在Swift main actor执行，Swift存储对象不持有runtime，避免跨FFI循环引用。实现 `From<uniffi::UnexpectedUniFFICallbackError>`，映射为无秘密内容的Storage错误而非panic。依据 https://mozilla.github.io/uniffi-rs/latest/foreign_traits.html ，使用锁定0.32.2支持的导出注解。不跨 FFI 暴露 Tokio channel、数据库句柄、AccountClient、Permit、原始 HTTP headers 或 anyhow 链。
- DTO `EventRecord` 逐字段映射 `domain::DanmuEvent`（含 author_id、origin、reply_to、emotes、gift）；时间转 Unix 毫秒、有符号64位；可空字段仍为空，不用0/空用户名冒充未知。候选逐字段映射 Bridge Reply 的公开序列化字段，但不暴露内部 owner/authorization/original_message；身份冻结只传 opaque generation。Room、OBS、Delivery、Settings 使用与原类型一一对应的 record/enum，禁止 Swift 第二次解释平台原始 JSON。
- FFI 错误采用可导出的 `CoreError` enum，变体固定 `InvalidInput|Busy|Stale|PermissionDenied|Storage|Transport|Protocol|Closed`，各自只有 `message: String`，用 UniFFI Error/thiserror 导出，不把 record 当可抛出错误。message来自既有安全摘要，不包含请求原文/凭据。跨 FFI 的 panic 不能伪装成功；事件流意外终止时 GUI 显示核心停止并禁用发送，不自动重启/重发。
- 新增 `R/script/build_swift_core.sh --configuration debug|release --arch arm64|universal --output PATH [--test-support]`：构建指定架构，使用本机 cdylib 生成绑定，静态库封装 XCFramework；universal 合并 aarch64-apple-darwin/x86_64-apple-darwin 两个 slice 后建单个 macOS XCFramework。`--test-support` 只允许debug并显式转发Rust feature；App只链接静态核心，不附带 `danmu` 或 background daemon。
- `uniffi.toml` 固定 Swift module `DanmuCoreFFI`，C shim module `DanmuCoreFFIShim`；生成包含 `Package.swift`、`Sources/DanmuCoreFFI` 与 `DanmuCoreFFIShim.xcframework`。G用本地生成包 `Vendor/DanmuCoreFFI` 依赖；通过 `G/script/sync_core.sh --source ../danmu --configuration debug|release --arch arm64|universal [--test-support]` 产生，不把另一个仓库的绝对路径写入最终 App。QA build/run自动以debug+test-support重新同步并核对产物；production不允许带test-support。
- 生成包同时记录 `core-build.json`：`source_sha256`、`source_revision`、`uniffi_version`、`configuration`、`architectures`、`test_support`。source_sha256对实际编译输入的排序路径/内容清单计算，排除该manifest与生成输出，不能只哈希HEAD；dirty修改必须包含。包缺失/manifest不匹配/架构不全立即失败，拒绝复用旧产物。
- 随每个可对外分发候选提供 `danmu-core-source.tar.gz`、独立的归档SHA-256及MPL/第三方通知。源码包按R的明确源文件清单导出当前工作树中的Cargo manifests/lock、src、crates、所需assets/agent-package与构建脚本；使用 `cargo vendor --locked` 将锁定依赖及许可证纳入该源快照，覆盖静态链接的UniFFI等MPL依赖，不只提供自己的修改。排除 `.git`、target、.local-archive、私有配置、测试输出与所有G源码。先冻结编译输入快照及vendor配置再构建，构建后把生成绑定附入源码归档，归档整体hash与编译输入hash分开记录以免自引用。关于页“开源组件”可定位随包源码归档；未上传公共仓库不等于无义务，也不假称已发布对应源码。

### 5. 私有 façade、旧数据与商业权限一次切换

G 保留 `DanmuDeskStore` 名称，但改成 `@MainActor` 的 UI 投影：持有一个 DesktopRuntime、一条观察任务、当前房间/导航、draft及revision、滚动锚点、审核可见版本、未读与面板状态。网络、去重、发送回声、AI和归档统一在 Rust。`OBSControlStore` 保留为同一 runtime 的薄 UI 投影，不再启动 obs-cli，不再独立轮询/判断发送或停播真相。

- 修改 `ShisuiDanmuDeskApp`，只创建一个 runtime owner，注入面板/主窗/设置/历史/菜单栏；订阅在应用级维护，view appear/disappear不能重复连接。通过已注册的窗口引用恢复窗口，移除0.5秒全局窗口猜测/强制激活路径。
- 复用 `DanmuDraftSubmission` 的 message+revision 判定；发送完成只可清空仍属于本次提交的草稿。失败/未确认/取消保留草稿；迟到结果不能清掉新输入，即使新输入正文恰好相同。
- 替换 `ContentView`、`DanmuMonitorWindow`、`SessionArchiveView`、`OBSControlViews`、`DanmuRichText`、`DanmuMonitorPreferences` 的旧 SDK 类型/调用。新增私有 `BilibiliAccountView.swift`、`AssistantReviewView.swift`、`AssistantSettingsView.swift`、`DiagnosticsView.swift`；当前不存在对应 Rust-native UI，因此新增。二维码用系统 CoreImage 从 core 返回URL生成，秘密输入用 SecureField，避免复制旧公开 SDK 的 SwiftUI 登录视图进入私有文件。
- 完成所有入口后，删除 G 的 `DanmuDeskRuntime.swift`、`DanmuIngressReconciler.swift` 及它们已被核心取代的逻辑；删除旧提问分类/状态/优先级/下一题入口与设置，但不删除原始 journal。`Package.swift/Package.resolved` 移除 shisui-danmu-kit 及 DanmuCore/BilibiliDanmu/OBSControl 产品依赖；不保留运行时双后端或 silent fallback。
- 现有 Swift tests 中保留草稿/账号/商业撤权/窗口/富文本行为断言，迁到真实 core local driver；删除仅绑定旧实现字段/转发次数的测试。原 `DanmuIngressReconcilerTests` 的用户可见去重边界迁至 Rust；不复制两份算法来让旧测试继续通过。

数据布局选择：

- 正式根保持 `~/Library/Application Support/cc.ss-data.ShisuiDanmuDesk`；保留既有 `Sessions/` 原件和 `BilibiliAccount/session.json`。Rust Credential 的 cookieHeader/csrf 与旧 Swift JSON 同名，identity 可缺省，复用后先真实状态核验，未核验不得恢复发送许可。损坏/权限不安全时报告需重新登录，不覆盖或“清空修复”。
- 新运行配置放根下 `Runtime/config.toml`、`Runtime/assistant.json`、`Runtime/assistant-account.json`、`Runtime/assistant-secrets/`、`Runtime/obs-control.json`；GUI 默认资料工作区设为根下 `AssistantWorkspace/`，不得继承 TUI 默认 `~/Projects/danmu-assistant`。`Runner::load`/workspace默认路径改为宿主注入，TUI继续旧默认。文件格式/严格迁移、0700/0600与符号链接防护沿用现有实现；路径不可用即明确失败，不落到全局临时目录或内存假成功。
- 首次 GUI 初始化无 Runtime/obs-control.json 时，只读旧根 `obs-control.json`，映射 host/port/microphoneInputName/defaultLiveScene 到新Rust格式；旧 executablePath 被忽略但原文件保留。目标存在不覆盖；源坏格式报告，允许用户显式重新配置，不悄悄连默认 OBS。
- 旧 Sessions 与新会话共根、按独立随机session目录写；不续写旧 Swift journal。`persistence::replay_legacy_session` 已有旧记录分支，`history::History::import_journal` 已有 schemaVersion=1/roomID/authorID 解析；提取共用 legacy event decoder 给两者，避免 replay 丢 UID，保留原始记录与未知旧提问字段。档案页显示“旧版提问操作已退役”，提供打开原归档目录/导出原始记录，不将旧提问状态变成新 AI 授权。
- 当前 API Key/OAuth 保持核心现有权限保护文件，不声称是 Keychain 加密。旧 OBS 密码**继续 Keychain**：新增公共 `ObsPasswordStore: Send + Sync`，同步 `read(host,port)->Result<Option<String>>`、`write(host,port,password)->Result<()>`。`ObsController::new` 增加该依赖；TUI注入原 environment→file store，GUI注入原 Keychain service `cc.ss-data.ShisuiDanmuDesk.obs-websocket`、account `host.lowercased():port`。Keychain 失败不回退到文件。
- 将 K 的 `OBSKeychainPasswordStore` 最小存储实现迁入 R 的公开 Swift binding companion、保留 MPL，适配为 FFI password foreign trait；不要复制 MPL 文件进私有业务源码。G 只注入该实现，密码走专用调用并保持在存储/连接层，不进观察快照。环境 `OBS_API_PASSWORD` 的现有显式覆盖仅在正常 TUI/正式GUI宿主适配中保留，QA禁止继承该变量。
- Runtime实例对 GUI 的 Runtime根持独占锁，第二个新版GUI只唤回现有窗口，不能双写/双开助手；不接管正在运行的旧GUI/TUI。已有旧GUI进程由用户自行退出，不能 pkill。

商业访问控制：保持 `CommercialRootView`/`ShisuiDanmuLicenseStore` 与授权库签名验证。私有 façade 在 Connect、发送、AI设置/启停/审核/授权、OBS操作、外部端点开启前统一检查 `allowsCommercialFeatures`，不能仅隐藏按钮。授权失效调用 runtime停止/撤权并等待已发请求按真相收束、关外部端点；不停止OBS、不删归档、不改变已有永久授权离线规则。授权/购买密钥不进入公开 Rust 包；不向免费核心增加支付门槛。

### 6. 在原生界面完整覆盖当前 TUI 操作

唯一功能基线为 `R/docs/terminal-feature-matrix.md:13-103` 的当前源码内容；不是其历史ACP表，也不是 G 旧22字符限制。每行矩阵都要对应 Mac 入口和验收结果，不以“常用功能完成”代替全部完成。

**面板**：工具栏为房间选择/连接状态、固定、工作台、更多；列表支持紧凑/聊天、名字/时间开关、字体与不透明度设置。底部有临时互动摘要、提示条、人工发送框、发送结果和累计看过/点赞/在线，以及 OBS/MIC状态。保持所有事件类型、主播身份强调、历史不抢滚动锚点、复制原文、选择回复对象/原生@。Enter换行、⌘Return发送，IME组合态不发送，粘贴只插入；发送框与候选编辑使用不同动作域。

**工作台**：原生侧栏“直播 / 助手审核 / 历史与重点”。直播为同一消息流；审核列表展示原问题、冻结发送身份、完整回复/分段、搜索来源状态及批准/编辑/丢弃。长候选完整展开且看到末尾后才启用“批准发送”；版本改变重置阅读确认。`⌘Return`仅在完整审核模式批准当前冻结版本；编辑中只保存回审核。批量操作先展示当前批次列表并原子确认，不包含后来候选；按住按键不会批准下一条。

**设置**：独立 Settings Scene，分“账号与直播间 / OBS / AI助手 / 外观与提醒 / 系统与关于 / 授权与更新”。

- 账号：主号扫码/退出、独立助手号扫码/确认UID/切换、房间号、房间标题/封面。与浏览器登录独立；无账号仍可公开监看。
- AI：模型协议、基址/模型/API Key、ChatGPT本人系统浏览器授权/重登/退出/模型目录、三方案、搜索/屏蔽词、人设/资料、处理可见弹幕/结束；本场范围与主题可直接在助手页调整。配置变更仍要求暂停、无在途/账号操作/发送许可/待续用，不新增自动重试、付费fallback或自动选模型。权限状态用文字说明，颜色只辅助。
- 历史：房间/关键词/日期检索、重点消息、Markdown/JSON导出、原档案位置、原有索引修复。写错误必须可见；修复复用备份/保留原件路径，不提供“删库试试”。
- OBS：真实WebSocket直连、host/port/password、场景/麦克风列表、静音、场景切换、有限推流启停与单路10Hz电平。移除obs-cli安装提示和旧轮询。停播默认取消，确认后3秒可取消；关闭弹窗取消尚未发出的请求。退出App和商业撤权都不停止推流。
- 系统：诊断、帮助、版本、构建核心哈希、数据路径、历史修复、CLI/MCP接入。内置命令搜索覆盖所有动作，显示快捷键；危险项进入对应确认界面，不直接执行。
- 外观：系统/固定深/固定浅；保留TUI四种配色作为内容配色选项（shisui/catppuccin-mocha/tokyo-night/gruvbox-dark），原生材质不随终端颜色实现耦合。高级文件级配置提供打开配置/资料位置入口及重启要求，不把这些TUI能力省略。

外部 CLI/MCP：`SetExternalEndpoint` 只有用户在设置中显式开启后，复用 `bridge::wire::serve` 在 GUI 核心上运行同样的loopback/token/0700目录锁端点。生成连接命令仍为用户单独安装的 `danmu --instance <GUI端点目录> agent ...` / `mcp`；GUI不安装、附带、启动TUI。保留 status/messages/report/reply/result、session/cursor/gap/幂等、MCP取消及同内置caller互斥语义；外部工具不能登录、改设置、授自动许可或调用桌面专用command。关闭端点/退出GUI移除本实例descriptor，不删除历史。

### 7. 未读与系统通知使用同一份私有策略

新增 G 的 `UnreadStore.swift`、`NotificationCoordinator.swift`；当前没有等价实现。所有窗口共享它们，不在每个View各自计数或发通知。

- 按房间保存 `unreadCount`、最后处理的session与累计值；在 `UserDefaults` 的 `danmu.unread.v1` 中编码。增量依据核心新消息累计值差值；同一消息/礼物upsert不重复累计。启动历史、归档恢复、replay和观察重同步不发通知；新会话累计值归零但原未读数保留，显式标记已读再清除。
- 自动标记已读必须同时满足：消息窗口实际可见、为key window、实时跟随在底部、当前房间未被只看重点等过滤隐藏消息。悬停预览不清零；回看旧消息不清零；点击“标记已读”可显式清除。切房间分别保存。纯进场/点赞默认不计数。
- 把手显示未读，菜单栏项目显示未读总数；仅 App 的Dock图标可见时更新其badge，0置nil。候选待审数来自核心Candidates，使用独立“待审N”标签，不混入红色弹幕数。不抖动、不反复弹出面板。
- 用 `UNUserNotificationCenter`，默认系统通知关闭；用户开启时才申请权限。普通弹幕只角标；启用后的默认通知类别为SC/上舰和助手致命故障，可显式开礼物/待审。点名通知只用现有结构化提及或完整名字的明确边界匹配，不调用模型、不过度按子串猜测；没有可靠目标就不发该类通知。
- 每房间3秒合并一批、最短10秒一条系统通知；同一故障状态只提醒一次，恢复后再次故障可重新提醒。消息窗口正在实时阅读时不发消息通知；故障仍在界面显示。默认无声音、正文预览关闭，只显示“直播间有N条重要互动”；用户打开正文预览后仍不包含Key/Token、内部诊断全文或私有资料。
- 点击消息通知只打开对应房间/事件（已不在实时窗口则检索归档），点击候选通知只进入审核。目标失效显示“消息已归档或候选已失效”，不自动批准/发送/切换授权。通知中不提供直接“发送回复”动作。
- 权限拒绝/系统专注模式下仍正常计数；不自绘绕过系统通知的角落浮层。通知位置由macOS决定，App不提供虚假的任意角落选择。半透明/置顶不代表OBS采集排除，界面不承诺隐身录屏。

### 8. 完成切换后的商业打包与退出行为

`Package.swift` 只保留新 DanmuCoreFFI、私有授权库、Sparkle 依赖；生成物/绑定与来源清单可复现，App的Contents/MacOS只有自身可执行文件，不包含danmu/obs-cli/node/Pi。保留Sparkle原生签名组件完整布局，不沿用旧脚本删除XPC服务的做法。

`AppDelegate.applicationShouldTerminate` 采用异步等待runtime.shutdown后再 reply 的原生退出流程，期间显示正在结束本场记录；退出错误留在可见错误界面，允许用户查看/再次退出，不伪报全部落盘。关闭工作台/面板仅隐藏窗口，菜单栏常驻，不结束会话。重开恢复房间/布局/尺寸/锚点/草稿，自动发送只能按核心已有显式保存且重新核验的授权规则恢复，不能因窗口恢复被开启。

分发路径固定为 Developer ID 直接分发，不转 Mac App Store。新增 `G/script/package_release.sh --version VERSION --build BUILD` 仅本地组装候选；version/build必须显式输入，不擅自重设用户已有商业大版本授权范围。正式模式要求环境 `DANMU_SIGNING_IDENTITY`、`DANMU_SPARKLE_FEED_URL`、`DANMU_SPARKLE_PUBLIC_KEY`，验证非空/HTTPS并设置 CFBundleShortVersionString/CFBundleVersion/SUFeedURL/SUPublicEDKey；缺少时失败，不读旧website的feed、不生成假值。签名采用由内到外 Developer ID + hardened runtime + timestamp，公证与上传须后续单独明确授权。

本地无正式参数时仍可交付第1步的可运行候选与完整源码包，不能称其已签名、公证或可销售发布。Sparkle在QA中完全不启动；正式配置缺失时不创建startingUpdater=true的控制器。真实更新安装过程中不得截断在途发送：已有会话需要用户确认后安全shutdown再交给更新器；不在直播期间强制退出。

## Critical files & anchors

执行前重读当前内容；行号仅为本轮定位，文件有用户改动时以新内容为准。

1. `R/src/terminal/mod.rs`：TerminalApp（约1067）、run（1375）、handle_ui_event（1945）、ingest_event（2274）、command（3045）——拆核的状态所有者与终端交互分界。
2. `R/src/terminal/send.rs`：TerminalTransport::send_confirm（17起）——注册回声matcher先于POST、身份与permit重验，不能被异步UI消费取代。
3. `R/src/terminal/agent.rs`：reviewed_reply/approve_review/review_all_candidates、enqueue（680）、set_automatic_permission（833）、restore_automatic（872）——审核冻结、双队列与自动授权的真相入口。
4. `G/Sources/ShisuiDanmuDesk/DanmuDeskStore.swift`：runtime/journal/ingressReconciler（30–44）、makeDefaultJournal（675）、clearSubmittedDraftIfUnchanged（750）——旧业务替换，禁止保留磁盘失败→内存journal的假成功fallback。
5. `G/script/build_and_run.sh`：pkill（23）、Sparkle裁剪（42–44）、run/verify（94起）——必须先修正副作用再用于验证，避免终止现有应用。

## Verification

### 前提与准确入口

本轮只读调查，未执行构建/测试/GUI；实际观察了 `danmu --help`、Swift 6.4 与 rustc 1.98.0。R当前统一门禁为 `script/verify.sh`，G当前为SwiftPM `script/verify.sh`；不引用不存在的Xcode scheme。执行者先完成实现，再跑统一验证；有行为失败必须修复，不重跑用户已报告错误来确认。目标macOS最低14保持不变，使用新系统API必须有14可运行路径。

`R/tests/fixtures` 当前为空。新增公开 `tests/fixtures/desktop-events.json` 为标准 `Vec<DanmuEvent>`，固定时间、房间123、可靠UID42/43，至少包括：两条新Danmu、一条SC、进场/点赞、重复跨来源消息、同批礼物交易与累计更新、Archived旧消息、中文/emoji长正文；不可含真实账号或平台凭据。新增Swift legacy journal fixture覆盖schemaVersion=1、roomID、authorID、旧提问记录和半条尾记录。fixture是隔离测试输入，不是产品fallback。

新增 `test-support` 仅debug可编译，用于无网络LocalTransport、假账号/OBS和官方订阅loopback端点；release启用时compile_error。该feature由danmu-ffi显式转发给根库，不在生产默认feature中。普通生产配置/环境不得覆写官方订阅端点。复用当前 `runner/subscription` 的cfg(test)签名fixture，通过同一个test-support模块供GUI QA调用，不使用真实Token。把 `script/agent_smoke.py` 中 `Scenario/ModelDriver` 与私有文件辅助函数提成公开 `script/smoke_support.py`，显式传protocol/private路径，保持原TUI smoke行为；不在import时启动进程或解析全局CLI。

在G新增 `script/run_qa.sh --scenario panel|parity|lifecycle|load --output ABS_NEW_DIR`。它构建/启动QA App，不装进Applications，不扫描HOME；创建0700隔离根，通过debug专用 `--qa-root` 与 `--qa-scenario` 注入，所有根/环境/模型与授权服务均指向该目录和literal loopback。启动在只允许loopback的sandbox中，复用现有TUI smoke的fail-closed规则；sandbox不可用则不运行联网UI。QA驱动使用真实Rust核心、合成平台Adapter及假授权签名，不把界面状态直接赋成成功。退出仅操作本次持有PID，不能按进程名终止。

QA窗口有debug专用控制入口“注入下一批 / 注入历史 / 注入重复 / 故障 / 恢复”，驱动真实事件入口，供电脑工具用AX定位；release中无此入口或QA参数。截图与结果写入指定output，不把凭据fixture复制到交付证据。

### 核心、TUI 与绑定验证命令

R工作目录：

```sh
cargo test --locked --no-default-features --lib
cargo build --locked --no-default-features --lib
cargo test --locked -p danmu-ffi
RUST_TEST_THREADS=1 ./script/verify.sh
python3 script/agent_smoke.py --binary target/release/danmu --output /tmp/danmu-mac-tui-openai-<unique> --protocol openai_chat
python3 script/agent_smoke.py --binary target/release/danmu --output /tmp/danmu-mac-tui-anthropic-<unique> --protocol anthropic_messages
```

`<unique>`由本次UUID替换且目录必须尚不存在，脚本root.mkdir不允许复用。订阅smoke：用 `cargo test --locked --lib --no-run --message-format=json` 输出中 `profile.test=true` 的 `compiler-artifact.executable` 获取实际lib测试程序路径，再调用同一脚本 `--protocol chatgpt_subscription --test-binary <该路径>`；不得猜测hash文件名。保留 `terminal::agent::tests::subscription_pty_instance` 入口给现有PTY脚本。

新行为回归至少覆盖：无任何terminal consumer时极速回声早于POST返回仍恰好确认一次；UI观察暂停/重同步不影响发送；隐藏窗口期间归档继续；两个GUI窗口共用一次连接；独立助手队列失败不影响人工；暂停/切房/凭据更换取消迟到候选；写盘失败不改真实已确认结果；停止OBS确认token过期/取消不发请求；GUI专属工作区不引用TUI默认目录；旧Swift档案UID不丢。

G工作目录：

```sh
./script/sync_core.sh --source ../danmu --configuration debug --arch arm64
./script/verify.sh
./script/build_app.sh --configuration debug --variant qa --arch arm64
./script/run_qa.sh --scenario panel --output /tmp/danmu-mac-panel-<unique>
./script/run_qa.sh --scenario parity --output /tmp/danmu-mac-parity-<unique>
./script/run_qa.sh --scenario lifecycle --output /tmp/danmu-mac-lifecycle-<unique>
./script/run_qa.sh --scenario load --output /tmp/danmu-mac-load-<unique>
```

G现有tests使用Swift Testing；沿用，新增不变量/状态迁移测试，不写仅证明view字段、复制值或mock转发次数的测试。已存在旧默认/措辞测试应删除而非重新钉死新文字。纯构建不是GUI证明。

### 必须在真实原生窗口观察的场景

使用computer工具、任务级授权、AX语义操作和新鲜窗口截图；不使用浏览器代替原生表面，不抓用户其他窗口内容。每个场景记录实际结果，未具备的屏幕/系统/服务条件标“未验收”，不能用几何单测冒充实测。

| 输入/操作 | 必须观察到的结果 |
| --- | --- |
| 拖到左/右外边缘，离开800ms，悬停200ms | 收成边缘把手；展开无键盘焦点变化；点击发送框才获得焦点；快速经过不闪开 |
| 贴边后中文IME输入、选择文本、打开审核/登录/停播sheet | 窗口不自动收起；Esc优先退出当前操作；无误发送 |
| 收起时注入2条新Danmu、1条SC、进场/点赞及同消息重复 | 未读为3；历史/重复/礼物upsert不重复增加；悬停仍为3；聚焦未过滤的实时底部后为0 |
| 回看旧消息时注入新消息，关闭再打开工作台/面板 | 阅读锚点和草稿保留，新消息提示存在；没有第二条连接或重复归档 |
| 新room切换后再回原room，重启QA App | 两房间未读和界面位置按规则恢复；历史不触发toast/AI；旧权限不跨房间继承 |
| 候选长文查看、编辑、批准、连按快捷键、审核中变更身份 | 未看全不能发送；编辑只保存；过期revision/身份被拒；不误批下一条；原人工草稿保留 |
| loopback模型两协议、订阅授权/目录/大SSE/额度错误 | 当前源码完整候选与真实LocalTransport结果闭环；额度错误暂停撤权，不转API、不重试旧批 |
| 人工发送期间重打同样正文，随后迟到确认；同时助手分段发送 | 新草稿不被清空；独立身份队列互不阻塞；未知结果显示未确认且不重发 |
| 假OBS连接/场景/电平、停播默认取消、确认后2秒Esc | 状态来自真实协议处理；取消场景StopStream请求数为0；App退出/授权失效也为0 |
| 系统通知关闭/拒绝/允许/专注模式；重要消息连续到达 | 角标始终可用；通知按规则合并，无正文预览默认；点击只定位，不批准；不强行绕过专注模式 |
| 暂停GUI观察消费后大量事件，再恢复 | bounded内存，显示重新同步；未读差值正确，无旧toast爆发；收发与归档未被阻塞 |
| 旧Swift会话+异常半条尾记录+旧B站/OBS配置 | 原件hash不变，完整旧消息可读且UID正确，不恢复旧自动发送；坏配置明确失败，Keychain不落明文 |
| 授权撤销/试用到期，关闭窗口、⌘Q、再开 | 商业门控不可经菜单/快捷键/外部端点绕过；撤未来发送但保真相；仅⌘Q结束runtime；不停止OBS |
| 多屏、拔插、混合缩放、Spaces、全屏、睡眠唤醒 | 把手/窗口仍可达，无跨屏藏窗；无焦点劫持；唤醒不重放旧发送；缺设备明确记录未验收 |
| 深浅色、不同桌面背景、Reduce Transparency/Motion、键盘/VoiceOver | 内容可读、聚焦与按钮可达；材质/动画正确降级；数字角标有语义标签 |

性能验证由新增 `G/script/run_qa.sh --scenario load --output ABS_NEW_DIR` 驱动：连续30分钟，每秒注入100个唯一合成事件，在第10分钟增加持续5秒的每秒1000事件峰值；沿同一生产摄入/归档路径进入，AI默认暂停、无真实网络。检查UI仍可打字/暂停/收起，RSS在有界列表/队列后趋稳，无主线程持续处理网络/归档，无观测到的业务事件/送达重复。此为验收负载而非对平台实际吞吐承诺；记录设备、CPU/RSS与UI卡顿实测，不填未经测量的目标数字。

本地release候选检查 `lipo -archs` 两架构、`otool -L` 仅包内/系统依赖、codesign完整性、无TUI/obs-cli/测试支持/秘密文件、core source SHA一致。Developer ID身份、真实appcast和公证授权齐备之后才执行正式签名、公证并验证 `codesign --verify --deep --strict`、`spctl --assess --type execute`、`xcrun stapler validate`；本计划不会自动上传或发布。

### 验收后的文档与交付证据

完成真实窗口冒烟后，更新现有 `R/docs/terminal-feature-matrix.md`，逐项记录共享核心/TUI/Mac入口与保持不变的安全约束；更新现有 `G/README.md` 的Rust绑定构建、窗口交互、设置/数据位置、授权边界、隔离QA和发布步骤，移除已退役的旧SDK/obs-cli/提问流程说明。只保留可重复运行的验收驱动，删除本轮临时调试脚本，不把测试录屏或假凭据装进App。

最终交付附：本地App路径与构建variant、核心输入hash/源码归档hash、实际执行命令及退出状态、贴边/悬停/角标/审核/撤权的截图或录屏、旧档案只读兼容结果、未获授权或缺设备而未验收的项目。任何未测事项不得标“通过”；完整功能实现、本地证据、正式签名/公证/真实服务验收分别列明，不混成一个“完成”。

## Assumptions & contingencies

- 已确认的用户选择：私有GUI+公开MPL核心；升级现有私有App；只以当前TUI完整功能为基线，退役旧GUI提问工作流并保留原档案。若用户以后改变闭源范围或要求保留旧提问流程，必须重新规划，不在实现时私自扩展。
- 分发选Developer ID而非App Store，最低macOS14，正式支持Universal 2；本机ARM可直接验证，Intel/macOS14/多屏若暂不可用，完成构建与所有可达验证，明确列实际缺失的运行证据，不删支持目标，也不伪造通过。
- 未来商业化不等于本轮发布：保留现有收费/试用/授权配置，但不操作生产用户、订单、证书信任、云端secret、appcast或下载站。缺签名/公证资料时交付明确标注的本地候选与脚本，不生成假配置。
- 真实B站发送、模型/官方ChatGPT账号额度、公网搜索、OBS实际推流仍需用户单独明确授权。无授权时使用loopback/合成账号/假OBS完成可达链路，并将真实服务兼容性作为未验收条目，不把LocalTransport confirmed写成B站实发成功。
- 执行时工作树可能已有变化：先读取当前文件/相关规则并保留；不reset/clean/stash/切worktree来避开。若已有对应runtime或窗口实现，按本计划契约调整那个唯一实现，不建立第二套。
- 工程找不到安全Application Support目录、发现旧目标冲突、核心版本/生成绑定不匹配、Keychain访问被拒绝，均明确失败并保留原件；不得回退到TUI数据、公开生产配置、临时内存journal或旧Swift发送实现。
