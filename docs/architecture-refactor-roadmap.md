# Haven 架构降复杂度重构路线图

> 状态：执行中
> 制定日期：2026-09-23
> 适用范围：Agent/Session、Memory、Tools、LLM、App IPC 与 UI
> 原则：先收敛权威和所有权，再收敛接口和文件；允许删除旧设计，不保留无期限兼容层。

## 1. 目标

本路线图不是继续机械拆大文件，而是减少同一个业务事实在多个运行时容器、事件形态和投影路径中重复维护。完成后应满足：

1. 一个 session 的热运行态只有 `SessionActor::SessionState` 一个 owner。
2. `session_events` 是恢复和回滚的唯一 durable authority；`messages`、`session_steps`、`llm_usage` 和 UI 事件都是明确的投影或 live 通道。
3. 工具、模型、记忆、任务和配置的跨层调用通过窄接口表达意图，不再通过总管对象或 raw `Database` 互相穿透。
4. 每一个稳定业务 DTO、状态枚举、策略和事件只有一个权威定义；动态 JSON 只停留在 provider/MCP/Skill 的明确扩展边界。
5. 每一个大阶段都能独立编译、测试、回滚和审查；破坏性 schema/config/IPC 变化都带有重置说明。

## 2. 已确认的基线

已有 ADR 0196、0202、0204、0205、0206、0207、0208、0209、0210、0211、0212、0213、0214 已经定义了主要目标边界。当前实现已经完成：

- session event replay、投影事务、durable UI sequence、usage 增量投影；
- `react_state` / `react_checkpoints` 删除后的新 schema 方向；
- `ToolsManager` 的执行入口、`ToolServices`、`OperationRegistry`、`OperationSpec` 和 `PlatformRuntime` 初步收口；
- UI 单一 session reducer、事件 handler 分层和工具结果 renderer 注册表。

当前仍可观测到的结构性差距：

- `resume.rs` 负责组装 `RunReplay` 并向 actor 发起 run；`ReActState` 已在 actor 的 run 路径中创建，但仍是 run-local projection，尚未成为 `SessionState` 的字段；
- actor mailbox 仍包含 messaging poll 等 actor-local 命令；usage 由 `UsageRuntime` 所有，stream identity 与 token estimate 已随单次 `ReActState` 收口，不再是跨 session engine sidecar；
- resume 仍需协调 RAM 队列、ingress cursor、undelivered scan、partial promotion 和 interaction replay；
- Agent/Tools/App 仍有较宽的 runtime facade 和 `Arc<Database>` 传播；
- ActionService 的内部领域输出、usage 写入参数和 LLM router 请求入口仍存在重复包装；
- `update_settings` 仍手动编排多个 runtime 的分阶段更新；model 的完整 prepare/publish 已归 coordinator，settings 阶段化应用待单独收口；
- UI 页面和 reducer 已有边界，但编排代码仍过重，IPC 类型仍是 Rust/TS 双份维护。

### 2.1 已完成的降复杂度切片（截至 2026-09-25）

以下切片已经独立提交并通过对应门禁；它们是阶段目标的增量落地，不代表后续阶段可以跳过契约收口：

