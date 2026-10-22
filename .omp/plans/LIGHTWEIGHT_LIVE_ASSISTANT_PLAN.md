# 轻量直播值班助手实施计划

## Context

用户要求为知识型主播的弹幕台减轻 AI 运行依赖、改善直播可用性，同时保留历史记忆。用户已选择：内置原生 ACP 直接替换为 API；支持 OpenAI 兼容和 Anthropic 两种接口；保留受限联网搜索。目标为 `/Users/elazer/Desktop/Workspace/90_Dev/shisui-danmu` 的 Rust TUI，不修改私有 Swift GUI，也不代配真实凭据或操作直播。

最终只有一条内置值班链路：应用选择消息和记忆 → 单飞模型 HTTP 请求 → 必要时一次搜索和一次补全 → 整批校验 → 原 Bridge 审核/授权/发送。下播资料整理继续使用已有外部 Agent 接口，不新增复盘自动化。减少依赖不等于已经确定历史不稳定的根因，也不承诺模型必定正确。

## Approach

### 1. 提取模型无关契约，再实现两个 HTTP 适配器

步骤1先只迁Candidate/parser及其直接调用，ACP仍保留自己的旧Answer/Prompt/Handle及旧运行测试；让InvalidCandidateTarget在model模块对兄弟ACP可见以保留旧错误分类，不把新增Answer变体提前塞给旧Runner。步骤2/3的新模块测试与旧测试并存，所有cfg(test)源码仍须可编译，cargo过滤不等于只编译一个模块。步骤4一次切换Runner/TUI/旧业务测试并删除ACP；不发布混合后端中间状态，不新增永久兼容层。

**复用并迁出候选校验。** 新建 `src/runner/model.rs`，把 `src/runner/acp.rs:295-349` 的Candidate、私有Plan和 `candidates(text: &str, round: &str, ids: &[String]) -> anyhow::Result<Vec<Candidate>>` 移入；复用唯一现有实现，保留deny_unknown_fields、当前轮、本批ID、目标唯一、最多8条与Bridge::validate_reply_text。过渡ACP直接调用新模块，runner/tests.rs直接解析调用同步迁移，不留重导出。模型内容错误只报告稳定类别（格式/轮次/目标/数量/正文），未知或重复目标仅保留最多80字符转义ID；serde原错误可能带出字段名/值，不串接其原始错误链、未知字段文本或原始模型正文。外层Protocol错误同样不回显响应；以合成私有sentinel验证错误诊断和Journal不泄露。

新模块定义以下应用契约（均 `pub(crate)`，不对外导出SDK类型）：

```rust
struct SearchTarget { message_id: String, query: String }
struct Prompt {
    round_id: String,
    text: String,
    message_ids: Arc<[String]>,
    cancel_epoch: u64,
    deadline: std::time::Instant,
    web_search: bool,
    search_targets: Vec<SearchTarget>,
}
enum Answer { Candidates(Vec<Candidate>), NoMessage, Rejected(String), Cancelled, Expired }
enum SearchState { NotRequested, Completed, Unavailable }
struct RoundOutput { answer: Answer, returned_model: Option<String>, search: SearchState }
```

**配置类型先独立定义，步骤4才替换 Settings。** 在 `src/runner/settings.rs` 定义 `ApiProtocol::{OpenaiChat,AnthropicMessages}`，serde值分别是 `openai_chat`、`anthropic_messages`；`OpenaiTokenLimit::{MaxCompletionTokens,MaxTokens}`，serde值是 `max_completion_tokens`、`max_tokens`。定义 `ApiConfig { protocol: ApiProtocol, base_url: String, model: String, max_output_tokens: u32, openai_token_limit: OpenaiTokenLimit }`，derive Clone/Debug/Eq/serde，拒绝未知字段。默认是OpenAI协议、`https://api.openai.com/v1`、空模型、4096输出上限、`MaxCompletionTokens`；Anthropic默认基址为 `https://api.anthropic.com/v1`。

- 普通设置保存允许空模型/空地址，表示尚未配置；非空地址必须有效。启动另行要求地址、模型与匹配Key齐备。
- `base_url` 最多2048字节，统一通过 `normalize_base_url(input: &str) -> anyhow::Result<String>`：trim、Url::parse、拒绝userinfo/query/fragment/控制字符，再取url序列化结果去掉末尾 `/`；使用url库自身的scheme/host大小写、IDNA、默认端口和IPv6规范化，不做第二套字符串猜测。设置保存与Key记录读取/保存均调用它，按协议+规范化结果比较；模型不参与凭据作用域。HTTPS可用，HTTP只允许IP字面量127.0.0.0/8或`::1`，不允许localhost等DNS例外。无效存量URL是错误，不能当缺Key。
- 基址是包含版本路径的服务根，追加 `/chat/completions` 或 `/messages`，不能用会丢弃已有路径前缀的根路径join。
- 模型名trim后1..256字节、无控制字符；输出上限256..8192。`openai_token_limit`是文件级兼容选项：防止旧兼容服务只接受`max_tokens`、新OpenAI模型只接受`max_completion_tokens`。不根据域名/模型名猜测，不收到400后偷偷换参数重试；Anthropic忽略该OpenAI专用项，固定发送`max_tokens`。

**新建 `src/runner/api.rs`。** 当前没有可复用的Rust LLM HTTP客户端；使用现有reqwest/tokio/serde_json，不加供应商SDK、异步trait工厂或通用Agent框架。普通enum分派两个协议的序列化/解析即可。客户端独立于B站Cookie客户端，禁重定向，不携带Cookie、Bridge token或宿主环境认证；loopback客户端禁代理。模型和搜索客户端均显式设置 `ClientBuilder::retry(reqwest::retry::never())`：当前锁定reqwest 0.12.28默认会重试协议NACK，不能只在应用代码中不写重试循环。