- `72b033e`：一次 session run 的驱动迁入 `SessionActor`，并补 actor 等待期间的 panic/运行态保护。
- `672e746`、`9038326`：删除 actor 内重复的 ReAct run 状态包装，收平只剩单一消息运行态的容器。
- `fbbb60f`、`426769c`、`48c8d1c`：分别移除 stream identity、run budget、token estimate 的 mailbox 往返，并把 LLM 请求策略快照固定在一次请求内（ADR 0219–0222）。
- `7ec0965`：usage 累计与持久化由 `UsageRuntime` 所有，actor 不再承载 usage mailbox（ADR 0223）。
- `5875b80`、`5abb9c3`：Agent 对工具目录和 observation 改用窄 port，保留授权/执行边界（ADR 0224–0225）。
- `00f5df5`：删除 AgentLayer 的 context limits 镜像，配置读取委托给 ReActEngine（ADR 0226）。
- `ce26c21`、`a9b03a4`、`6f86362`：Action board 输出、continue 截断和 usage 写路径分别完成 typed/store ownership 收口（ADR 0215、0217–0218）。
- `950e0fb`：UI 可见消息改为从 session reducer 纯派生，页面不再维护第二份消息数组，并补 selector 单测。
- `fce6a90`、`562ba80`：删除 LLM router 的仓库内 chat 转发别名；定时任务以 `ActionEntry.session_id` 和 `ScheduledActionEntry.due_at` 为唯一进程内事实（ADR 0228–0229）。
- `fc2662b`、`322c231`：ContextLimits 刷新同时覆盖 Router 且在 Router 成功后应用其他消费者；工具 Usage 批量写入归属 `UsageRuntime`（ADR 0230–0231）。
- `2b9844d`：UI 页面直接派生 active session 的 token stats/LLM usage，删除 usage 镜像与同步 effect；ReActEngine 剩余 Database 依赖经审查后暂缓统一封装，因为读取、事件权威写入、transcript 投影和记忆副作用仍是不同事务边界。
- `95189fa`、`d7dd2e3`：MemoryEmbeddingIndex 成为 embedding vector-space identity 的唯一解析者；ActionService 接管 App action 历史读取，命令不再直接读取 raw Database（ADR 0232；ADR 0215 边界补充）。
- `cfa080b`：会话恢复、初始输入和清理路径通过 `ManagedAssetLeasePort` 管理媒体资产租约，工具 session overlay 注销保持独立（ADR 0233）。
- Phase 6 首个请求对象切片：`CompleteRequest` 成为普通与 tools 完整请求的唯一 Router 入口；删除 Router chat 转发 API 并迁移仓库内调用点（ADR 0234）。
- `3bc807d`：回滚目标先校验，SessionStore 在事务内解析消息/分支 projection boundary，过期 transcript cursor fail closed（补充 ADR 0207）。
- `0321a61`：settings/model 共用配置应用串行边界，Router 与媒体依赖从同一 `ConfigSnapshot` 预构建，准备失败不修改 live runtime（ADR 0235）。
- `update_settings` 的旧 hotkey 读取与 Settings 合并现由同一次 `ConfigService::edit` 完成；该 edit 返回运行时应用所用的旧 hotkey、snapshot 和 change。移除外层 snapshot 读取及 permissions 复制，no-op 仍在 runtime apply 前快返；`AppConfig::apply_settings` 继续保护 permissions、`encrypt_sensitive` 和表单不管理字段，运行时顺序与半失败语义不变（补充 ADR 0235）。完整 RuntimeConfigCoordinator 仍待后续阶段设计。
- 当前切片：settings 与 model 均由 `RuntimeConfigApplyPlan` 判定 Router target；model 的实际 `ConfigService::edit` 变更只产生 `ConfigDomain::Llm`，plan 映射到 `LlmRouter`，no-op 不重建。两个入口仍持有同一个 `ApplicationRuntime::config_apply_gate`；提交、预构建、发布及副作用顺序未变（ADR 0235）。
- `9f45e46`：Action IPC wire DTO、状态值和可选字段加入静态漂移检查；不引入通用 codegen，性能诊断命令目录同步修正。
- `40563f9`：删除聊天页重复的 active-session 错误清理 effect，错误迁移统一归 reducer（UI 706 项测试）。
- `10b72fc`：后台 action 持久层终态写入改为 first-wins CAS，completion outbox 只记录胜出事务（ADR 0236）。本轮按 ADR 0248 收口 ActionService 终态仲裁：只有 CAS 成功者应用内存状态并发布；CAS 输家按 action row 对齐但不通知；持久化错误保留候选并退避重试；session cleanup 在提交前保留 entry；late attach 先提交 owner/outbox 更新，持久模式不重复发 transient completion。完成/取消竞态、CAS 丢失、写入错误/重试、重复完成、cleanup 与 late attach 均有回归测试，无 schema/API 变更。
- `9b73c5e`：定时任务授权请求与 live authorization engine 调用收口到 SessionSupervisor 窄方法，确认/receipt/execute_gated 顺序不变（ADR 0237）。
- `a2ddd11`：resume 的 ingress-cursor-after 与 unanchored-user-window 查询归属 SessionStore，保持两个恢复边界分离（ADR 0238）。
- `18507a3`：删除已无调用者的 LLM/Tools/Common facade API，保留仍有语义或生产调用的 helper（ADR 0239）。
- `a7a1a1d`：MCP/Skill/builtin session overlay 的恢复与清理通过 SessionToolOverlayPort 收口，保持 asset lease/live registration 分离（ADR 0240）。
- `3cf3a47`：聚合 stream 主入口用借用式 StreamRequest 承载请求数据，cancel/hooks 继续独立（ADR 0241）。
- 当前切片：embedding 与 health_check 主入口分别改用拥有数据的 `EmbeddingRequest`、`HealthCheckRequest`；embed 空批次、路由、重试、permit、usage、健康状态与连接诊断语义保持不变（ADR 0242）。
- `SessionStore::truncate_projection_after_latest_committed_recovery` 在单一写事务内读取全历史最新 recovery marker、验证 phase、解析 active branch cutoff、删除 projection、追加 usage 补偿并重建 session usage；Agent continue 仅调用该操作，保留生命周期与提交后发布顺序（ADR 0243）。
- 当前切片：Tools 将 `RuntimeCapabilities.web_search` 从展示字符串改为 `WebSearchAvailability`，provider/MCP 能力判定与优先级留在 Tools，Agent 映射回逐字相同的三种 prompt 值（ADR 0244）。
- 当前切片：历史错误原因缓存并入 `SessionReducer`，`sessionErrorStore` 函数保留为兼容委托；busy 清除时机、历史页 fallback 和删除/清空列表生命周期不变（ADR 0245）。
- 当前切片：LLM Router 的 native transcription、complete、embedding、raw stream 建流和 health check 共用请求结果投影；聚合 stream 的取消豁免与其他状态语义不变（ADR 0246）。
- 当前切片：`MemoryWorker` 的 FastChat 调用经注入的 `MemoryInferencePort`；Agent 层的事实抽取、维护与游标行为不变，Router 适配器独占请求类型和响应 DTO（ADR 0247）。
- `ActionService` 后台终态仲裁已实现（ADR 0248）：数据库提交后再发布，错误重试，CAS 输家静默对齐，cleanup 保留未提交条目；持久 late attach 使用 outbox 恢复，headless late attach 仍补发 owner 绑定后的 completion。
- 当前切片：SessionStore 增加按 ID 读取 session record 与全量 pending records 的 typed 方法；保持缺失返回、pending 状态过滤、`created_at DESC`、`limit=-1/offset=0` 及 dispatcher 安装/入队顺序（ADR 0249）。
- 当前 Phase 9 切片：移除无仓库调用的 `LlmRouter::chat_with_prompt_output_cap`；`CompleteRequest` 仍直接承载 `max_output_tokens`，其他有调用或有独立语义的 helper 保留（ADR 0250）。
- `acd11de`：PartialStore 的 checkpoint/promote/discard 改经共享的 SessionStore typed port；保留 per-session lock、generation、原子 promote 与 scratch projection 时序，不改变 schema（ADR 0251）。
- `a14f596`：标题生成与后台记忆推理迁移到 `PromptRequest`，删除旧三参数 `chat_with_prompt`；全仓代码调用为零，Router 只保留拥有数据的请求入口（ADR 0252）。
- `5aadd3c`：settings/model 的 Router prepare/publish 编排归 `RuntimeConfigCoordinator`，model 使用完整 prepare→publish，settings 保留原有分阶段副作用顺序；删除 commands 层重复 Router helper（ADR 0253）。
- 当前 Phase 5 切片：模型命令只提供 selector validation/slot mutation 闭包；`RuntimeConfigCoordinator` 统一持有 model 的 gate、durable edit、Router target 判定及完整 prepare→publish（ADR 0323）。`update_settings` 的 phase tracker 现由 `config_runtime` 持有，并为半失败日志记录 snapshot version、phase、Router publish 状态及 restart-required targets；保留 no-op 快返、原副作用顺序和 `Result<(), String>` renderer。此项是 failure observability/phase ownership，不提供 live runtime compensation 或 rollback；完整 settings coordinator 与补偿策略仍待决（ADR 0324）。
- `7775e11`：`TranscriptBatchWriter` 只依赖 `SessionStore`；阻塞调度、可选取消和缺少 session 行的兼容语义下沉到 typed transcript batch port，保留既有事务实现（Phase 3，ADR 0255）。
- 当前切片：Ingress/recovery 消息通过 `SessionStore::persist_session_message` 持久化；blocking 调度、可选取消和 message_id 幂等由 typed port 承接，移除唯一仓库调用的 `SessionSupervisor::db()` getter；partial discard 与 X12 assistant transcript 边界保持不变（Phase 3，ADR 0256）。
- 当前 Phase 3 切片：失败会话的 pending/running action-step 清理通过 `SessionStore` 调度到 blocking pool，继续调用既有 Database 操作；`unknown` 状态、observation、完成时间、session 范围及 Error 更新后、`SessionError` 发布前的调用顺序不变（ADR 0258）。此切片不涉及 ActionService 或 MemoryRuntime。
- 当前 Phase 3 小切片：`UsageRuntime` 的 usage seed、批量追加和 rollback epoch 补偿均通过 `SessionStore` 可取消端口执行，Agent 不再持有该 runtime 的 `Arc<Database>`；usage FIFO、X12 事件顺序和 cancellation 语义不变（ADR 0272）。
- 当前 Phase 3 小切片：memory trigger producer 的 session 存在性检查与 durable append 通过 `SessionStore` 可取消端口执行，ReAct hook 不再传递 `Arc<Database>`；payload、sequence、replay 和 best-effort 语义不变（ADR 0273）。
- 当前 Phase 3 小切片：thought step materialized projection 通过 `SessionStore` 调度，EventDispatcher 与 ReAct transcript 路径不再直接传递 `Arc<Database>`；消息内容权威、ID 关联和 X12 发布/修复语义不变（ADR 0274）。
- 当前 Phase 7/8 小切片：ActionService 的 agent-facing status/list 查询先产生 typed projection，`ActionsTool` 在过滤和 running 判断后才转换为既有 JSON；UI `ActionView`、completion outbox 与动态 `tool_args` 边界保持独立，字段、排序和等待提示兼容（ADR 0275）。
- 当前 Phase 1 小切片：token estimate 由单次 run 的 `ReActState` 独占；删除 `ReActEngine` 的跨 session cache、generation/revision/LRU 和 reset 转发，append 增量、非 append 失效与 compaction 重建语义不变（ADR 0276）。
- 当前 Phase 3 小切片：消息心跳标题缺失时由 `ContextSource` 通过 `SessionStore::session_title` 读取，删除该 context assembly 路径的 raw `Database` 持有；actor 游标/缓存和 inbox 轮询语义不变（ADR 0277）。
- 当前 Phase 3 小切片：7 个历史查询命令改经 `ApplicationRuntime` 注入的 `SessionStore` blocking-pool ports；原 Database 查询、cache、谓词、排序、页面默认 limit/offset、export JSON 和 resume 读取不变，dropping caller future 不会停止已启动的 blocking query（ADR 0279）。
- 当前 Phase 3 小切片：fresh-run prompt 的最近消息窗口通过 `SessionStore::conversation_window` 异步窄端口读取，仅跨边界传递 role/content；保留既有 limit、消息筛选与顺序、错误文本和 fresh-run/resume 分界，完整附件读取不变（ADR 0280）。
- 当前 Phase 3 小切片：`update_session_title` 经 `ApplicationRuntime` 注入的 `SessionStore` 异步端口写入；持久化成功后才更新 executor 并发布既有事件，底层继续调用原 Database 方法，future 被丢弃不保证中断已启动的 blocking write（ADR 0281）。
- 当前 Phase 3 小切片：App resume response 的 messages、steps、session usage、LLM usage 与 active domain events 经 `SessionStore::session_resume_projection` 在一个 blocking closure 中按既有顺序读取；App 继续解码 interaction events 并映射原 IPC DTO，不声明跨查询快照。当时两个 command 保留原有 session record 查询，现已由 ADR 0283 收口。
- 当前 Phase 3 小切片：`get_session_for_resume` 的按 ID 记录读取与 `get_last_conversation` 的最近会话选择均通过 `SessionStore` 异步端口执行；按 ID 查询保留 `None` 和精确 not-found 错误，最近会话复用 `list_sessions(1, 0)` 的 `created_at DESC`、limit/offset 与空结果语义。同步 Agent `session_record`、resume IPC 与消息/附件恢复均不变（ADR 0283）。
- 当前 Phase 3 小切片：`end_session` 的持久展示标题 fallback 经 `ApplicationRuntime` 注入的 `SessionStore` 异步端口读取，保留 executor title/input 优先级、持久 title/input_text 语义、查询失败 warning 降级及结束/通知顺序（ADR 0284）。
- 当前 Phase 3 小切片：`list_facts`、`add_fact`、`delete_fact` 通过 `ApplicationRuntime` 注入的 `MemoryFactStore` 执行；blocking 调度、source 列表选择和可见性过滤归 `haven-memory`，App 保留原 trim/空值/敏感值校验、tags 规范化、IPC 类型和错误日志。facts 不并入 `SessionStore`，recall/maintenance 不变（ADR 0285）。
- 当前 Phase 3 小切片：`DesktopNotifications` 的会话标题缓存 miss 通过 `ApplicationRuntime` 注入的同步 `SessionStore::session_record` 读取；保持缓存优先、持久 title/input_text/session_id fallback、查询失败 warning 与缓存写回，通知处理和同步调用模型不变，不新增重复 port（ADR 0286）。
- 当前 Phase 3 小切片：Tauri `RunEvent::Exit` 通过 `ApplicationRuntime` 注入的同步 `SessionStore::pause_running_sessions` 暂停运行会话；返回计数、日志分支、持久化状态转换、崩溃恢复和 `teardown_blocking` 顺序不变（ADR 0287）。
- 当前 Phase 3 小切片：Agent 后台标题生成上下文通过 `SessionSupervisor::session_store()` 的 typed port 在一个 blocking closure 内读取；缺失 session/已有标题短路、最多 10 条消息后 user 过滤与顺序、既有标题写端口及持久化→executor→事件顺序不变（ADR 0288）。
- 当前 Phase 3 小切片：action completion 与 peer inspect 在 executor miss 时通过 `SessionSupervisor::session_store().load_session_record()` 读取；保留 executor 优先级、action fallback 的缺失/错误折叠、peer not-found/error 传播及 status/title/terminal 映射，peer wait 轮询和 completion 行为不变（ADR 0289）。
- 当前 Phase 3 小切片：`SessionSupervisor`/`SessionActor` 的状态持久化通过 `SessionStore::update_session_status` 调度；保留三次重试、最终错误、持久化成功后才修改 actor 内存状态，以及 actor miss 的 `Completed` 写入顺序。交互事件读写现也经 SessionStore；其他仍需 raw `Database` 的路径不在本切片范围（ADR 0290、0294）。
- 当前 Phase 3 小切片：`SessionSupervisor::delete_session` 与 `clear_sessions_and_delete` 的 durable 写入通过 `SessionStore` 异步端口调度；保留单删先 quiesce/移除 actor 再删 row、全清先 quiesce/清内存再原子 clear 的顺序，以及 not-found、计数、Database cleanup 与 blocking write cancellation 语义（ADR 0291）。
- 当前 Phase 3 小切片：AgentLayer 的首条消息失败清理、peer 显式标题与 notification-safe fallback 标题写入改用已有 `SessionStore` 端口；保留原错误、warning、注册、executor 更新和 title event 顺序。SessionStore 只承接 blocking-pool 调度；AgentLayer 保留 raw `Database` 供 MemoryService、ReActEngine 等其他职责使用（ADR 0292）。
- 当前 Phase 3 小切片：terminal ingress fallback 删除刚持久化的 ghost user message 改经 `SessionStore::delete_message_by_id`；删除失败仍只记录 warning，后续 session-updated、actor 移除与 `Supplemented(None)` 行为不变（ADR 0293）。
- 当前 Phase 3/1 小切片：SessionStore 异步端口调度 active interaction domain event 的读取与追加；`SessionActor::spawn` 不再接收 raw `Database`，`SessionSupervisor` 仍保留其 Database 字段供 `tool_runner` action-step 持久化使用，但不再用它读取或追加 interaction event。interaction replay reducer 留在 Agent，AgentLayer、ReActEngine 等其他模块仍保留各自 raw DB 依赖。持久化成功后才更新 interaction 内存状态；无 schema/IPC 变化（ADR 0294）。
- 当前 Phase 3 小切片：`tool_runner` 的 pending action-step、ensure-and-start 与 ensure-and-finish 经由已有 `SessionStore` typed ports 调度；复合端口在同一个 blocking closure 内复用既有 Database 操作并保留调用顺序、bool、确认与终态语义。Agent 继续拥有 action-step policy/metadata，`SessionSupervisor` 删除 raw Database 字段；无 schema/IPC 变化（ADR 0295）。
- 当前 Phase 1 小切片：stream identity 由单次 run 的 `ReActState` 独占；删除 `ReActEngine` 的跨 session map、session key 和 `RunMsgIdGuard`，主请求、重试、partial 与最终 projection 继续共享同一消息 ID（ADR 0278）。
- `SessionSupervisor` 与 ingress 创建路径通过 `SessionStore::create_session` 创建 durable session row；生命周期闸门、首条消息顺序、actor 安装和 dispatch 仍由 Agent 持有，创建本身不追加事件（ADR 0260）。
- 当前 Phase 7 小切片：删除全仓无调用的 `MemoryWorker::recall_memory_query` 转发；recall 继续由 `MemoryService`/`MemoryRecallPort` 所有，不新增同义 `MemoryReader` trait（ADR 0254）。MemoryRuntime 已完成 cursor/replay/ordered trigger/live recovery 核心，并已由 AgentLayer 在 dispatcher 前完成启动准备；interval、pause hook trigger、compaction-summary extraction 与周期 maintenance 调度均已收口到 MemoryRuntime 的 durable producer/outbox/scheduler 边界（ADR 0264、0265、0266、0267）。
- 当前 Phase 7 小切片：`MemoryWorker` 普通事实抽取与 compaction-summary 抽取共用 durable outbox 的逐 job 指数退避；失败保留 marker，成功确认后才清理（ADR 0268）。不引入与 `ActionService` 混合的通用 Job 状态机。
- 当前 Phase 7 小切片：`MemoryWorker` durable outbox 增加明确的应用停机边界；取消只停止 live projection，不确认或删除 durable marker，由 `AgentLayer`/`ApplicationRuntime` 显式触发（ADR 0269）。
- 当前 Phase 3/7 小切片：MemoryStore 通过共享的 `MemoryService` owner 承接 MemoryWorker fact/summary outbox marker 的 enqueue、restore、ack 与 summary episode 读取；Worker 继续负责 live projection、inference 和逐 job 退避。marker ack 失败与取消均保留 durable marker；fact inference 算法及 maintenance/kv/embedding 路径不变（ADR 0301）。
- 当前 Phase 3 小切片：`MemoryTool` 的 search/list/remember/forget 与 keyword recall fallback 经 `MemoryFactStore` typed ports 执行；Tools 只保留参数、安全和输出适配，desktop `MemoryRecallPort` 仍优先并拥有 embedding-aware recall（ADR 0302）。
- 当前 Phase 3/7 小切片：`MemoryService` 的 prompt candidate 与 typed recall 查询通过 `MemoryRecallStore` 调度；Agent 保留 prompt 归一化、缓存、provider 调用与候选合并，Memory 保留 keyword/vector 过滤、可见 facts hydration 与完整 retrieve 边界（ADR 0304）。
- 当前 Phase 3/7 小切片：`ActionService` 的 completion outbox、action history、后台 action 和 scheduled action 所有 SQLite 调用通过 `ActionStore` typed ports；Memory 内部拥有 blocking 调度与终态/outbox 事务，Tools 继续拥有 board、CAS 结果仲裁、恢复和重试策略（ADR 0305）。
- 当前 Phase 3 小切片：Tools 的 `AdminContext` 只接收 `SessionStore` 与 `MemoryFactStore` capability；诊断列表/计数通过 SessionStore 异步 history ports，MemoryTool 的事实 store 由 app-binary 组合根创建并注入，不再由 builtin 从 raw Database 构造。unavailable、limit、排序、status 过滤、错误日志与 provider wire contract 保持不变（ADR 0306）。
- 当前 Phase 3/7 小切片：MemoryService 构造并共享 `MemoryFactStore` 给 MemoryWorker；`load_known_facts` 通过有界端口读取，blocking 调度、有效置信度顺序、敏感过滤和 limit 属于 Memory，Agent 保留既有 prompt 格式、sanitize 和错误降级（ADR 0307）。
- ADR 0308 切片完成时：专用 `MemoryFactExtractionStore` 承接普通 session fact extraction 的 transcript projections、节流时间戳、`fact_extraction.{session_id}` 用户消息游标及游标推进；MemoryWorker 保留窗口/LLM/事实写入策略。当时事实批量写入、summary episode cursor/共享节流及维护数据库路径尚未迁移；后续事实写入、确定性维护和 LLM maintenance 分别见 ADR 0309–0311。embedding catch-up 始终沿用 MemoryService 的 MemoryEmbeddingStore。
- ADR 0309 切片完成时：`MemoryFactStore::persist_inferred_batch` 接收 Agent 已解析、规范化和清洗的 typed writes，在一个 blocking closure/SQLite 事务内批量检查存在性并执行 upsert 与 source-ref 持久化；Agent 保留敏感/空值/置信度/长度/谓词/标签策略，整批失败回滚且保留 per-fact 错误上下文。当时 MemoryWorker 仍经 MemoryDatabase 执行 LLM maintenance 与 summary episode cursor/共享节流；LLM maintenance 后由 ADR 0311 迁移，summary KV 仍保留。
- 当前 Phase 3/7 小切片：确定性维护及 LLM maintenance persistence 经 `MemoryMaintenanceStore` 的独立 typed 操作执行。Worker 保留确定性步骤顺序、日志、计数、best-effort 继续和最终聚合错误；LLM 路径由 Agent 保留配置门禁、提示词、解析、候选过滤、提案 gate 与失败策略，store 只执行 typed 查询/写入和 blocking 调度。每条 predicate rewrite 独立调用，不合并事务。周期确定性维护传递 cancellation token。ADR 0310/0311 时剩余的 summary cursor/throttle 路径后由 ADR 0312 收口到 MemoryFactExtractionStore；embedding catch-up 仍经 MemoryService。
- 当前 Phase 5/6 小切片：LLM usage runtime input 的 `cache_accounting` 使用 `haven-common::CacheAccounting`；SQLite 与 IPC 仍在边界转换为既有字符串，外部文本恢复统一按 `Unknown` 处理（ADR 0270）。
- 当前 Phase 4 切片：`AgentLayer` 在 composition root 只取得一次 `ToolsManager`，共享给 prompt builder 与 `ToolsManagerToolCatalogAdapter`，并显式注入 `ReActEngine`；engine 不再从 executor 查找 catalog port。目录 snapshot、session ID、工具执行和 live authorization 语义不变（ADR 0257）。
- 当前 Phase 4 切片：ToolsManager façade 请求 crate-private `ToolRuntimeCoordinator` 构造并解析唯一的 `ToolCapabilitySnapshot`，同一个 platform generation 的媒体 operation catalog、prompt capability、TTS/STT 与录音 gate 都从该 snapshot 读取；搜索的 provider/MCP/unavailable 优先级也由它统一投影。每次读取从当前 `PlatformRuntime`、Router config 和 MCP index 重建。PlatformRuntime 替换、Router config 发布和 MCP tools/list 更新没有共同版本钟，因此暂不缓存；不引入第二套 capability mapping（ADR 0331，承接 ADR 0326）。ADR 0333 将 runtime composition、平台/config 更新顺序、MCP discovery config/index 与 catalog rebuild 移入 `ToolRuntimeCoordinator`。app-binary 的 Router 准备/config gate、`update_settings` 跨阶段编排、MCP 命令的持久化和连接动作、McpManager 连接/catalog-version owner，以及 `ApplicationRuntime` shutdown 顺序继续留在原边界。`ToolsManager` 仍保留执行/授权、session Skill/MCP overlay、asset lease、catalog projection 与能力请求 façade；这些职责仍可继续拆分。

当前阶段判断：阶段 1 的 mailbox/运行态收口已基本完成；本阶段新增 token estimate 与 stream identity 所有权收口，两者现在随 `ReActState` 单次 run 生命周期存在，不再由 `ReActEngine` 跨 session 持有（ADR 0276、0278）；阶段 2 已收口 rollback boundary、两类 resume 读取 port、session overlay 恢复边界，以及 continue 的 committed recovery marker 决策与 projection 截断事务，但全局恢复/事件重放和崩溃窗口仍待继续验证；阶段 3 已开始以 domain typed stores 替代局部 raw Database 读取，当前覆盖 session record、pending session、partial stream、transcript batch writer、ingress/recovery 消息写入、terminal ingress ghost message 清理、失败会话 action-step 清理与 tool-runner action-step 持久化、ReAct durable replay state 读取/transcript seed/单条追加/event-boundary cursor 检查及 compaction summary episode 写入、Agent usage runtime、memory trigger producer、thought projection、ContextSource 的 session title read、Agent 标题生成上下文、App 历史查询、fresh-run conversation window、App 会话标题写入、App resume read model、end_session 与桌面通知标题 fallback、退出时暂停运行会话、Agent/SessionActor 状态与交互事件持久化、会话删除与清空、AgentLayer 的首条消息失败清理和 peer 标题写入、resume media read model，以及 MemoryFactStore 的 App CRUD 和 MemoryWorker 有界 known-facts 查询、普通事实批量写入与 `MemoryMaintenanceStore` 确定性及 LLM 维护 persistence（ADR 0238、0249、0251、0255、0256、0258、0272、0273、0274、0277、0279、0280、0281、0282、0283、0284、0285、0286、0287、0288、0289、0290、0291、0292、0293、0294、0295、0296、0298、0299、0300、0301、0307、0309、0310、0311）；ReActEngine 与 MemoryWorker 的 durable Memory 写读现通过 MemoryStore，普通 facts 读取/批量写入与维护分别通过 MemoryFactStore/MemoryMaintenanceStore，Worker 继续拥有 live projection、inference、LLM 维护策略与退避，AgentLayer 已不再保留 raw Database 字段，但仍向既有 Memory 持久化 owner 注入 Database。ADR 0312 已完成 Memory store convergence：生产 MemoryWorker raw Database 已清零，summary cursor 和共享节流戳经 MemoryFactExtractionStore；MemoryService 只保留私有 backing Database 以构造 typed stores/index。后续仍需让 Agent 只提交 SessionCommitted。阶段 4 已完成目录/观察/资产租约/overlay/定时授权入口边界、runtime web-search typed capability、ReActEngine catalog port composition-root 显式注入、统一 `ToolCapabilitySnapshot` 构造/读取和 `ToolRuntimeCoordinator` composition/runtime/catalog ownership（ADR 0257、0326、0331、0333）；ToolsManager 剩余执行/授权、session overlay、asset lease、catalog projection 与能力请求 façade 拆分仍待后续阶段。阶段 5 的 model apply owner 已收口：`RuntimeConfigCoordinator` 负责 model mutation 闭包、gate、durable edit、Router target 与完整 prepare→publish（ADR 0323）；settings 的 phase ownership 与失败可观测性已完成，`update_settings` 仍独立编排各阶段，后续仍需单独处理完整 settings coordinator、补偿/rollback 策略及其他配置写入口（ADR 0324）；阶段 6 已完成 CompleteRequest、聚合 StreamRequest、EmbeddingRequest、HealthCheckRequest 与 PromptRequest 请求对象切片，并已删除旧 prompt wrapper；统一请求结果投影和 ModelDirectory 的 client/primary-route 目录提取已完成（ADR 0246、0316），CallExecutor、raw StreamExecutor 与 AggregatedStreamExecutor 拆分均已完成（ADR 0318、0327、0328）；RequestDescriptor/capability-call-purpose 语义全贯穿仍待实施；阶段 7 已完成 action 持久层 CAS/outbox、ActionService 终态仲裁、ActionService agent-facing typed projection、MemoryWorker FastChat 窄端口、MemoryRuntime cursor/replay/ordered trigger/live recovery 核心、AgentLayer composition/dispatcher readiness barrier、interval/pause durable producer、compaction-summary durable per-episode outbox、maintenance scheduler ownership，以及 durable marker 持久化边界的 MemoryStore 收口（ADR 0247、0259、0261、0262、0263、0264、0265、0266、0267、0275、0301）；Memory store convergence 已完成、生产 MemoryWorker raw Database 已清零（ADR 0312）；完整 Job 生命周期仍待迁移。阶段 8 已完成 Action DTO 漂移检查和 UI 错误/usage/message 派生收口，其他 IPC/UI 编排仍待收口；阶段 9 已删除三个无调用者 facade API，并移除两个由 CompleteRequest/PromptRequest 完整替代的无调用 Router wrapper，剩余工作以 profiling 和更大范围公共面审查为主。