`api::Handle::spawn(config: ApiConfig, key: credentials::ApiKey) -> anyhow::Result<Handle>`只验证本地配置和构建客户端，不联网。沿用现有有界worker设计：容量1的 `mpsc::Sender<Prompt>`、容量8的事件通道、cancel epoch和stop watch；`epoch()/cancel()/stop()`及异步task所有权保留。事件仅为 `Started { round_id }`、`Answer { round_id, result: Result<RoundOutput, ApiError> }`、`Closed(Result<(), ApiError>)`，不再有Ready/Configured/原生快照。`ApiError`定义 `Http(u16)`、`Transport(&'static str)`、`Protocol(&'static str)`、`Configuration(&'static str)`；不给错误附加服务原始正文、请求headers或含地址的reqwest错误链。

每轮执行规则：

1. 比对epoch/stop和deadline后才产生Started并发请求，排队期间取消的轮次不能发HTTP。
2. 每个模型请求 `stream:false`，连接超时10秒；整个普通轮次共用 `min(发起时+180秒, 最早消息120秒截止)`，含搜索和第二次补全。worker到deadline统一返回Answer::Expired；Runner在普通Flight上记expired并继续新消息，在repair Flight上按180秒操作超时明确暂停撤权。这是两种业务期限，不让transport猜轮次类型。
3. 通过分块读取把解压后的完整模型HTTP响应限制为256KiB，缺Content-Length也有效；先有界读取后解析，不能先无界text()/json()再截断。HTTP请求JSON最多48KiB（覆盖24KiB应用上下文、工具定义和最多16KiB搜索片段），超限不发HTTP。
4. cancel/stop和deadline包住整个HTTP/搜索future；取消丢弃响应future和连接读取，不声称远端推理或收费已经停止。worker Drop须终止自有task，shutdown必须可完成，不产生孤儿任务。
5. 使用完整应用输入，不保留供应商会话或跨轮messages数组。只有同一轮搜索补全可附加第一次assistant工具调用和该次tool result。

协议请求/响应固定如下，不能回退到另一协议、模型或URL：

| 协议 | 请求 | 响应准入 |
|---|---|---|
| OpenAI兼容 Chat Completions | Bearer Key；`model`、`stream:false`、两个初始messages（system=`workspace::CONTRACT`，user=Prompt.text）；只发送配置选定的一个输出上限字段。不附带temperature、thinking、response_format或供应商专用参数。 | 顶层JSON对象；恰好一个choice且index=0、role=assistant。`finish_reason=stop`时仅字符串content作为候选正文；null/空白为NoMessage。`tool_calls`只在步骤2准入。`length/content_filter`为本批Rejected；缺少/未知结束原因、坏envelope为Protocol错误。忽略非正文reasoning等附加字段，不把它们当候选。 |
| Anthropic Messages | `x-api-key`、`anthropic-version:2023-06-01`；`model`、`max_tokens`、`stream:false`，顶层system=`workspace::CONTRACT`，初始messages仅一条user=Prompt.text。不启用extended thinking/beta/server tools。 | type=message、role=assistant；`stop_reason=end_turn`仅合并text块；无正文为NoMessage。`tool_use`只按步骤2；`max_tokens/refusal`为本批Rejected；缺少/未知结束原因或非本契约内容块为Protocol错误。 |

解析顺序固定：先HTTP状态、有界完整正文、JSON/外层结构与类型，再结束原因，最后候选/工具正文；拒绝原因不得因缺正文被误判NoMessage。OpenAI要求object=chat.completion、恰好一项index=0且role=assistant；Anthropic要求type=message、role=assistant。允许常规供应商元数据，不接受错误类型。length/content_filter、max_tokens/refusal及非空OpenAI refusal均整批Rejected，正文可空；stop/end_turn分支若夹带任何tool调用也整批Rejected，不解析其候选。tool_calls/tool_use结束分支只执行步骤2准入，关闭/预算耗尽/缺少工具块均Rejected，零搜索；该分支的解释text忽略。其余/缺少结束原因为Protocol。只有正常结束且确实无工具的null/空文本才是NoMessage；不启用thinking，因此Anthropic非text/tool_use块为Protocol。

官方协议参考：`https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create`、`https://platform.claude.com/docs/en/api/messages/create`。这里的“OpenAI兼容”明确是上述Chat Completions契约，不含Responses、Azure专用认证或所有厂商变体。

### 2. 在应用中执行一次受限搜索，不保留原生工具循环

新建 `src/runner/search.rs`；无独立Rust搜索客户端，按已读 `src/runner/hosts/pi_search.js` 的HTTP逻辑迁移，不另选供应商。生产地址固定 `https://mcp.exa.ai/mcp?tools=web_search_exa`，与对外 `danmu mcp` 无关。

- MCP保留initialize（版本2025-03-26，clientInfo为shisui-danmu-search/1）→notifications/initialized→tools/call（web_search_exa、numResults:3）；请求id分别1、2。存在session时沿用字母/数字/下划线/连字符1..128校验。未取消时结束后最多1秒best-effort DELETE；取消时不再发清理请求。
- 各响应分块读取最多96KiB；application/json与SSE均按JSON-RPC版本、请求ID、唯一响应匹配；SSE按空行组事件、多行data合并，不把网络chunk当事件。仅接纳文本content，最终最多4000个Unicode标量，保留来源URL，不主动抓取任意URL。一次搜索含握手/清理共用20秒和本轮deadline；禁重定向及reqwest底层重试。
- **查询由程序构造，不由模型自由生成。** launch只从本批可回复的Danmu/SC公开正文生成SearchTarget：trim后非空、无控制字符、1..240个Unicode标量，query逐字等于该正文，不附加UID、昵称、topic、历史、资料或其他消息；不符合上限的整条不提供搜索目标，不截断或拼接。最多8个目标随Prompt冻结。取舍：依赖私有记忆才能补全的指代问题不做扩写搜索；回答可核实部分或交主播，不增加隐私分类器。
- Exa只收到冻结query和numResults，不收到模型任意参数或上下文对象。资料/历史内的合成秘密即使诱导模型请求搜索，也不能经额外query字段或旧message_id出站；字段/目标违规整批Rejected而不是执行子集。
- 失败、429、无结果、错ID、超限为Unavailable，不重试、不切其他/付费服务、不伪造核实。公共额度及实际可用性未验收，不承诺永久免费可用。

工具状态机：