阶段 3 增量校准（2026-09-24）：历史查询命令、App resume read model、两个 resume command 的 session record 读取、`end_session` 展示标题 fallback、桌面通知会话标题 fallback、退出时暂停运行会话与 Agent 标题生成上下文现纳入 SessionStore typed-port 覆盖范围（ADR 0279、0282、0283、0284、0286、0287、0288）；fresh-run window 与 App session title write 路径分别见 ADR 0280、0281；上段阶段汇总记录的是此前已完成的覆盖项。

阶段 3 小切片补充（2026-09-24）：action-completion status 与 peer-session inspection 在 executor miss 时改经既有 SessionStore record port 读取；错误降级/传播和 peer wait 轮询语义不变（ADR 0289）。

阶段 3 小切片补充（2026-09-24）：会话状态通过 SessionStore typed port 调度，Agent 保留原重试和持久化先于 actor 内存变更的语义；其他仍需 Database 的 actor 路径保持原样（ADR 0290）。

阶段 3 小切片补充（2026-09-24）：单会话删除与全量清空的 blocking Database 调度迁入 SessionStore；Agent 继续拥有 closing/lifecycle gate、run quiesce 与 actor/内存清理，Database 删除/事务、缓存、KV 和 embedding cleanup 保持原实现（ADR 0291）。

阶段 3 小切片补充（2026-09-24）：AgentLayer 的三处 session 写路径使用已有 SessionStore 异步端口；首条消息失败仍尽力删除 session 并返回原错误，peer 标题仅在 durable write 成功后更新内存，显式标题事件与 fallback 通知行为保持原顺序。SessionStore 负责 blocking 调度，AgentLayer 继续保留 raw Database 供其他职责使用（ADR 0292）。

阶段 3 小切片补充（2026-09-24）：terminal ingress fallback 的刚持久化用户消息删除改经 `SessionStore::delete_message_by_id`；Memory 端口仅复用既有 Database 删除并调度到 blocking pool，Agent 保留 warning 降级及 session-updated、remove-session、`Supplemented(None)` 顺序（ADR 0293）。

阶段 3/1 小切片补充（2026-09-24）：interaction domain event replay/append 改经 SessionStore 异步端口；SessionActor spawn 不再接收 raw Database，SessionSupervisor 保留该字段供 tool_runner action-step 持久化；Agent reducer 继续拥有解析与状态恢复策略，AgentLayer、ReActEngine 等模块的 raw Database 依赖不变（ADR 0294）。

阶段 3 小切片补充（2026-09-24）：tool_runner 的 action-step 写入通过 SessionStore 的三个 typed ports 调度；ensure/start 与 ensure/finish 各保持在单个 blocking closure 内，原 Database 调用顺序、confirmed/outcome 和 bool 语义不变。Agent 仍拥有 policy 与 metadata，SessionSupervisor 不再保留 raw Database 字段；无 schema/IPC 变化（ADR 0295）。

阶段 3 小切片补充（2026-09-24）：ReAct durable replay state 读取、transcript seed 与单条 transcript 追加通过 SessionStore blocking-pool ports；Agent 保留 record 解析、ReActState 投影与解析错误语义，单条追加缺失 session 仍返回 sequence `0`，校验失败不产生 durable/live event 副作用。compaction summary、branch/rollback/recovery 的其他路径不变；ReActEngine 因剩余路径仍使用 raw Database 而保留该字段（ADR 0296）。

阶段 3/2 小切片补充（2026-09-25）：rollback target 精确读取、`rollback_to` 整体事务（含 replacement transcript）与 continue recovery projection 截断改由 SessionStore 异步端口在 blocking pool 调度。Agent 保留 lifecycle cancel/join、replay 与边界策略、事件/branch trimming、tools restore、usage invalidation 和 status 更新；事务失败时成功后置步骤不运行。三条路径继续使用不可取消的 `run_blocking`，`rollback.rs` 不再自行调度 raw Database；AgentLayer、ReActEngine 其他职责的 raw Database 仍在，resume attachments、compaction summary、Tools/UI 不在本切片（ADR 0297）。

阶段 7 的 MemoryRuntime committed-event 消费设计已由 ADR 0259 采纳；Phase 7.1 已完成 SessionStore 独立 event cursor/有界 durable replay/生命周期清理（ADR 0261）、按序处理核心、启动时已有 cursor 回放、bounded live/replay recovery runner，以及 AgentLayer composition/dispatcher recovery readiness barrier（ADR 0262、0263）。interval、pause hook trigger、compact-summary episode extraction 与周期 maintenance 调度已经由 typed intent/atomic episode write + durable producer/outbox/runtime schedule 接管（ADR 0264、0265、0266、0267）。Agent prompt/recall 查询现由 `MemoryRecallStore` 收口（ADR 0304）；ActionService 的持久化接口现由 `ActionStore` 收口（ADR 0305）；MemoryWorker 其他 Database 路径仍待后续切片，本设计不改变 recall 或 rollback facts 语义。

补充：阶段 3 的 transcript batch writer 已在 `7775e11` 进一步只依赖 `SessionStore`（ADR 0255），Ingress/recovery 消息路径、失败会话 action-step 清理与生产会话创建也已通过 SessionStore typed port 收口（ADR 0256、0258、0260）；阶段 7 的无调用 recall 转发已在本轮删除（ADR 0254），compaction-summary extraction 已完成 per-episode durable marker/outbox（ADR 0266），周期 maintenance 的调度策略已归 MemoryRuntime（ADR 0267），Agent prompt/recall 查询已通过 MemoryRecallStore 收口（ADR 0304）。这些切片不改变 facts/recall/rollback 语义；完整 Job 生命周期与 MemoryWorker 其他 Database 路径仍未完成。

阶段 3 小切片补充（2026-09-25）：ReAct pause/continue/error 共用的只读 event-boundary cursor 检查经 `SessionStore` 异步端口调度，Memory 复用 `load_replay_state`，保留可选 cancellation 与原 `run_blocking_cancellable`/`run_blocking` 分支；Agent 保留 boundary 返回值、warning 与指标语义，无投影或事件写入。`ReActEngine.db` 仍由 compaction-summary episode 写入使用，并在 summary 达到既有长度阈值时写 extraction marker（ADR 0298）。

阶段 3 小切片补充（2026-09-25）：compaction summary episode 与 pending extraction marker 通过 MemoryStore 调度既有原子事务；ReActEngine 继续拥有 trim/empty/长度门槛和持久化成功后的 wake 顺序，MemoryWorker 继续拥有 extraction live outbox。ReActEngine 的持久化边界现只依赖 SessionStore + MemoryStore；AgentLayer 已删除直接持有的 raw Database 字段；MemoryService 和 MemoryWorker 其他既有 DB 路径不在该切片迁移（ADR 0299、0300）。

阶段 3/7 小切片补充（2026-09-25）：fact/summary durable outbox marker 的 enqueue、pending restore、条件 ack 与 summary episode read 经 MemoryService 共享的 MemoryStore 执行；MemoryWorker 继续负责 live projection、inference 和逐 job 退避。ack 失败时 durable marker 保留并重试，停机取消不提前确认。fact extraction 算法和其他维护路径保持不变（ADR 0301）。

阶段 3/7 小切片补充（2026-09-25）：MemoryWorker 的已知事实上下文通过 MemoryService 持有并注入的 MemoryFactStore 有界读取；Memory 层先执行可见性过滤，再保留有效置信度顺序并截断，Agent 的 prompt 行格式、subject 前缀、sanitize 和错误降级保持不变。其他 MemoryWorker Database 路径不迁移（ADR 0307）。

阶段 3/7 小切片补充（2026-09-25）：普通 session fact extraction 经 MemoryFactExtractionStore 读取消息/步骤投影、读取与写入节流时间戳、读取与推进用户消息 cursor；窗口构造、模型调用、事实持久化策略与取消后保留 durable outbox marker 的语义不变。该切片完成时事实批量候选校验/写入随后经 MemoryFactStore 完成，summary episode cursor/共享节流与维护路径仍未迁移；事实写入及确定性维护由 ADR 0309/0310 接续，LLM maintenance 由 ADR 0311 接续。此后 summary cursor/throttle 已由 ADR 0312 收口；当前生产 MemoryWorker 不再使用 raw Database，embedding catch-up 继续通过 MemoryService 的 MemoryEmbeddingStore。

阶段 3/2 小切片补充（2026-09-25）：resume 的初始消息 id、attachments、`media_inputs` 与 session 全量 attachments 由 `SessionStore::session_resume_media` 在一个 blocking closure 中读取；保留消息顺序、首个 user 选择、空媒体与错误映射。Agent 继续负责 canonical initial input、fresh-run/resume 分界和 managed asset lease/register，事件流仍是恢复 authority；不迁移 memory_index/MemoryWorker 其他 Database 路径（ADR 0300）。

## 3. 不变量与禁止事项

### 3.1 不变量

- session-local 可变运行态只能由 actor 任务修改；supervisor 只负责 registry、admission、生命周期和 handle。
- 一次 run 在 actor 任务内执行；只有在外部等待之间的 yield 点短暂借用 `&mut SessionState`。
- 工具/模型/数据库等待期间不能持有 `SessionState` 的可变借用；外部命令必须能在等待期间被处理。
- durable 写入顺序固定为：追加 session event、更新物化投影、事务提交、再发布 committed UI event。
- 工具的外部副作用在事务外执行，并以稳定的 tool call/action result identity 做幂等和迟到结果丢弃。
- 流式 chunk、实时工具输出、WebSearch、usage、请求准备阶段 MediaPlan 属于 ephemeral/live 通道，不占 durable event sequence。
- rollback 由 `SessionStore` 解析 durable sequence 和投影 cutoff；ReAct 不直接操作多套时钟。
- `ToolsManager` 只能是执行/目录/启动装配的窄 facade；进程服务通过显式 bundle 或 capability context 传递。
- 安全确认仍在 `AuthorizedExecutor` 之前由调用方完成，不能为了抽象方便移入工具 future。

### 3.2 明确不做

- 不再纯按行数拆 `react/`、`router.rs` 或 Svelte 页面；
- 不新增微服务、外部 event bus 或第二套 AppRuntime；
- 不重写 provider adapter wire mapping；
- 不把所有实时 chunk 变成 durable event；
- 不为 MCP、Skill、provider raw payload 强行设计静态业务 DTO；
- 不保留已无调用者的兼容 facade、旧 snapshot 或内容比对去重路径。

## 4. 分阶段执行计划

阶段必须按顺序推进；同一阶段内部可以把不重叠的只读审查或测试任务交给独立 Agent，但不能同时修改同一写集。

### 阶段 0：基线、契约和观测（先行）

目的：在破坏性迁移前建立可重复的行为安全网。

范围：`docs/`、Agent session/react 集成测试、Memory session store 测试、UI session/stream 测试、必要的诊断日志。

工作项：

- 增加一次完整链路测试：输入 → provider 请求 → tool call → observation → pause/ask → resume；
- 增加 actor 等待 provider 时仍可处理 submit/steer/cancel 的并发测试；
- 增加 usage、stream identity、token estimate 不经过 mailbox 往返的结构约束测试/审查脚本；
- 清点所有 `Arc<Database>`、`conn()`、`get_tools()`、`run_react_loop()` 和 session map 调用点，登记删除目标；
- 记录全量门禁基线和已知失败，不把已有失败伪装成重构回归。

退出条件：当前分支 clean；后端和 UI 基线门禁可重复；核心事件/恢复行为有可定位的失败测试。

### 阶段 1：SessionActor 成为唯一热运行态 owner（P0）

目的：落实 ADR 0214，不再让 `resume.rs`/`ReActEngine` 维护第二份 session-local 运行态。

目标结构：

```text
SessionSupervisor
  registry / admission / lifecycle / handles

SessionActor
  SessionState { queues, interaction, runtime, hot transcript, run }
  actor task { mailbox select + run future }

TurnEngine / ReAct loop
  pure-ish turn progression, only at yield points borrows SessionState

SessionStore
  event append + projection + committed publication boundary
```

工作项：

- 把一次 run 的启动、恢复和退出驱动迁入 actor task；
- 将热 transcript、canonical、branch points、retry nudge、cancel state 收进 `SessionRuntimeState`；
- 删除 `EnsureStreamId`、`RecordUsage`、`EstimateTokens`、`AppendTokenEstimate`、`ResetTokenEstimate` 等内部 mailbox 命令；
- 将 `next_run_id`、usage 累计、stream identity、token estimate 改为 actor-owned 函数调用；
- 将外部命令限制为 Submit、Steer、ResolveInteraction、Cancel、BackgroundResult、生命周期/快照/消息交互；
- 让 actor 在 provider/tool/timer await 期间 `select!` 外部 mailbox，并在 yield 点重新取得短生命周期 mutable borrow；
- 删除 `resume.rs` 中直接构造 `ReActState` 的生产路径，保留纯 replay/project helper；
- 保持 X12、confirm/ask、tool order、LoopExit、partial generation 和 committed UI sequence 语义不变。

主要文件：`crates/agent/src/session/actor.rs`、`dispatcher.rs`、`resume.rs`、`react/loop.rs`、`react/turn.rs`、`react/context.rs`、相关 integration tests。

验收：actor 运行期间 submit/steer/cancel 不阻塞；两个 session 的热 transcript 不共享；resume/rollback/compaction 后不存在旧 generation 命中；`cargo test --locked -p haven-agent` 通过。

回滚：单提交回滚；不改变数据库 schema、IPC 或用户数据。若迁移不完整，必须整体回退 run 所在任务、SessionState 字段和 mailbox 命令，不能保留半套。

### 阶段 2：恢复、回滚和事件/投影边界再收口（P0）

目的：删除 actor 迁移后残留的三条恢复旁路和调用方时钟知识。

工作项：