1. Prompt.web_search=false或search_targets为空时不携带tools，任何返回工具请求整批Rejected、零执行；repair始终无工具。
2. 开启且有目标时只提供web_search，schema为 `{type:"object",properties:{message_id:{type:"string",enum:[本批可搜索ID]}},required:["message_id"],additionalProperties:false}`。OpenAI使用function工具和tool_choice:"auto"；Anthropic使用input_schema和tool_choice:{type:"auto",disable_parallel_tool_use:true}。工具描述说明“用指定本批完整公开问题搜索；不能提供query或扩写私有上下文”。
3. 第一次输出是最终候选或恰好一个web_search调用。tool-call ID须1..128字节且无控制字符；message_id必须命中冻结SearchTarget，再由程序取query。多个、未知、非法参数调用整批Rejected、零搜索，不执行合法子集；工具前的解释/候选不入队。
4. 结果是非可信tool result：OpenAI追加assistant的唯一tool_calls与role=tool、原tool_call_id；Anthropic追加assistant唯一tool_use块与user tool_result、原tool_use_id。不回送被忽略的解释正文，避免无关输出扩大第二请求。成功内容包含公开查询和有界来源文本；失败用明确“本轮搜索不可用，未核实”（Anthropic is_error:true），不升级为system指令。
5. 最多一次第二模型请求。移除工具定义，system追加固定“搜索预算已用完，只返回最终候选JSON”；再次工具请求Rejected，不产生第三次模型请求或第二次搜索。
6. Unavailable后若产生候选，Runner加search_unavailable待审原因，禁止本轮自动发送；空候选正常结束。SearchState序列化为not_requested/completed/unavailable，诊断显示真实状态。

生产URL不设环境/配置后门。测试只在cfg(test)构造器注入loopback地址，通过真实HTTP证明MCP成功/失败。

### 3. 将模型Key放入独立私有存储，复用秘密输入能力

新建 `src/runner/credentials.rs`，因为现有Pi/OMP认证属于原生宿主，不可当作应用Key管理复用。路径固定为 `assistant.json`同目录下 `assistant-secrets/api-key.json`；一个当前连接，不建立多供应商Key注册表。

定义 `ApiKey(reqwest::header::HeaderValue)`，不derive Debug/Serialize，不提供Display；HeaderValue设置sensitive。提供：

```rust
fn load(config_path: &Path, api: &ApiConfig) -> anyhow::Result<Option<ApiKey>>;
fn save(config_path: &Path, api: &ApiConfig, input: &str) -> anyhow::Result<()>;
fn clear(config_path: &Path) -> anyhow::Result<()>;
```

磁盘私有记录严格为 `{ "format":1, "protocol":"openai_chat|anthropic_messages", "base_url":"规范化地址", "key":"..." }`，拒绝未知格式/字段，最多16KiB。Key只trim两端空白，剩余1..4096字节、非空且均为可见ASCII；不支持环境变量展开、shell命令或URL内认证。

- 私有根是assistant-secrets而非整个配置父目录。新建时用 `bridge::wire::private_root` 建Unix0700；既有根权限过宽或符号链接直接拒绝，绝不chmod。配置父目录允许惯常0755，但Unix须当前UID所有且不可被组/其他人写入；先解析并固定父目录真实路径，再检查根，不能默默改父目录权限。文件检查复用 `workspace_config::check_file/private_file` 的普通文件、非符号链接、Unix当前UID/单链接/无组及其他人权限；O_NOFOLLOW/O_NONBLOCK打开后fstat复核、有界读取。使用 `storage::write_private_atomic` 写Unix0600，失败保留旧文件。Windows沿用项目现有文件安全能力，不宣称Unix等价ACL或加密；这不是抵御同UID恶意进程的沙箱。
- 读取返回None仅限文件不存在或协议/规范化base_url作用域不匹配；损坏/不安全路径返回明确错误。换地址/协议不能自动发送旧Key；同服务换模型可复用Key。文件不自动同步到工作区或Journal。
- clear只清除此私有Key文件，不删目录/旧宿主凭据；缺失视为已清除，不安全路径拒绝。成功清除/更换使旧idle handle失效，下次显式启动重新读取。运行中的Key是启动时快照：外部手工编辑/删除文件不触发实时监视，也不改变已发请求；须先Ctrl-P停用本机worker、再启动才使用新文件，服务端撤销Key另行操作。
- Key状态只在加载、打开模型页、保存、清除和启动时核对并缓存；不得每帧读取密钥文件。页面只显示当前连接“已设置/未设置/文件错误”，永不回填旧Key。

### 4. 原子切换配置、Runner与TUI，并退役ACP

以下子项必须作为一次可编译、可运行的切换落地；不要先发布format=4而运行链仍是ACP，也不要保留高级ACP入口。

#### 4.1 配置迁移

`Settings.format`改为4，以 `api: ApiConfig`替代 `host/binary/source/native_preferences`；其余资料、回复积极性、动态策略、感谢、mention/repair、词表、workspace与skills字段保持现有意义。`automatic/resume_on_start/web_search`继续存在，但旧后端迁移时三者都置false：旧授权和搜索服务不能被新后端自动继承；原偏好仍在备份。format=4的日常加载不重置这三项。

保留 `Settings::load(path) -> Result<(Self,Option<String>)>` 的加载不写、32KiB上限、损坏不覆盖和未知字段/版本拒绝。先只分辨format，再进入对应strict legacy parser；绝不在format4上预先删除或容忍旧字段。三套私有数据结构如下，不留下运行时Host/Source/NativePreferences或别名：