- `SessionStore::load_replay_state` 成为恢复所需 durable 聚合的唯一入口；
- resume 只按事件回放，并用明确的 ingress/recovery marker 处理尚未入事件流的已持久化用户输入；不做内容比对；
- partial 只负责 live 草稿，promote/丢弃规则和 generation 保护统一在 run-exit；
- `rollback_to` 对外收敛为 durable sequence/branch identity，projection cutoff 由 store 在事务内计算；
- 将迟到 `BackgroundResult` 按 action result identity 丢弃；工具副作用不回滚、不重复执行；
- 检查所有 UI event 是否由 committed row 发布，清除另一路 live/durable 镜像；
- 增加 crash-window、lag/replay、rollback epoch、compaction root replacement 回归测试。

主要文件：`crates/memory/src/repositories/session_events.rs`、`crates/agent/src/resume.rs`、`rollback.rs`、`resume_support.rs`、`event.rs`、UI session event handlers。

验收：恢复不读取快照/steps/messages 作为 ReAct 真源；事件、messages、steps、usage、UI 的 sequence/clock 关系有测试覆盖。

回滚：代码回退；若涉及 schema 版本，只能按 `docs/release-and-reset.md` 完整重置数据库，不运行部分迁移。

### 阶段 3：存储 domain ports 与 typed projection（P1）

目的：把过宽 `Database` facade 和稳定业务 JSON 接口收窄。

工作项：

- 在 `haven-memory` 内建立 `SessionStore`、`TranscriptStore`、`UsageStore`、`ActionStore` 的最小公开面；
- 逐步删除 Agent/Tools/App 的 `Arc<Database>` 传递与上层 `conn()` 使用；
- 统一 `persist_llm_call(session_id, LlmCallUsageInput)`；cache accounting 与 `llm_usage.call_kind` 运行时输入已完成 typed enum 收口（ADR 0270–0271），其他 call purpose 表达另行收敛；
- 将 ActionService 的 board/status/list 输出收敛为 `ActionView`、`ActionKind`、`ActionState`、`ActionOutput` 等 DTO；动态 tool args/MCP payload 仍保留 `Value`；
- 保留 projection/cache invalidation/事务细节在 Memory 内部；
- 为每个删除的旧 Database API 删除无调用测试和旧文档入口。

主要文件：`crates/memory/src/` repositories/database、`crates/agent/src/` usage/session、`crates/tools/src/action_service.rs`、`crates/app-binary/src/commands/`。

验收：上层不再取得 raw connection；稳定跨层返回值无默认 `serde_json::Value`；内存数据库测试覆盖事务、回滚和空结果。

### 阶段 4：Tools capability runtime 与 Agent ports（P1）

目的：让 Agent 依赖能力接口，而不是理解 ToolsManager 内部服务结构。

工作项：

- 基于现有 `ToolServices`、`OperationRegistry`、`AuthorizedExecutor`、`PlatformRuntime`，为一次执行引入 `ToolExecutionContext`；
- Agent 只依赖最小的 operation catalog/executor、authorization、memory/session 读取端口；
- 删除 Agent 中 `executor.get_tools()`、逐个 setter/bind 和服务 locator getter；
- 不为每个内部服务制造 trait；只有跨 crate、需要替换/测试的能力才进入 ports；
- 将运行时 snapshot 整体替换，避免 `RwLock<Option<Arc<_>>>` 半初始化状态；
- 保留交互式确认在执行前，补安全负向测试。

主要文件：`crates/tools/src/manager.rs`、`tool_runtime.rs`、`execution.rs`、`registry.rs`、`crates/agent/src/layer.rs`、`session/`、`crates/app-binary/src/runtime.rs`。

验收：Agent 不依赖 tools 具体服务类型；启动完成后执行上下文不可观察到半绑定 runtime；工具目录/授权/取消/超时行为不变。

### 阶段 5：RuntimeConfigCoordinator（P1）

目的：命令只提交配置 patch，运行时应用由一个协调器按依赖顺序完成。

工作项：

- 从 `update_settings` 提取 typed `RuntimeConfigCoordinator` 和 `RuntimeApplyPlan`；
- model mutation 的 durable edit、Router target 与完整 prepare→publish 已由 `RuntimeConfigCoordinator` 拥有（ADR 0323）；
- 先校验和持久化 snapshot，再按模型/媒体/工具/MCP/日志/hotkey 依赖应用；
- 评估整体 runtime snapshot 替换与失败补偿策略；在定义可靠逆操作前不将 settings apply 视作事务，也不承诺恢复旧 live snapshot；
- 将配置副作用、通知和日志归属集中；命令保持 `Result<T, String>`；
- 增加应用顺序、半失败、失败元数据和敏感字段不泄漏测试；补偿/回滚测试待策略确定后新增。

主要文件：`crates/common/src/config/service.rs`、`crates/app-binary/src/commands/settings.rs`、`runtime.rs`、`bootstrap.rs`、MCP/tools wiring。

验收：settings command 不再逐个 bind router/tools/agent/MCP；热更新不会观察到混代 runtime；失败路径可诊断。

当前边界：model 命令的完整 apply 路径已统一；`update_settings` 的 phase ownership 与失败可观测性已收口，但 security/MCP/context/logging/hotkey 等副作用仍由命令按原顺序编排。失败记录不代表补偿成功；是否提供逐阶段逆操作、重启恢复或接受半应用状态，仍须单独决策（ADR 0324）。

### 阶段 6：LLM Router 请求对象化（P2）

目的：减少 router 公共包装入口，不改变 provider adapter。

已完成的请求对象切片：普通与 tools 完整请求统一为 `CompleteRequest`（ADR 0234）；聚合 streaming 主入口使用借用式 `StreamRequest`，cancel/hooks 作为独立执行控制（ADR 0241）；embedding 与 health_check 主入口使用 `EmbeddingRequest`、`HealthCheckRequest`（ADR 0242）。Router chat 转发入口及仓库内调用点已删除，不提供兼容别名。请求对象只承载路由数据；Router 保留路由、配置 snapshot、permit、熔断、规则与 health/cooldown 状态，内部执行器复用既有策略与结果投影入口。聚合 stream 的取消豁免保持单独分支（ADR 0246）。

工作项：

- 已完成第一步（ADR 0319）：在 ModelDirectory route filtering 使用 crate-private `RequestDescriptor` 显式并列逻辑用途 `RequestKind` 与 provider/model `Capability`；`LlmCallKind` 继续作为独立 usage owner。`RequestKind` 仍保留原配置/route key 与序列化形式；
- 已完成（ADR 0316）：crate-private `ModelDirectory` 拥有 provider client map、primary route 表、按 request 选择/解析 client 与 model id、configured route 判定及 capability/endpoint/context-window metadata lookup；Router 的 config snapshot 是唯一配置真源，health/circuit/rate-limit/semaphore、stream rules、retry/timeout/usage/cancellation 和执行编排继续由 Router 持有；
- 已完成（ADR 0318）：crate-private CallExecutor 接管已解析 client/model 的 plain/tools complete 和非空 embedding 的内容校验、现有 retry/total timeout 与 Router outcome 投影调用；Router 仍决定路由并持有全部可变运行态，empty embedding 仍先于 route/permit 快返。
- 已完成第一刀（ADR 0327）：crate-private `StreamExecutor` 只接管 raw `chat_stream` 的 validate、既有 retry/total timeout、最终 outcome closure 投影与 `PermitStream` 包装；Router 仍拥有 route/client 选择、permit acquisition/cooldown、circuit、config snapshot 和健康状态。retry 仅覆盖建流，permit 保持到返回 stream drop。
- 已完成第二刀（ADR 0328）：crate-private `AggregatedStreamExecutor` 接管聚合流的共享 `StreamContext`、首次 `on_chunk` 交付前重试、StreamRule guidance retry、聚合总 timeout 与 final outcome handoff；`streaming.rs` 继续负责单条 provider stream 消费/聚合。Router 保留 client/model 选择、两处配置读取时点、permit 生命周期、stream rule 状态及 health/cooldown 投影；Cancelled 仍跳过 health failure。
- 已完成（ADR 0329）：Router 将 `RequestDescriptor` 贯穿到 CallExecutor 的 complete/embedding、raw StreamExecutor 和 AggregatedStreamExecutor；ModelDirectory 仍以 `RequestKind` 为 route key，并拒绝与 primary route descriptor 不一致的执行请求。
- 仍待独立 slice：public `CompleteRequest`/`PromptRequest`/`StreamRequest` 与专用入口仍保留 `RequestKind` route data；health/native transcription、metadata/config helpers 和仓库其他 RequestKind 调用点尚未评估为 descriptor 使用者。`LlmCallKind` usage role 必须从调用方边界显式传递，不能由 Router 推断。
- 删除只转发参数的 chat/chat_request/output_cap/stream wrapper；
- provider adapter 只做 wire mapping，保持 golden fixture。

主要文件：`crates/llm/src/router.rs`、`model_directory.rs`、`types.rs`、`request_pipeline.rs`、`streaming.rs`、Agent/Tools 调用点。

验收：四类能力各有 request object 和负向 capability 测试；ModelDirectory 覆盖生产 credential/capability filtering、注入 route filtering、无 route、shared model identity 与 endpoint/context-window metadata；provider wire 与 usage/stream 契约不变；无重复 retry/usage 入口。

2026-09-25 切片进展（ADR 0316）：ModelDirectory 已接管模型 client 与 primary route 目录，并按生产/注入构造保持对应 credential 与 capability 过滤；endpoint/context-window metadata 通过借用 Router 的单一 config snapshot 查询。Router 仍拥有所有执行状态和策略。

2026-09-25 切片进展（ADR 0318）：crate-private CallExecutor 只接收 Router 已解析的 model identity/client 与单一 RequestPolicy，接管 plain/tools complete 和非空 embedding 的校验、retry、总 timeout 及通过 Router 闭包完成的 health/rate-limit outcome 投影；permit wrapper 只负责并发与 cooldown 等待，避免同一 429 cooldown 重复投影。embedding empty-input 仍在路由前快返。未完成：raw/aggregated streaming execution ownership，以及 RequestKind 的 capability/call-purpose/UI usage role split。无配置、持久化或 wire 重置要求。

2026-09-25 切片进展（ADR 0319）：ModelDirectory 构造 primary routes 时以 `RequestDescriptor` 将逻辑用途/原 route key 与显式 `Capability` 分开；production 仍要求凭据及能力匹配，注入 route 仍要求能力匹配。完整请求继续通过原 `RequestKind` 选配置和模型；配置字符串与 `LlmCallKind` usage owner 不变。此为语义类型第一步；完整 descriptor 向其他 Router 请求 DTO、streaming、embedding/health-check、metadata/config helpers 和仓库调用点迁移仍待独立评估。

2026-09-25 切片进展（ADR 0327）：raw stream 建流执行已迁入 `StreamExecutor`，复用 `request_pipeline` retry/timeout 和 Router outcome closure；permit 包装仍覆盖 stream 完整对象生命周期。聚合 streaming retry/guidance/cancellation/callback orchestration 保留在原 Router/`streaming.rs` 路径；aggregated stream executor 与 descriptor 全贯穿仍待后续切片。

2026-09-25 切片进展（ADR 0329）：`RequestDescriptor` 从 ModelDirectory route table 解析一路传入 complete/embedding、raw stream 与 aggregated stream executor；route key 仍为原 `RequestKind`，能力不匹配或 route descriptor 不一致时 fail closed。descriptor mapping 继续委托唯一的 `RequestKind::required_capability()`；usage role 仍由 Agent/Tools 调用方所有，public request DTO、health/native transcription、metadata/config helper 与仓库调用点迁移待后续评估。

### 阶段 7：统一 Job 生命周期与 MemoryRuntime（P2）

目的：减少后台/定时任务重复状态，并把记忆后台编排移出 Agent ReAct。

工作项：

- 在现有 `actions` 表和状态模型上把 trigger 与 execution 分开，后台任务是 `Immediate` trigger，定时任务是 `At/After` trigger；
- 统一 claim、cancel、timeout、retry、tail output、completion outbox 和 UI projection；
- messaging 不并入 Job，仍是独立 transport domain；
- `MemoryRuntime` 监听 committed session event，负责 fact extraction/maintenance/index catch-up；
- `MemoryWorker` 的 FastChat 调用已通过小型 `MemoryInferencePort` 注入（ADR 0247）；Agent recall 查询已由 `MemoryRecallStore` 提供（ADR 0304）；ordinary/summary 事实抽取状态、批量写入与确定性/LLM maintenance persistence 已通过专用 stores 收口（ADR 0308–0312）。后续仍需让 Agent 只提交 `SessionCommitted`；embedding catch-up 继续由 `MemoryService` 提供。

状态：committed-event consumer 架构设计已完成并采纳（ADR 0259）；Phase 7.1 的 SessionStore cursor/replay、`MemoryRuntime::process_event` 顺序处理、启动时已有 cursor 回放和 `run_until_cancelled` bounded live/replay recovery 已实现，并由 AgentLayer 在 dispatcher recovery 前装配与启动（ADR 0261、0262、0263）。interval、pause trigger 与 compact-summary extraction 已通过 durable producer/outbox 接入，周期 maintenance 调度已由 MemoryRuntime 负责；MemoryWorker durable outbox 已加入逐 job retry/backoff 和应用停机 cancellation boundary，marker 持久化与读取/ack 现归 MemoryStore（ADR 0264、0265、0266、0267、0268、0269、0301）。ActionService 的全部 action 持久化经 ActionStore 调度，后台终态与 outbox 仍由同一事务提交，transcript durable 后才 ack（ADR 0305）。Phase 7 终态内核切片（ADR 0317）让后台与 scheduled 共用终态构造、时间戳语义、认领判定和进程内提交 guard；各自 CAS/outbox、重试、process kill、timer/consumer 回滚和事件顺序仍分开。ADR 0321 让两类按 session 清理共用 typed live-owned action 选择与串行遍历；background-only 与 explicit full cancellation 的范围、family-specific callback、background terminal board cleanup 和各自错误处理顺序保持原样。ADR 0325 将 completion DTO、receiver、broadcast 与 scheduled pending-fire claim/lease recovery 收至 crate-private `action_completion` transport；ADR 0332 又让 background outbox claim 与 scheduled fire claim 共用纯 typed `ActionLease<T>` 决策核心，同时保留 SQLite `BEGIN IMMEDIATE`/CAS、30 秒 durable outbox lease、15 分钟 scheduled in-process lease、terminal/outbox ack 与 timer rollback 边界。ADR 0334 将 background/scheduled 终态持久化修复的 deadline/attempt/backoff/stop decision 收至纯 typed `ActionPersistenceRetryPolicy`；生产路径仍无 deadline 或 retry budget，scheduled 每次 store 操作的 3 次/50 ms 重试、CAS/outbox/timer rollback 和 Agent completion delivery retry 各留原 owner。两类 claim 现有身份仍只是稳定的 `action_result_id` / `action_id`，没有独立 claimant owner token 或续租操作；scheduled terminal 会清除 pending fire 与 lease，background lease 过期后仍可恢复直至 durable ack。trigger/execution 分离、action-level 执行 timeout、tail output/UI projection 和完整 Job 生命周期仍未完成。usage runtime 的 cache accounting 与 `llm_usage.call_kind` 运行时输入均已完成 typed input 收口（ADR 0270、0271）；durable read model 和 IPC 继续使用既有字符串字段。

主要文件：`crates/tools/src/action_service.rs`、`action_terminal.rs`、`action_lifecycle.rs`、`crates/agent/src/memory_worker.rs`、`memory_service.rs`、`memory_index.rs`、`crates/memory/src/`。

验收：阶段目标仍要求两类 Job 共用一套完整生命周期和 UI 投影；截至 ADR 0334 已完成终态内核、session cancellation skeleton、completion transport ownership、跨 kind typed claim/lease core 与终态持久化 retry decision。trigger/execution、action-level 执行 timeout、tail output 和 UI projection 仍未统一。ActionLease 单测覆盖 claim success/conflict、expiry/invalidation 和 token mismatch；ActionPersistenceRetryPolicy 单测覆盖 deadline、retryability、attempt/budget、cancel/terminal 与 backoff；outbox 测试覆盖过期 claim 恢复及 result identity ack，ActionService 测试覆盖 background durable retry/outbox publication 与 scheduled terminal retry、跨 receiver 去重、terminal 清除和 no-consumer rollback。记忆失败不改变 ReAct turn 结果；重启、重复 outbox、取消和限额有测试。

2026-09-25 切片进展（ADR 0332）：background completion outbox 与 scheduled fire recovery 使用同一个纯 `ActionLease<T>` core 判断有效 claim、过期可重新 claim 和身份匹配；background 仍由 SQLite 30 秒 deadline/CAS 与 `action_result_id` ack 恢复，scheduled 仍由共享进程 map、15 分钟单调时钟 lease、`action_id` 及 terminal/no-consumer 清理恢复。未增加 owner token、lease renewal、schema 或 IPC；完整 Job 生命周期、timeout/retry、tail output 和 UI projection 仍待后续阶段。

2026-09-25 切片进展（ADR 0334）：ActionService 两个终态持久化修复 worker 共用纯 `ActionPersistenceRetryPolicy` 决定 deadline、attempt/backoff 与 stop reason；生产 policy 继续无 deadline/预算，退避为 1 秒起步、指数增长、30 秒封顶。scheduled store call 的短重试、background terminal/outbox CAS、Agent durable result projection/ack、claim lease 与各自 rollback 不变。这里没有 action-level execution timeout，也没有自动重放失败 job；provider/LLM 和 Agent tool-call retries 仍属各自请求策略。tail output/UI projection 与完整 Job 生命周期仍未完成。

Phase 7.1 验收与未决风险：见 ADR 0259、0261、0262、0263、0264、0265、0266、0267、0268、0269、0301、0304、0305、0307、0308、0309、0310、0311、0312。当前已完成持久化端口、按序处理核心、启动回放、bounded live/replay runner、AgentLayer dispatcher readiness barrier、interval/pause producer、summary per-episode durable outbox、maintenance scheduler ownership、MemoryStore marker persistence/read/ack ownership、MemoryWorker retry/backoff、app shutdown boundary、Agent recall/query 的 MemoryRecallStore、MemoryWorker known-facts prompt 读取与事实批量写入的 MemoryFactStore、普通 session fact extraction 状态与投影的 MemoryFactExtractionStore、确定性及 LLM maintenance persistence 的 MemoryMaintenanceStore 和 ActionService 的 ActionStore；完整 Job 生命周期仍未迁移。summary extraction 的 episode cursor 与共享 throttle KV 现经 MemoryFactExtractionStore 读写；生产 MemoryWorker raw Database 使用已清零。模型维护 query/write 均经 MemoryMaintenanceStore，embedding catch-up 沿用 MemoryService 的 MemoryEmbeddingStore。

### 阶段 8：IPC 单源生成与 UI 编排收口（P2）

目的：减少 Rust wire DTO、TS contract、mapper、文档之间的重复维护，同时保留一个权威 session store。

工作项：