| 输入 | 私有schema与映射 |
|---|---|
| 无format | 仅接受旧Previous的host、binary、name、style、topic、scope、model、strength、automatic，沿用其serde缺省与类型。name空则现默认名；style/scope非空项用原“；”合并为preferences。topic/model/strength/宿主字段仅读取后退休，不继承场次；automatic强制false。 |
| format2 | 完整LegacySettings字段同下一行，另接受可选旧topic并丢弃；必须是format2。 |
| format3 | format、host、binary、name、preferences、use_profile、use_project、workspace、source、automatic、resume_on_start、reply_activity、dynamic_host_priority、dynamic_muted_support、thank_gifts、thank_likes、thank_follows、thank_shares、mention_sender、repair_blocked、web_search、blocked_words、native_preferences、skills、persona；缺省值固定为当前format3的Default，不借用新Settings::default推算。 |
| 旧嵌套字段 | LegacyHost接纳当前已知serde值及旧加载器专门接受的vs_code；LegacySource仍是kind/value的default/omp_profile/native_prompt；NativePreferences严格为host/binary/source（必需）及model/thinking（Option），各层拒绝未知字段、错类型。只作已知数据解析，不启动宿主、不解析OAuth或读取原生提示文件。 |
| format2/3转换 | 除host/binary/source/native_preferences与format外逐字段保留；api用新默认值，automatic/resume_on_start/web_search置false。沿用当前harden对use_profile/use_project/dynamic_host_priority/repair_blocked的既有强制规则；其余业务字段走原验证。不存在的字段沿用旧缺省。 |

Legacy默认快照：host=Omp、binary/preferences空、name=拾穗小助手、workspace=None、source=Default、reply_activity=Cautious、persona=Assistant；use_profile/use_project/dynamic_host_priority/dynamic_muted_support/repair_blocked=true；automatic/resume_on_start/各thanks/mention_sender/web_search=false；词表/native_preferences/skills为空。已退休模型、thinking、OAuth、OMP profile与NativePrompt绝不推测为API配置。

第一次显式保存旧格式前，将原始字节存到 `assistant.pre-api.json`，已存在同内容可复用、不同内容拒绝覆盖，备份失败则不提交设置。保留旧备份，不删历史。迁移通知必须说明：资料与归档保留；需配置API和重新启动/授权；旧原生规则来源不再加载，需本人将必要文本放入既有SYSTEM/人设/PROJECT或preferences；不自动扫描/复制原生配置。保存统一复用私有原子写入，提交成功才更新内存。

`Settings::validate`允许API未配齐以支持分步设置；`Settings::ready`要求合法完整ApiConfig，不再检查POSIX/可执行程序/宿主搜索白名单；`Runner::start`再要求匹配可读Key。旧remembered_automatic文件不因加载修改：现有启动续用入口依赖automatic，restore_automatic同时要求automatic && resume_on_start，迁移置false足以阻止恢复，不另加迁移状态位。回归必须实际放入匹配旧授权文件，证明没有启动/授发送权；仅现有显式自动授权流程可重新开启。

#### 4.2 Runner无状态请求，业务状态不丢

修改 `Runner::save(&mut self, settings: Settings, bridge: &Bridge) -> Result<()>`、`Runner::start(&mut self, bridge: &Bridge, visible: bool) -> Result<()>`，移除rebuild参数；修改 `TerminalApp::start_assistant(&mut self, visible: bool) -> Result<()>`。删除 `change/connect_options/prepare_connection/connect/begin_restart/native_id`、原生report/pending/configuring/needs_rebuild、临时ACP workspace、native_prompt_text、restorable、配置ACK、restart_*和上下文累计轮换字段；保留应用scene、driver、generation、flight、history、workspace_context、quiet_rounds。

- 启动准备现有资料/历史，加载Key并创建HTTP handle；无新消息就零HTTP，不请求模型列表、不做付费“连接测试”。界面“等待新消息”仅表示本地可执行，不称远端已鉴权。
- `Flight`仍冻结轮次/场次/generation/消息ID/设置/动态策略/资料/送达事实，把native_session/options换成 `api: ApiConfig`；API配置和Key不能在运行或有在途轮时更改，因此不保留不可能发生的原生options_changed回执判断。通过统一保存/Key入口拒绝这类更改，不能只灰掉UI而内部入口仍能改。
- API配置、Key、web_search提交统一要求 `!running && !in_flight() && !stopping && !bridge.sending_enabled()`，否则提示先/pause；TUI若resume_pending也拒绝提交，防止等待续用期间换连接。明确新增 `Runner::save_api_key(&mut self, input: &str, bridge: &Bridge) -> Result<()>` 和 `Runner::clear_api_key(&mut self, bridge: &Bridge) -> Result<()>`，与Runner::save共享生命周期检查。TUI只调Runner入口，不直接调用credentials写盘。单TUI事件循环中同步完成“校验当前状态/作用域→原子落盘→更新缓存状态并退役旧idle handle”，中途无await或重入；失败不改设置/handle/旧Key且保留秘密编辑内容。其他现有偏好沿用本轮冻结/下轮生效，资料变化撤权边界不变。
- launch/launch_repair提交新Prompt，保留consume_routing_models/单飞/背压及消费本批后不重放。search_targets直接从冻结Batch的原消息正文构造，不再次解析prompt JSON找私有资料。repair强制web_search=false和空search_targets，只接纳原消息一条候选；NoMessage/空/多条/改目标均Rejected，原失败结果保留、不再repair。继续使用原Permit/阻断原因/retry_of/confirmed_segments/unconfirmed_segments/duplicate_risk：已确认段绝不重发，未确认段最多改写一次。
- pause先停分流、递增generation并撤权，再stop worker（同时取消在途HTTP）；保留driver所属业务场次、待审候选、人工草稿和真实平台发送的确认。处理完取消/Closed再允许启动，无需“重建上下文”。stop另释放driver；shutdown等待自有task完成。
- 资料每秒刷新仍走 `refresh_workspace_context`：旧在途结果和未发自动候选失效，待审/人工草稿保留，资料缺失不沿用旧值；无原生会话不意味着可以删这些校验。上下文改变时取消尚在进行的本轮模型/搜索future，绝不重试本批。
- 普通结果超过原消息120秒截止标expired并释放lease；结果迟到、generation/场次不匹配或暂停后均不入队。Started和结束报告归属检查保留，不能把worker内部归属错误当普通模型拒答。

错误边界明确如下：

| 观察到的结果 | 行为 |
|---|---|
| 完整合法envelope内的空候选/无正文 | no_reply/no_message；继续新消息，沿用5/10秒空轮合批，不重放旧批。 |
| 完整可归属envelope内的候选JSON畸形、旧round、未知/重复目标、非法正文、数量超限、模型输出截断/拒绝、越权工具请求 | 整批rejected、零入队、不重试；保留已有候选和当前许可，继续新消息。这里拒绝的是模型内容，不是吞掉外层协议错误。 |
| 搜索不可用 | 记录Unavailable，仅一次补全；产生的候选强制待审，不假报核实。 |
| HTTP非2xx（含401/403/429/5xx）、DNS/TLS/连接/响应中断、外层JSON/协议不匹配、超限响应、repair180秒超时 | 本批error，暂停并撤权；用户处理后显式启动，不自动重试、回退模型或切换服务。TUI收弹幕和人工输入继续。 |
| 取消/普通消息deadline | cancelled/expired；不送候选；只有用户暂停才停止后续运行。 |
| worker错归属/意外终止、发送身份/授权/当前存储故障 | 明确错误并安全停止相关助手发送；不得吞成无候选。 |

复用advance_bridge的落盘汇合与“round journal成功后才take_ready入发送队列”的既有顺序，不新增第二套WAL。致命停止要取消resume_pending、remember_enabled(false)、forget_automatic；参考现有reject_automatic_identity逐项合并清理失败的方式，避免调用返回()的pause_assistant后用原错误覆盖掉清理错误。原始错误始终保留，清理失败不能称持久撤权成功。模型内容Rejected不走全局撤权。真实Journal写失败仍传播或按现有边界暂停，已确认送达不改成失败。

`round_completed`不再写native_session，新写 `api_protocol/requested_model/returned_model/search_state`，只记录有实际证据的返回模型与搜索状态；错误时未知值用null。其余原有outcome/耗时/输入字节/实际受理状态保持。旧Journal不改写；`SessionJournal::assistant_stopped(&self, session: &DanmuSession, reason: &str)`去掉native_session参数，新记录只保留reason，轮次信息来自既有round_completed。TUI诊断显示协议、配置模型、实际返回模型（如有）、本轮状态，不显示Key/headers/完整prompt/原始响应。

`workspace_config::assistant`继续用白名单投影：删host/source_kind/omp_profile/native thinking，增加api_protocol、model、max_output_tokens和openai_token_limit；不放Key、凭据路径、API地址或认证信息，不改成serialize(Settings)。旧观察记录保留，绝不用于恢复授权。

#### 4.3 设置与秘密输入

复用 `/settings` → AI助手 → 模型，以及 `/ai model`。Models页仅显示协议选择、API基址、模型名、设置/更换Key、清除Key；max_output_tokens/openai_token_limit是assistant.json文件级设置，详情展示当前值并明确重启读取。协议选择有两项，改变协议时保存官方默认基址并清空模型；提示旧作用域Key不会发送到新服务，不自动删除旧Key。重选当前协议不重置自定义地址或模型。

删除Page::Agents、宿主发现、Row::Host/Option/ConnectOptions和原生Choice/SettingValue。使用TUI本地 `ChoiceKind::{Sending,ApiProtocol,ClearApiKey}` 加selected索引替代原生Choices；Sending仍调用原 `set_automatic_permission`，不得变成普通配置保存。新增普通编辑字段 `ApiBaseUrl/ApiModel`；Key不进入普通Field。清除Key用保留/清除两项确认，空秘密输入不当作清除。

把 `secret_mode: bool`干净替换为 `secret_target: Option<SecretTarget>`，`SecretTarget::{ObsPassword,AssistantApiKey}`，复用现有secret_draft和EditorInput。`submit`先分派秘密目标，失败保留秘密输入且不恢复公开发送，成功/取消恢复原Unicode人工草稿和光标；秘密粘贴在assistant_panel处理之前，仍受4096字节限制。更新所有焦点拦截、visible_input遮罩、draw_input标题、命令搜索/Shift-Enter/鼠标/本地event拦截。新直接命令 `/ai key`只能打开隐藏输入，不接收命令行明文参数；命令搜索与模型菜单复用同一入口。Key不进入unhandled_command、复制出来的面板文本或Journal。

保留 `/ai start`、`/ai visible`、`/ai stop`、`/pause`、`/review`、本场topic/range和已有Quiet/TextQa/Thanks预设，不新增第二套“模式”枚举。删除 `/ai reset` 与Row::Context/“重建上下文”；`visible`继续显式纳入当前可见消息但不重新处理已消费旧批。缺配置启动时打开Models，说明缺哪个字段，不发现/启动本机Agent。不再用report.session判定启动动画；处理中显示本地请求状态，等待、暂停、内容拒绝和致命失败必须区分。

联网开关仍在发送策略，默认关闭；详情说明只把被选中的本批完整公开问题正文发送到Exa，不发送私有资料/历史扩写查询；每轮最多一次、最多3条、可能限流。设置保存/切预设不启动推理、不授发送权；感谢复用submit_routing_template，不另建模型感谢路径。

#### 4.4 无隐式记忆损失的请求装配

保留 `round_input` 和 `workspace::load`，不增加数据库/向量库/摘要Agent：

- 新模型请求system来自workspace::CONTRACT；从JSON删除重复contract.instruction，保留schema/target_rule。应用预算严格为 `system.as_bytes().len() + prompt.text.as_bytes().len() <= 24 * 1024`，其中text是已序列化的本轮JSON，不把JSON wire层转义再算入该24KiB；HTTP完整序列化body另做48KiB检查，任一阶段超限都不得发送超限请求。沿用先删除最旧近期参考再补历史的优先级，不截断当前问题凑预算。
- 每次请求都含完整operator_context、名字/preferences、人设、本场topic、live_context.audio_available=false、当前问题、近期本场对话、actual_delivery_results、房间公开信息和相关历史；删除ACP的operator_context_unchanged去重优化。
- 保持资料8KiB、新批最多8条/12KiB、近期最多12条/4KiB、历史合计最多4KiB（观众最多3KiB、每UID最多3条、全批最多8条、每条240字符）的现有边界；按房间和准确UID查询，只取本批前的记录。
- actual_delivery_results原样保留状态、retry_of、confirmed_segments、unconfirmed_segments和duplicate_risk。已明确confirmed的分段无论总体状态是Confirmed还是部分失败的Rejected，都是真实已发送事实；Uncertain/未确认段不能说已送达。待审/丢弃候选不追加成assistant发言，不新增第二份回复事实结构。
- 新增/修订 `workspace::CONTRACT` 的业务约束：优先明确问助手、资料型问题与相关连续追问；资料不足不编，依赖未知口播、主播私人立场/承诺及应由主播判断的争议不代答；必要时返回空候选，原消息仍在主弹幕流和归档，不新增主播待办队列。不保证提示词可机械保证模型判断正确。
- 保留Journal/SQLite及workspace内容，不迁至供应商会话。无UID/无历史命中才是空历史，当前库损坏/读取失败不伪装空记忆；旧归档坏片段沿现有隔离维修边界。明确范围：history::import_journal当前只索引SessionStarted/EventReceived，并不导入AutoReply；跨进程保留的是既有观众/场次历史与原Journal回复记录，不新增“所有旧助手发言进入历史检索”的承诺。只存在原生宿主且未归档的聊天状态不声称可迁移。更新新建workspace默认SYSTEM的ACP说明，不覆写已有用户正文。