- Rust wire DTO 作为生成 TypeScript contract/字段映射的唯一来源；保留前端运行时校验；
- 命令数量不为减少复杂度而强行合并；事件按 `session/agent/action/recording/app` envelope 收敛；
- 已完成（ADR 0313）：将 `+page.svelte` 的 session submit、resume reload、rollback、continue、switch/终态内存回收、end/interrupt 编排抽到 typed `ChatController`；
- 已完成（ADR 0315）：聊天页 session/app/agent/usage handler map 与异步注册生命周期抽到 typed `chatEventController`；`events.ts` 是共享注册入口并调用各领域 mapper；
- 已完成（ADR 0320）：聊天页 model/effort/web-search 操作及 typed payload、成功状态更新、通知、失败处理与 refresh-suppression 收口到纯 TypeScript `chatModelOperations`；页面只注入 Svelte state callbacks 并传入 toolbar。`chatModelSync` 仍拥有 discovery/settings 同步。
- 已完成（ADR 0330，本切片）：session lifecycle wire event 使用单一 `contracts/session.ts` mapper，删除重复的 TS wire-interface 镜像；mapper 校验必需字段、忽略未知扩展字段，并复用既有 optional/unknown-enum 降级。此处仍保留手写的内部 camelCase DTO 与映射代码，不代表 Rust DTO 到 TypeScript 的生成已完成。
- 已完成（ADR 0335，本切片）：Action board 的 `list_actions` rows 与四个 action lifecycle channels 共用 Rust `ActionEvent` wire DTO 和 `contracts/action.ts::mapActionPayload`。Command store 与 event listener 共用运行时 validator/mapper；保留未知 status→`failed` 降级，未知 kind/必需字段错误 fail closed，忽略未知附加字段。Action completion outbox 与动态 JSON `tool_args` 保持现有独立边界。
- 剩余：command request/response、recording event、settings contract 的手写 mirrors 仍待按域收口；session mapper 内部 camelCase DTO/字段映射也仍是手写代码。`contracts/agent.ts`、`app.ts` 等其余 event DTO mirrors 同样待处理。未来可评估 Rust DTO 驱动的生成流程，但本阶段不引入 codegen。`events.ts` 是共享注册/分发入口，各领域 mapper 只在对应 contract 模块定义一次。ask/input 决策、启动恢复与 view state 继续由页面/controller 编排；
- 已完成（ADR 0314）：将 `sessionReducer.ts` 按 lifecycle/transcript/interaction/usage/stream 拆成内部 reducer module；外部 API 和单一 `sessionStateStore` 订阅保持不变；
- 已完成（ADR 0322）：页面与布局不再把完整 `SessionReducerState` 镜像到 `$state`；通过相等性门控 selector 订阅同一个 `sessionStateStore` 的 sessions、active session ID、活动 transcript/usage、interactions 和必要的 error/termination 切片。selector 只缓存当前结果，引用不变时不通知，最后一个 listener 离开时释放 root subscription；reducer state ownership、dispatch、事件顺序均不变。
- 剩余：ask/input 决策、复杂 view state 与启动恢复仍由页面/controller 原路径编排，replay 继续由现有 reducer/event 路径管理。ADR 0322 只让页面读取的 interactions 进入 selector，不迁移 ask/input 决策状态或 replay 状态；contract generation 与剩余手写 mirror 范围见本阶段上方的 ADR 0330 条目。

主要文件：`crates/app-binary/src/events.rs`、`ui/src/lib/contracts/`、`ui/src/lib/sessionReducer.ts`、`ui/src/lib/sessionReducer/`、`ui/src/routes/+page.svelte`、`+layout.svelte`、scripts。

验收：Rust/TS 生成检查在 CI 通过；单一事件登记点；UI session/stream/resume/rollback/optimistic 行为测试通过。

2026-09-25 切片进展（ADR 0313）：Controller 只通过 typed invoke/submit/reducer/session-snapshot/通知与 UI callback dependencies 执行会话异步流程；`continueSession.ts` 与 `resumeMessages.ts` 保持纯策略/message projection 边界。页面保留 input-router/ask 决策、传入 `chatEventController` 的 typed callback wiring、model sync、resume target/auto-restore、新会话入口及 dialog/loading/menu/scroll 状态。纯 Vitest 覆盖 rollback 两分支、continue 两种策略、interaction preservation、created-session selection 和失败/重复请求保护。其余 Phase 8 工作仍按上列范围推进。

2026-09-25 切片进展（ADR 0314）：`SessionReducer` 内部实现已按 lifecycle、transcript、interaction、usage、Agent stream 与共享 replay/state helper 拆分；原 facade 继续拥有跨域 resume/clear 组合、observable wrapper 和唯一 writable store。回归覆盖 resume + pending interaction + usage restore/live、stream reset + chunk sequence、error + termination 刷新。

2026-09-25 切片进展（ADR 0315）：`chatEventController` 拥有聊天页 handler map 组合及注册/释放生命周期；页面等待 listener ready 后才加载 settings 和恢复会话。`events.ts` 拥有共享 listener registration 与领域 mapper 调用入口；测试通过注入 registration port 覆盖通道、ready 和释放竞态。

2026-09-25 切片进展（ADR 0320）：`chatModelOperations` 拥有三个 toolbar model 操作的 typed payload、状态更新、通知、错误处理与 refresh suppression；页面仅接线，`chatModelSync` 继续拥有 settings/discovery。Rust DTO → TS contract/mapper generation 与旧手写 contract 镜像清理仍待后续。

2026-09-25 切片进展（ADR 0322）：`createSessionSelectorStore` 只观察唯一 `sessionStateStore`，按 `Object.is` 对当前选择结果门控通知，并在 selector 最后一个订阅者释放时断开 root subscription。页面迁移 sessions、active session ID、活动消息、usage、interactions、error/termination；布局迁移 sessions、active session ID、interactions。活动消息按 reducer 当前 active ID 选取，缺失消息复用稳定空数组。完整 root state 不再广播到这两个路由的 `$state` 镜像；复杂 view state、ask/input 决策、启动恢复和 replay 仍走既有路径。无 reducer ownership、状态转换、事件顺序、IPC 或持久化变化。

2026-09-25 切片进展（ADR 0330）：以 app-binary session event DTO 为 wire 权威，`contracts/session.ts` 是 session lifecycle snake_case→camelCase 的唯一前端 mapper；删除重复 TS wire interfaces，用运行时校验拒绝 malformed required fields、丢弃未知事件、忽略新增字段，并保留未知 status→`error`、未知 waiting reason→`null` 的降级。`events.ts` 的两个 session listener 路径共用该 mapper；chat controller/handler/reducer 仅接收已映射 DTO。channel、payload、顺序、幂等和 UI 行为不变。其余 event/command DTO mirror 及 session 内部类型/字段映射的生成收口仍待后续阶段。

2026-09-25 切片进展（ADR 0335）：以 app-binary `ActionEvent` 作为 Action board 与 lifecycle wire 权威；`list_actions` command rows 和 `action:created/updated/output/finished` 通过 `contracts/action.ts::mapActionPayload` 共用唯一 runtime validator/mapper。`actionStore` 与 `events.ts` 两个消费路径删除不安全的 wire casts；错误必需字段/未知 kind 丢弃，未知 status 仍降级为 `failed`，未知扩展字段忽略，warning 不含 payload。字段、排序、分页、running/terminal 投影、event/command 注册、通知和 completion outbox 顺序不变。Action completion outbox 是独立的 agent 内部完成类型，`tool_args` 仍为原始 JSON 扩展，不纳入 UI ActionEvent。剩余手写 mirrors 明确包括 command request/response、recording events、settings contracts、session 内部 camelCase DTO/字段映射，以及 agent/app event DTO；未引入 codegen。

### 阶段 9：Common 收缩、性能剖析和发布验收（最后）

目的：只有在所有权和稳定边界稳定后，才决定是否拆 crate 和做性能优化。

工作项：

- 根据依赖图决定是否把 common 拆成 contracts/config/media/platform；若只是移动复杂度则不拆；
- 对 actor mailbox、event replay、reducer broadcast、LLM request、Job/Memory outbox 做 profiling；
- 只根据数据加入 bounded cache、selector 或批处理；每个缓存写清容量、失效和取消；
- 用全新数据目录完成启动、设置、会话、工具、媒体、任务、恢复、回滚、升级重置和卸载验收；
- 更新 `architecture.md`、`stability-refactor-plan.md`、ADR 索引、发布/重置说明。

## 5. Agent 委派策略

- 只把有明确写集和退出条件的阶段交给一个 Agent；模型固定 `gpt-6-luna`、reasoning `xhigh`。
- 不让两个 Agent 同时修改同一文件；独立审查和测试可以并行。
- 每个代码 Agent 必须：先读规范/相关 ADR；增加或保留回归测试；删除旧路径；运行适用门禁；提交单一目的 commit；报告改动文件、测试和遗留风险。
- 主线程集成前检查 `git diff`、`git diff --check`、依赖方向和旧入口搜索；必要时补测试或拒绝补丁。
- 每一阶段完成后再派发下一阶段；不得用“先留下兼容层”绕过退出条件。

## 6. 全局完成定义

全部阶段不等于文件变少，而是以下检查同时成立：

- durable session 恢复只依赖事件回放；
- session-local mutable state 只有 actor owner；
- 上层没有 raw Database/ToolsManager service locator 穿透；
- stable domain outputs typed，动态 JSON 边界有明确注释；
- 配置、工具、模型、任务、记忆的 runtime replacement/失败/取消语义有测试；
- Rust/TS IPC 生成与校验一致；
- 后端 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`，UI `corepack pnpm run check`、`corepack pnpm run test:run` 与生产构建通过；
- 文档、ADR、重置说明、Git 历史和工作区状态可审查。