#### 4.5 完成切换时移除旧运行链路及所有消费者

LSP已核对Settings/Host/Source/NativePreferences、ACP Handle/candidates和Runner公共方法引用。实施时重读定义并重新用references定位，以下是切换的完整消费者集合：

- `Runner::save`签名消费者：`terminal/assistant.rs`的start_assistant/apply_assistant_row/save_assistant_editor和相关tests；`terminal/agent.rs::set_automatic_permission`；`runner/tests.rs`；`workspace_config.rs`内tests。`Runner::start`消费者：`terminal/assistant.rs::start_assistant`、`terminal/mod.rs`启动续用分支、`terminal/agent_tests.rs`、`runner/tests.rs`；删除Runner自身原生restart调用。
- `TerminalApp::start_assistant`消费者：`assistant.rs::apply_assistant_row`的Run/Visible（删除Context），`terminal/mod.rs::command`的start/visible（删除reset）。`SessionJournal::assistant_stopped`仅由`terminal/agent.rs::advance_bridge`与`persistence.rs`内测试调用。
- 保留文件中原生类型消费者全部迁移：`runner/mod.rs/settings.rs/tests.rs`、`terminal/assistant.rs/mod.rs/agent.rs/agent_tests.rs`、`workspace_config.rs`。模型相关新类型集中在上述新模块，不保留兼容重导出/原生会话ID占位值。
- 删除 `src/runner/acp.rs`、`src/runner/hosts.rs`及其`hosts/`原生适配器/JS/Pi过滤器；删除仅准备Pi的 `src/setup.rs`、`config::Command::Setup/SetupAdapter`、`main.rs` setup分流、`lib.rs` setup导出和 `agent-client-protocol`依赖。不要顺带删仍有调用者的libc或用户安装的适配器目录。
- 删除 `tests/fixtures/acp_agent.py/acp_search.py`；将 `runner/search_tests.rs`改为应用搜索回归而不是删除业务覆盖。移植搜索/HTTP/PTY断言后退役 `script/verify_pi.py/verify_pi_search.mjs/verify-native-text-hosts.py/check_amp_native_policy.py`。混合Runner/terminal测试改loopback HTTP fixture；原生握手/配置ACK/进程轮换断言退休，业务取消/过期/权限/记忆场景保留。仅锁定文案、源码或实现细节的旧断言删除，不换新字符串重钉。
- 保留 `bridge::{Request,apply_owned_reply,apply_repair}`、CLI `agent status/messages/report/reply/result`、stdio MCP、`script/agent_config.py`及外部资料维护用途。`status.driver=runner|external`、同caller的runner_active互斥和其他已授权独立caller的行为不变；不把对外MCP也当作ACP删掉。
- 本轮smoke通过后，同步现有README.md的未发布/当前操作说明、docs/terminal-feature-matrix.md的当前能力及docs/agent-onboarding.md的当前接入与本轮验收；历史版本/历史证据明确保留为历史，不重写成新API已通过。网站对应修改 `website/src/content/docs/guide.md` 的设置/密钥/首次使用/排错/命令表，替换Pi流程和/ai reset；更新 `website/src/pages/index.astro` 的助手说明，在既有 `website/src/content/docs/changelog.md` 未发布区记录本次破坏性迁移。写清费用、明文私有Key、重新授权、受限公开问题搜索和记忆边界；不承诺未测Windows/Linux UI。代码去依赖时更新Cargo.lock，并从THIRD_PARTY_NOTICES.md移除不再分发的ACP SDK段落，保留其他声明。外部CLI/MCP文档继续存在，不误删。只改本地内容，不提交/推送/发布网站；不创建另一份用户手册。

## Critical files & anchors

实现前重读，行号仅是本轮定位提示：

1. `src/terminal/agent.rs::advance_bridge`（约568）：生产tick、轮次落盘和发送队列汇合，所有新候选必须经过此路径；fatal持久撤续用不能破坏已确认送达。
2. `src/terminal/agent.rs::restore_automatic`及`src/terminal/mod.rs`启动续用分支（约1530）：模型配置/Key验证不能替代同房间、同UID、账号generation和真实B站身份核验。
3. `src/bridge/mod.rs::consume_routing_models/recent_conversation_before/recent_results/apply_owned_reply`：单飞lease、记忆事实及唯一发送准入；不要重新建队列。
4. `src/workspace_config.rs::assistant/check_file/private_file`：非敏感白名单与私有文件检查模式，不将配置观察当权威设置。
5. `script/agent_smoke.py::Surface/execute_command`和顶层PTY流程：保留真实终端证据框架；当前脚本仍断言旧五类设置、/ai workspace和ACP，不能直接引用旧通过记录。

## Verification

以下是执行阶段要求；计划阶段未运行测试、推理、搜索或直播实例。工作目录固定仓库根；Rust≥1.89，PTY脚本需Python≥3.11。所有自动验证使用tempdir、假Key和127.0.0.1，不读取用户原生认证。

### A. 模块与生产业务回归

新建模块内分别注册 `#[cfg(test)] mod tests`；共享test_support仅cfg(test)。按步骤运行 `cargo test --locked runner::model::tests::`、`cargo test --locked runner::api::tests::`、`cargo test --locked runner::credentials::tests::`。步骤2新搜索用例先放 `search.rs::tests`并运行 `cargo test --locked runner::search::tests::`；步骤4把旧search_tests.rs改为该子模块（`#[path="search_tests.rs"] mod tests;`）并合并新用例，不留重复模块。切换后运行 `cargo test --locked runner::tests::`、`cargo test --locked terminal::`；每个过滤器必须实际匹配测试，且整个crate的cfg(test)代码始终须可编译。原业务断言迁移，不加只证明字段转发、接线或文案相等的测试。

HTTP fixture复用 `bilibili/client.rs::account_server` 的TcpListener/有界读HTTP模式和 `runner/tests.rs` 的oneshot屏障，不复用B站业务handler。把共享测试辅助放在新 `src/runner/test_support.rs`，只在cfg(test)加载：实际loopback HTTP服务、两协议正式envelope、记录请求、可控延迟/断流/状态码，不用全局环境变量或真实时间长睡眠来切换案例。测试deadline由Prompt参数缩短，不改生产超时常量。

必过行为：

- 两协议分别：新问题→候选→Bridge待审；连续两轮请求都包含完整资料与本轮正确目标；无新消息/仅保存设置→零HTTP。HTTP认证字段只到目标loopback，不进入prompt/诊断/归档。
- 完整envelope的错误JSON、旧round、未知/重复/合法非法混合ID、非法正文→本批零候选零发送；同一worker随后正常问题仍可待审/在已有许可下入队，旧候选和许可未被清掉。外层断流/错协议/超限体/401/429/5xx→明确暂停撤权，不重试旧批；手工输入仍可用。
- 同时注入多条问题只产生一条在途模型链；零候选/无正文后按原空轮合批；点名只绕合批等待，不绕单飞、背压或授权。
- 取消排队请求→零HTTP；取消在途/结果已排队但未被tick消费/换场/资料改变→迟到输出零入队；租约释放，显式重新启动可处理新题，不重新处理旧题。
- 两协议搜索：第一次web_search→一次真实loopback MCP搜索→第二次候选；最多一次search/two model calls。关闭/无合格公开问题/多个tool/未知tool/旧message_id/额外query字段均零搜索；再次tool无第三模型请求。JSON/SSE跨chunk及多行data、429、缩短的20秒测试deadline、取消、错ID、超限体、Unavailable强制待审均覆盖。设置/历史植入私有sentinel，验证实际Exa请求仅含选中当前正文，不含sentinel、UID或Key；长于240字或控制字符正文不进工具enum。生产URL仅cfg(test)可注入。
- 迁移最小fixture包含每种旧格式各一份非默认资料/感谢/词表/技能/人设组合；format2额外topic；format3带OMP profile/NativePrompt及多条native_preferences；vs_code走legacy；再覆盖缺省、未知字段/嵌套字段/版本、错类型、32KiB边界、损坏与备份冲突。无版本/2/3→4保留规定业务字段，关闭automatic/resume/web_search、不继承原生模型/认证；同范围remembered_automatic实文件存在时仍不自动启动/授权。加载原字节不变，首次保存备份逐字一致；失败旧文件不变。format4重开不丢已合法保存的新配置与授权偏好。
- Key：协议/规范化地址不匹配不发旧Key；默认端口/尾斜杠/host大小写/IPv6按唯一规范化函数判定，错误URL不是None。损坏、符号链接、Unix宽权限或可写父目录拒绝；写失败保旧值；换Key提示会替换这唯一记录。真实Runner提交入口在运行/flight/stopping/已有发送权及TUI待续用时拒绝，暂停后成功。外部删除不假称已取消远端请求，下次启动必须重新读取且拒绝缺Key；旧handle不复活。
- 记忆：同场追问、同UID改昵称关联/同名异UID不混、跨房间不混、未来记录排除、两协议预算不扩张。消费真实状态转换：部分confirmed+内容拒绝→最多一次repair→下一轮，确认段仍为已发送事实、retry_of关联、未确认段不被伪造成成功且确认段不重发。待审/丢弃候选不成为已说过的话。
- 跨进程用新增 `runner::tests::memory_roundtrip_across_processes`：复用windows_tests的current_exe + `--exact runner::tests::memory_roundtrip_across_processes --nocapture`模式。无DANMU_TEST_MEMORY_PHASE的父测试创建tempdir，依次以Command.env仅向子进程传root和writer/reader-openai/reader-anthropic；任何未知phase失败，不在父进程set_var。writer写带稳定UID/房间的过去合成事件与confirmed回复Journal并正常关库；两个reader各自新建场次/Runner/History，使用同一目录及真实loopback协议fixture读取下一轮请求，断言旧UID消息命中、无旧topic/会话/许可，原Journal回复记录仍在。不把AutoReply声称已被history索引。子进程断言失败输出传给父测试，每个exit status须成功；不新建生产命令或放宽local新空目录保护。
- 保留原账户、同房间UID恢复、独立号不回退、审核版本冻结、Repeat不发送、未知送达不补发、一次repair、分段确认、真实写盘错误不吞、人工草稿保留、外部CLI/MCP并发与caller互斥回归。

### B. 实际TUI闭环，不用测试代替运行证明

改造现有 `script/agent_smoke.py`：复用其PTY/VT/HTML/cells、CLI/MCP和LocalTransport，删除退休菜单/ACP脚本假设；引入Python标准库ThreadingHTTPServer，迁用`verify_pi.py`的HTTP夹具结构，但输出非流式两协议envelope、不依赖Pi/Node。新增 `--protocol openai_chat|anthropic_messages`（默认openai_chat）；脚本通过真实模型设置页面和隐藏Key输入完成配置，不能直接把候选塞Bridge冒充模型链路。

HTTP driver仅监听127.0.0.1随机端口，处理 `/v1/chat/completions` 或 `/v1/messages`；其他route/协议失败。父脚本用queue.Queue逐个预置Scenario，在注入/event前安排expectation；handler按round_input真实结构提取轮次/当前目标生成正式envelope，不硬编码ID。请求headers只在内存校验假Key，落盘仅布尔认证检查及脱敏payload。场景推进用threading.Event与PTY pump的有界条件等待，不靠固定sleep猜合批时机：

| Scenario | 驱动与观察 |
|---|---|
| normal / followup | 当前目标输出短候选；第二次请求必须包含前问与已确认回复，server核验后才作答。 |
| bad-json / next-normal | 第一次完整envelope承载非法候选JSON；看到本批rejected后再注入新问题，下一正常候选无需重启。 |
| uncertain | 正常候选；审核前执行/local uncertain，批准后断言唯一未知送达，后续等待无额外发送/改写。 |
| hold / late-answer | handler设置received事件后阻塞release；父脚本先观察请求到达，再Ctrl-P并确认暂停，最后release返回迟到候选；Bridge及Journal不得出现其入队/送达，草稿仍在。换场隔离用新场次再做一次hold。 |
| search-unavailable | 首次用本批可搜索message_id请求web_search；真实二进制公共搜索被sandbox拒绝；第二次请求须携带失败tool result且无tools，再返回候选；诊断Unavailable、候选强制待审，之后零第三次调用。 |

菜单入口分别通过真实/settings导航、Ctrl-O命令搜索和SGR鼠标点击验证；鼠标坐标从当前VT可见“模型”行动态求得，不固定屏幕坐标。`/ai key`打开后才发假Key，粘贴使用ESC[200~…ESC[201~；Enter仅保存、Esc恢复原草稿，审核Shift-Enter沿现脚本kitty序列。先观察实际焦点/遮罩再送下一动作，不让密码粘贴落入设置或公开输入。

在macOS复用已读verify_pi网络sandbox：只允许loopback，清除继承的Key/Token/代理变量并使用隔离HOME；没有sandbox-exec时不静默放开公网，改在具备deny-egress条件的隔离环境运行同一脚本。脚本只结束自己启动的TUI/MCP/HTTP进程，不触碰用户终端。

每次使用全新证据目录，release构建后运行：

```sh
PROOF=$(mktemp -d /tmp/danmu-api-proof.XXXXXX)
python3 -B script/agent_smoke.py --binary "$PWD/target/release/danmu" --protocol openai_chat --output "$PROOF/openai"
python3 -B script/agent_smoke.py --binary "$PWD/target/release/danmu" --protocol anthropic_messages --output "$PROOF/anthropic"
```

每条脚本保存ANSI/VT/HTML/cells、results.json、脱敏请求与local-deliveries.jsonl；这些证据及Journal不得含假Key原文。隔离凭据文件本身按设计含假Key，须从对外证据打包排除；不以这个必要存储例外掩盖画面/日志泄露。实际操作：

1. 四类设置、/ai model、命令搜索、鼠标入口一致，保存无HTTP。/ai key输入/粘贴被遮罩，Enter不发送，Esc恢复Unicode草稿；人为制造隔离Key文件保存失败时保留秘密输入、不落回公开发送。按上述证据范围检查无Key泄露。
2. `/ai start`后无消息零HTTP；`/event 42 如何理解增量游标？`→待审；审核完整查看后Shift-Enter批准→恰好一次LocalTransport confirmed。下一条`/event 42 那它和时间戳有什么区别？`请求携带前一问及已确认回复，而不是仅本条文本。
3. 坏候选→下一条正常候选无需重启；`/local uncertain`后未知送达不重发；Ctrl-P取消在途，输入草稿仍在；显式重新启动、`/session end`/`/session new`后旧轮不跨场入队。
4. 二进制在网络sandbox中尝试搜索时必须可见降级为Unavailable并强制待审，不发生第二次搜索；搜索成功的协议闭环由A项真实loopback客户端/Runner测试证明，不把被沙箱阻断称作成功联网。
5. CLI/MCP仍读取同一实例；runner持有时同caller写入被拒，其他独立caller保留原授权边界。/quit返回0、instance.json移除、自有进程自然清退。目视检查两协议设置、秘密输入、待审、拒绝/失败和诊断页面，不能只看退出码。

### C. 完整门禁与本机安装证明

```sh
RUST_TEST_THREADS=1 ./script/verify.sh
./script/install_cli.sh
shasum -a 256 target/release/danmu "$HOME/.local/bin/danmu"
```

两个哈希必须一致；安装路径再次执行B项两协议smoke并换全新证据目录。只报告本轮结果，不复用历史测试数/哈希；LocalTransport不称真实B站送达，fake API不称真实模型效果验收。用户自行退出并重启旧TUI，不关闭其直播/OBS/终端。

同步网站文档后，Node.js 24下在website运行 `npm run build`（现有脚本包含astro check与astro build）；用 `npm run dev -- --host 127.0.0.1` 启动本地页，browser.open检查首页助手说明、/guide/新配置及迁移步骤、/changelog/未发布记录，保存实际页面截图，检查内部链接后关闭自有tab/server。这里只验证文档呈现，不部署Cloudflare、不修改远端。最后去掉本轮临时脚本/调试开关；保留的是正式回归与可重复smoke，不把证据目录提交仓库。

## Assumptions & contingencies

- 支持一种当前连接、两种明确协议，不维护ACP高级入口；文件级OpenAI token字段兼容选项已定义，不做自动协议/模型探测和错误后回退。没有用户选定的真实模型时不预填过时模型名。
- 本地私有Key是用户权限保护的明文存储，不是系统保险箱；如用户另行要求Keychain/跨平台加密保险箱，应先改计划，不能暗中新增平台凭据系统。
- 既有Exa服务不可用时执行Unavailable+强制待审，不能因此恢复Pi或接入新付费服务。OpenAI兼容服务不支持标准tools时，用户关闭搜索或换合规服务；程序显示明确错误，不伪装搜索可用。
- 真实模型/真实联网效果仅在用户明确提供验证专用Key、模型并授权调用后补做合成场景验证；默认完成loopback协议、业务与TUI闭环，不读取或消耗用户已有原生账号额度。
- 若执行前源码已有用户改动，按当前符号语义移植本计划，不回退/覆盖用户改动；若仅行号漂移，用LSP定位。遇到未预期协议或权限变化时按上面的拒绝/暂停规则处理，不增加未授权兼容旁路。
