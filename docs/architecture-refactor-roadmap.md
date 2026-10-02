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

- 人工交互仍是统一 renderer DTO 下的多 owner 实现：session request 由 `session_events` / actor 恢复，scheduled confirm 与 UI 直调各有进程内 registry；ADR 0156 的 snapshot 来源已被 ADR 0196 取代。typed owner 路由、统一后端 expiry、授权与动作完成分离、通知消费 lifecycle event 的目标见 ADR 0424；
- 权限管理已分别列示、撤销和重置永久规则与 session grants；没有持久 session owner 的界面/调度确认不能写 session grant。ADR 0425 收口设置范围，交互 owner registry/fallback 迁移仍按 ADR 0424 后续切片处理；
- `resume.rs` 负责组装 `RunReplay` 并向 actor 发起 run；active run future 存在 `SessionState::react_run`，并独占捕获 run-local `ReActState`（ADR 0382）；
- actor mailbox 仍包含 messaging poll 等 actor-local 命令；usage 由 `UsageRuntime` 所有，stream identity 与 token estimate 已随单次 `ReActState` 收口，不再是跨 session engine sidecar；
- resume 仍需协调 RAM 队列、持久 pending-input 状态、partial promotion 和 interaction replay；
- Agent、Tools、App 的部分 facade 仍较宽；已迁移路径通过 typed store/port 访问。`SessionSupervisor` 的生产构造接收组合根创建的 `SessionStore` 与 `SessionToolPorts`（ADR 0363、0388）；`AgentLayer::build` 接收组合根创建的 `MemoryService` 与 `AgentToolPorts`，后台 session/media cleanup 也经 typed ports（ADR 0364、0365、0374、0384）。生产 Agent 不通过 `get_tools()`/通用 `services()` 做 service locator 查找；生产 `haven-agent` 只依赖能力 ports，不持有具体 `ToolsManager`，manager-backed adapters 由 `haven-app-binary` composition root 创建（ADR 0388）。阶段 3 将 `MemoryService` 改为只接收 typed stores，并完成 ActionService 稳定 status/list projection 的阶段范围审计（ADR 0383）；组合根仍可持有 raw `Database` 创建 typed stores。
- ActionService 的终态仲裁、claim lease、持久化 retry、tail policy、scheduled trigger policy、UI 投影与 admin writer owner 已有各自审计边界（ADR 0317、0325、0332、0334、0338、0343、0344、0373）；scheduled dependency relation/result 与重启 watcher 恢复已由 ADR 0392 实现，scheduled tool completed/failed transcript delivery 与跨 kind owner 边界由 ADR 0393 收口；完整通用 Job executor、统一 action deadline、owner token/续租与自动 replay 是明确非目标；terminal history 在 completion ack 前拒绝删除，ack/delete 与无 owner completion/迟到绑定 writer race 已由 ADR 0374 收口；LLM request descriptor 与 usage owner 契约已由 ADR 0354 收口；
- Settings 的 typed target/phase plan、执行顺序与失败观测现由 `SettingsRuntimeApplyCoordinator` 持有；命令回调仍调用既有 runtime owner。`RuntimeConfigCoordinator` 持有 model edit 及 Router/media prepare/publish；Settings 不做全量补偿/rollback 或显式 retry；
- 已完成配置 apply 边界审计及实现（ADR 0351、0372）：`ConfigService` 是唯一进程配置 owner，Settings/model 与同配置域 Tools AdminServices writer 共用 app composition root 创建的 gate；domain→target 只有 `RuntimeConfigApplyPlan` 一份，Settings phases 是其派生计划。纯 `SkillsExec` 仅持久化并标记 restart-required，跳过 live Skills phase；Skills 与 SkillsExec 混合变更照常执行 Skills live phase，同时保留 restart-required。durable-first 后 apply failure 保留磁盘配置、不自动 retry/compensate，普通会话和工具在重启前仍可继续使用各自当前 live runtime，允许 consumer 暂时处于不同 config revision；重启从磁盘初始化。`update_settings` 唯一前端 builder 复用开放式 `SettingsPayload`，脚本校验 handler/registry/直接 caller，不复制 Rust nested schema。未改配置格式和数据库 schema，无需数据重置。
- UI 页面和 reducer 已有边界；ask/input 决策、启动恢复与复杂 view/scroll state 已收口到独立 owner。Rust handler/Serde DTO 到 TypeScript command contract 已全局生成；前端保留运行时校验与 event mapper，command registry/docs 只保留人工审阅的边界、安全说明和事件语义。

产品决策状态更新（2026-09-29，通知见 ADR 0373；dependency recovery 与 transcript result 见 ADR 0392、0393）：Settings apply 失败保留 durable config、报告部分失败、不自动重试，重启从磁盘配置重新初始化；terminal history 在 completion ack 前拒绝删除；不增加统一 action-level deadline，沿用工具自身 timeout 与用户取消。Dependency-waiting 关系和有界结果持久化，重启恢复 watcher 并保证 continuation 至多触发一次；scheduled tool completed/failed 结果通过 completion outbox 进入 transcript，Continue 保持正常输入 transcript，cancelled 不投影。通用 Job executor、owner token/续租和自动 replay 均不属于当前契约。

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
- `2b9844d`：UI 页面直接派生 active session 的 token stats/LLM usage，删除 usage 镜像与同步 effect；当时 ReActEngine 剩余 Database 依赖经审查后暂缓统一封装，因为读取、事件权威写入、transcript 投影和记忆副作用仍是不同事务边界；后由 ADR 0299 移除 ReActEngine 的 raw Database ownership，改用 `MemoryStore` 与 `SessionStore`。
- `95189fa`、`d7dd2e3`：MemoryEmbeddingIndex 成为 embedding vector-space identity 的唯一解析者；ActionService 接管 App action 历史读取，命令不再直接读取 raw Database（ADR 0232；ADR 0215 边界补充）。
- `cfa080b`：会话恢复、初始输入和清理路径通过 `ManagedAssetLeasePort` 管理媒体资产租约，工具 session overlay 注销保持独立（ADR 0233）。
- Phase 6 首个请求对象切片：`CompleteRequest` 成为普通与 tools 完整请求的唯一 Router 入口；删除 Router chat 转发 API 并迁移仓库内调用点（ADR 0234）。
- `3bc807d`：回滚目标先校验，SessionStore 在事务内解析消息/分支 projection boundary，过期 transcript cursor fail closed（补充 ADR 0207）。
- `0321a61`：settings/model 共用配置应用串行边界，Router 与媒体依赖从同一 `ConfigSnapshot` 预构建，准备失败不修改 live runtime（ADR 0235）。
- `update_settings` 的旧 hotkey 读取与 Settings 合并现由同一次 `ConfigService::edit` 完成；该 edit 返回运行时应用所用的旧 hotkey、snapshot 和 change。移除外层 snapshot 读取及 permissions 复制，no-op 仍在 runtime apply 前快返；`AppConfig::apply_settings` 继续保护 permissions、`encrypt_sensitive` 和表单不管理字段，运行时顺序与半失败语义不变（补充 ADR 0235）。此为早期切片状态；model apply owner 后由 ADR 0323 收口，Settings phases 后由 ADR 0324、0337 收口，补偿/rollback 与 restart recovery 仍未决（ADR 0351）。
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
- 已完成（ADR 0245、0376）：历史错误原因由 `SessionReducer` 独占；页面通过 reducer 直接读写，删除原兼容委托模块。恢复交互只接受 Rust 当前 snake_case DTO，删除 camelCase 别名、字段 coercion 和生成默认时间；busy 清除时机与历史页行为不变。
- 当前切片：LLM Router 的 native transcription、complete、embedding、raw stream 建流和 health check 共用请求结果投影；聚合 stream 的取消豁免与其他状态语义不变（ADR 0246）。
- 当前切片：`MemoryWorker` 的 FastChat 调用经注入的 `MemoryInferencePort`；Agent 层的事实抽取、维护与游标行为不变，Router 适配器独占请求类型和响应 DTO（ADR 0247）。
- `ActionService` 后台终态仲裁已实现（ADR 0248）：数据库提交后再发布，错误重试，CAS 输家静默对齐，cleanup 保留未提交条目；持久 late attach 使用 outbox 恢复，headless late attach 仍补发 owner 绑定后的 completion。
- 当前切片：SessionStore 增加按 ID 读取 session record 与全量 pending records 的 typed 方法；保持缺失返回、pending 状态过滤、`created_at DESC`、`limit=-1/offset=0` 及 dispatcher 安装/入队顺序（ADR 0249）。
- 当前 Phase 9 切片：移除无仓库调用的 `LlmRouter::chat_with_prompt_output_cap`；`CompleteRequest` 仍直接承载 `max_output_tokens`，其他有调用或有独立语义的 helper 保留（ADR 0250）。
- `acd11de`：PartialStore 的 checkpoint/promote/discard 改经共享的 SessionStore typed port；保留 per-session lock、generation、原子 promote 与 scratch projection 时序，不改变 schema（ADR 0251）。
- `a14f596`：标题生成与后台记忆推理迁移到 `PromptRequest`，删除旧三参数 `chat_with_prompt`；全仓代码调用为零，Router 只保留拥有数据的请求入口（ADR 0252）。
- `5aadd3c`：settings/model 的 Router prepare/publish 编排归 `RuntimeConfigCoordinator`，model 使用完整 prepare→publish，settings 保留原有分阶段副作用顺序；删除 commands 层重复 Router helper（ADR 0253）。
- 当前 Phase 5 切片：模型命令只提供 selector validation/slot mutation 闭包；`RuntimeConfigCoordinator` 统一持有 model 的 gate、durable edit、Router target 判定及完整 prepare→publish（ADR 0323）。Settings 使用独立的 `SettingsApplyPlan` / `SettingsRuntimeApplyCoordinator`，从共享 `RuntimeConfigApplyPlan` 派生 target 和原阶段顺序，驱动命令提供的副作用回调，并唯一跟踪 phase/failure、snapshot version、Router published 与 restart-required targets。ConfigService edit/no-op、原副作用 owner/顺序、错误 renderer 和半失败语义保持；不提供 live runtime compensation 或 rollback；现按 durable-first 部分失败、重启从磁盘恢复（ADR 0351、0372）。
- `7775e11`：`TranscriptBatchWriter` 只依赖 `SessionStore`；阻塞调度、可选取消和缺少 session 行的兼容语义下沉到 typed transcript batch port，保留既有事务实现（Phase 3，ADR 0255）。
- 已完成（ADR 0336）：ReAct live transcript 以 `SessionCommitted` domain intent 提交；SessionStore 在同一事务中先追加 events、再物化消息/步骤投影，提交后才广播。Agent 不再构造 storage-shaped transcript batch；CommittedUiPublisher 仍在提交成功后按 sequence 发布。
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
- 当前 Phase 7 小切片：删除全仓无调用的 `MemoryWorker::recall_memory_query` 转发；recall 继续由 `MemoryService`/`MemoryRecallPort` 所有，不新增同义 `MemoryReader` trait（ADR 0254）。MemoryRuntime 已完成 cursor/replay/ordered trigger/live recovery 核心；ADR 0367 将 prepare、live consumer 与 periodic task 的应用所有权移至 ApplicationRuntime，dispatcher 仍在内存恢复准备与 live task 注册后才开放。interval、pause hook trigger、compaction-summary extraction 与周期 maintenance 调度均已收口到 MemoryRuntime 的 durable producer/outbox/scheduler 边界（ADR 0264、0265、0266、0267）。
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
- ADR 0257 的 Phase 4 catalog slice 曾由 `AgentLayer` 直接共享 `ToolsManager` 给 prompt builder 与 catalog adapter；engine 不再从 executor 查找 catalog port，snapshot/session ID 与 live authorization 语义保持（ADR 0257）。ADR 0388 随后将所有 manager-backed adapters 移至 app composition root。
- Phase 4 最终收口（ADR 0374、0384、0388）：`AgentLayer::build` 接收组合根构造的 `AgentToolPorts`，生产 `SessionSupervisor` 接收 `SessionToolPorts`。`ToolExecutionContext` 跨边界携带 session/tool/input/cancel/step identity；执行与授权准备、prompt/catalog/observation、session overlay 和 asset lease 各由窄 port 表达。`haven-agent` 生产代码不依赖具体 `ToolsManager`，共享 manager adapters 由 `haven-app-binary` composition root 创建。live authorization 仍在执行前判断，turn catalog 仍使用同一不可变 snapshot。
- 当前 Phase 4 切片：ToolsManager façade 请求 crate-private `ToolRuntimeCoordinator` 构造并解析唯一的 `ToolCapabilitySnapshot`，同一个 platform generation 的媒体 operation catalog、prompt capability、TTS/STT 与录音 gate 都从该 snapshot 读取；搜索的 provider/MCP/unavailable 优先级也由它统一投影。每次读取从当前 `PlatformRuntime`、Router config 和 MCP index 重建。PlatformRuntime 替换、Router config 发布和 MCP tools/list 更新没有共同版本钟，因此暂不缓存；不引入第二套 capability mapping（ADR 0331，承接 ADR 0326）。ADR 0333 将 runtime composition、平台/config 更新顺序、MCP discovery config/index 与 catalog rebuild 移入 `ToolRuntimeCoordinator`。app-binary 的 Router 准备/config gate、`update_settings` 跨阶段编排、MCP 命令的持久化和连接动作、McpManager 连接/catalog-version owner，以及 `ApplicationRuntime` shutdown 顺序继续留在原边界。ADR 0345 将授权请求策略解析从执行器中拆出；审计确认 session overlay、asset lease 和 catalog snapshot 分别由 `SessionCatalog`、`ManagedAssetRegistry` 与 `OperationCatalog`/`ToolCatalogSnapshot` 提供单一状态或派生视图，不存在要迁移的第二份事实。`ToolsManager` 仍是这些操作的 public façade；更大拆分须先识别新的重复权威或独立生命周期边界。

当前阶段判断（2026-09-29）：阶段 1–8 均已完成。阶段 1 的 SessionState 所有权校准与阶段 3 的 raw Database/typed projection 阶段范围验收由 ADR 0382、0383 关闭。阶段 2 的 search-final fallback 已并入 owning ToolCall commit（ADR 0385）。阶段 3 的 typed-store 调用链已收口；生产 MemoryService 改为仅持有 typed stores。阶段 4 的 execution/authorization ports 与 app composition adapters 已由 ADR 0388 收口；Agent runtime owners 不依赖具体 ToolsManager，live authorization 与 immutable turn catalog 语义保持。阶段 5 的 Settings/model/Tools 同域写 gate、durable-first 部分 apply failure 与 SkillsExec restart-only 语义已完成（ADR 0351、0372）；不提供 compensation/rollback 或显式 retry。阶段 6 的请求对象、执行器与 purpose/capability/usage owner 契约已完成（ADR 0354）。阶段 7 的 MemoryRuntime ownership、dependency-waiting 持久化恢复、scheduled terminal transcript 与阶段定义的 Action 边界已按 ADR 0392、0393 完成；完整通用 Job lifecycle、execution timeout、owner token/续租及自动 replay 明确不属于本阶段。阶段 8 的 Rust handler/Serde DTO→TypeScript command contract 生成、命令集合 drift check 与 ask/input、启动恢复、复杂 view/scroll owner 已完成（ADR 0394）；全量 command map 保留运行时校验，registry/docs 不重复 wire fields。SessionCompleted/SessionError 跨 channel 重复清理已由 ADR 0386 使用显式 occurrence identity 收口。阶段 9 保持开放：容量 profile、`SQLITE_FULL` 故障恢复、隔离 APPDATA 首启 bootstrap 与旧 schema 拒绝/重置已验证；真实 GUI 交互、安装升级/卸载和物理盘满验收仍未执行（ADR 0395）。同一 Windows 用户同一时间只能运行一个 Haven 实例，profile smoke 须顺序执行。

状态校准（ADR 0374、0372、0375、0392、0393）：生产 `get_tools()`/`services()` service locator 已删除，AgentLayer 改为组合根显式注入共享 `ToolsManager`，supervisor 仅保留窄 capability wiring。Settings durable-first/partial apply failure/重启从磁盘配置恢复、纯 SkillsExec 变更跳过 live Skills apply 并标记重启生效、Settings/model 与同配置域 Tools admin writer 串行均已实现。terminal history ack/delete guard 已实现。Dependency-waiting durable relation、result summary 与 restart watcher 已实现；scheduled tool completed/failed 通过 outbox 投影 transcript，Continue 沿用 session input 路径。统一 action-level deadline、owner token/续租、通用 Job executor 和自动 replay 明确不属于当前契约。

Dependency-waiting 的已确认细则：producer `completed`/`failed`/`cancelled` 都满足等待条件并向 continuation 传递状态/结果；producer 缺失也只触发一次 continuation 并传递 `not_found`，避免永久 waiting。依赖关系 durable 保留，重启时重建 watcher；依赖满足后 continuation 只执行一次。若 continuation 对应 scheduled action 已进入 `running` 后进程崩溃，重启不自动 replay，而是恢复为 `failed`，由用户手动重试。任务 UI 仍只使用 `waiting`/`running`/terminal 三大状态，具体阶段可在卡片内展示；TaskCenter 活动任务卡已共用 `projectActionCard` projection，保留 kind-specific details 与现有展示行为（ADR 0373）。
统一任务完成通知已实现（ADR 0373）：`notification.action_completed` 提供默认开启、可分别关闭的应用内 toast 与 Windows 通知开关，background 与 scheduled 共用。Settings、旧配置默认值、事件标记、UI toast owner 与 DesktopNotifications gate 已贯通；普通 Agent 通知保持原有语义，不受该设置控制。
后台任务与定时任务的终态结果经共享 ActionCompletion outbox 和 Agent transcript owner 投影；scheduled tool completed/failed 写入有界结果摘要，Continue 使用正常 session input transcript，cancelled 不投影。该边界与 kind-specific execution owner 已由 ADR 0392、0393 收口。

MemoryRuntime 应用对象所有权审计（2026-09-26，ADR 0362）：MemoryRuntime 的启动回放、live consumer 和周期 schedule policy 已有单一对象，但对象仍由 `AgentLayer` 构造/持有，`ApplicationRuntime` 只拥有周期 task 的 cancel/join。`AgentLayer::start_inner` 是当前唯一能保证 prepare/replay 成功后再启动 live consumer 和 SessionActor dispatcher 的入口；app-binary 没有 prepared-consumer API 或 dispatcher readiness handoff。为避免字段空迁移、第二份 runtime 或 Agent 长期强持有 runtime，本轮不改代码。后续先设计唯一 typed 构造交接与可证明的 readiness token/port，再一起迁移 app schedule/manual maintenance/shutdown 接线并保留 barrier 与 lifecycle 回归。

SessionSupervisor 构造边界切片（2026-09-26，ADR 0363）：`new` 与 `new_with_session_tool_overlay_port` 改为接收已有 `SessionStore`；AppState 在组合根显式创建 supervisor 专属 store，继续与 App command/read store 使用不同的 live sender。测试 fixture 仅通过 cfg(test) helper 创建 Store 后再调用 typed constructor。AgentLayer、MemoryService、SystemPromptBuilder 及其他 raw Database 路径未迁移；SessionStore clone 的事件 sender、MemoryRuntime 使用的实例及 start/recovery/shutdown 所有权关系未变。

AgentLayer memory 构造边界切片（2026-09-26，ADR 0364）：`AppState` 在已有 `Database`、`LlmRouter` 与 `ContextLimitsConfig` 下只创建一个 `Arc<MemoryService>`，将它传给 `AgentLayer::new`。AgentLayer 从该实例派生 memory stores、`MemoryWorker`、`MemoryRuntime` 和 `SystemPromptBuilder`；Agent memory、Worker 与 PromptBuilder 共享同一 service/cache，Runtime 继续持有同一 Worker。Router 注入、worker context limits 与 `embedding_chunk_size` 的来源不变。MemoryRuntime 长期对象所有权和 startup readiness barrier 未迁移（ADR 0362）。

MemoryRuntime 应用所有权切片（2026-09-26，ADR 0367）：`AgentLayer::build` 返回唯一的 `AgentStartup { agent, memory_startup }`；AppState 将 `MemoryStartup` 交给 ApplicationRuntime 长期持有。AgentLayer 不再存储或启动 MemoryRuntime。不可 Clone 的 `PreparedMemoryRuntime` 隐藏 live receiver，`MemoryLiveTask::register_with` 只有在 app task 注册成功后才交出 `MemoryReady`，Agent dispatcher 仅通过该 token 和 typed recovery mode 开启。ApplicationRuntime 注册并 join prepare、live consumer 与 maintenance tasks；周期策略、手动单次 maintenance、worker shutdown 与 app exit 顺序不变。启动 retry/cancel、dispatcher not-before-ready、live join 和 manual maintenance 均有回归覆盖。

SystemPromptBuilder 构造边界切片（2026-09-26，ADR 0365）：删除 public `SystemPromptBuilder::new(tools, Arc<Database>)`，使 `with_memory_service` 成为唯一公开构造入口。仓库内原调用只有 agent 测试；测试先以既有 `router=None`、chunk size `64` 创建 `MemoryService`，再调用 typed constructor。生产 AgentLayer、共享 service/cache owner 和 prompt/runtime 行为不变；未知外部下游调用需显式迁移。本切片不改变 `MemoryService::new` 的 raw Database API。

Session 清理与 Agent wiring 收口（2026-09-26，ADR 0374）：`SessionStore` 提供 orphan finalization、retention delete 和 managed attachment reference 三个异步 typed port；AppState 的启动/保留期/上传/每日 cleanup task 不再捕获 raw Database。`AgentLayer::build` 显式接收组合根 `ToolsManager`，生产 `SessionSupervisor::get_tools()`/`services()` 删除，内部改持有窄 authorization/action capability；执行 runner 和 prompt/catalog/observation adapters 仍保留 manager 实现依赖。terminal history 在 completion ack 前的删除 guard、ack/delete writer race、无 owner ack 与迟到绑定 race 也在本 ADR 独立收口；无 step id 的 session-scoped live-output 工具在 action 启动时绑定 owner。

阶段 3 增量校准（2026-09-24）：历史查询命令、App resume read model、两个 resume command 的 session record 读取、`end_session` 展示标题 fallback、桌面通知会话标题 fallback、退出时暂停运行会话与 Agent 标题生成上下文现纳入 SessionStore typed-port 覆盖范围（ADR 0279、0282、0283、0284、0286、0287、0288）；fresh-run window 与 App session title write 路径分别见 ADR 0280、0281；上段阶段汇总记录的是此前已完成的覆盖项。

阶段 3 小切片补充（2026-09-24）：action-completion status 与 peer-session inspection 在 executor miss 时改经既有 SessionStore record port 读取；错误降级/传播和 peer wait 轮询语义不变（ADR 0289）。

阶段 3 小切片补充（2026-09-24）：会话状态通过 SessionStore typed port 调度，Agent 保留原重试和持久化先于 actor 内存变更的语义；其他仍需 Database 的 actor 路径保持原样（ADR 0290）。

阶段 3 小切片补充（2026-09-24）：单会话删除与全量清空的 blocking Database 调度迁入 SessionStore；Agent 继续拥有 closing/lifecycle gate、run quiesce 与 actor/内存清理，Database 删除/事务、缓存、KV 和 embedding cleanup 保持原实现（ADR 0291）。

阶段 3 小切片补充（2026-09-24）：AgentLayer 的三处 session 写路径使用已有 SessionStore 异步端口；首条消息失败仍尽力删除 session 并返回原错误，peer 标题仅在 durable write 成功后更新内存，显式标题事件与 fallback 通知行为保持原顺序。SessionStore 负责 blocking 调度，AgentLayer 继续保留 raw Database 供其他职责使用（ADR 0292）。

阶段 3 小切片补充（2026-09-24）：terminal ingress fallback 的刚持久化用户消息删除改经 `SessionStore::delete_message_by_id`；Memory 端口仅复用既有 Database 删除并调度到 blocking pool，Agent 保留 warning 降级及 session-updated、remove-session、`Supplemented(None)` 顺序（ADR 0293）。

阶段 3/1 历史切片补充（2026-09-24，ADR 0294；SessionSupervisor raw Database 字段后由 ADR 0295 删除，AgentLayer/ReActEngine 的 raw Database ownership 后由 ADR 0299 收口）：interaction domain event replay/append 改经 SessionStore 异步端口；当时 SessionActor spawn 不再接收 raw Database，SessionSupervisor 暂留该字段供 tool_runner action-step 持久化；Agent reducer 继续拥有解析与状态恢复策略，AgentLayer、ReActEngine 等模块的 raw Database 依赖当时不变。

阶段 3 小切片补充（2026-09-24）：tool_runner 的 action-step 写入通过 SessionStore 的三个 typed ports 调度；ensure/start 与 ensure/finish 各保持在单个 blocking closure 内，原 Database 调用顺序、confirmed/outcome 和 bool 语义不变。Agent 仍拥有 policy 与 metadata，SessionSupervisor 不再保留 raw Database 字段；无 schema/IPC 变化（ADR 0295）。

阶段 3 历史切片补充（2026-09-24，ADR 0296；后由 ADR 0299 移除 ReActEngine raw Database ownership）：ReAct durable replay state 读取、transcript seed 与单条 transcript 追加通过 SessionStore blocking-pool ports；Agent 保留 record 解析、ReActState 投影与解析错误语义，单条追加缺失 session 仍返回 sequence `0`，校验失败不产生 durable/live event 副作用。compaction summary、branch/rollback/recovery 的其他路径不变；该切片当时记录 ReActEngine 因剩余路径仍使用 raw Database 而保留该字段。

阶段 3/2 历史切片补充（2026-09-25，ADR 0297；AgentLayer/ReActEngine raw Database ownership 后由 ADR 0299 收口）：rollback target 精确读取、`rollback_to` 整体事务（含 replacement transcript）与 continue recovery projection 截断改由 SessionStore 异步端口在 blocking pool 调度。Agent 保留 lifecycle cancel/join、replay 与边界策略、事件/branch trimming、tools restore、usage invalidation 和 status 更新；事务失败时成功后置步骤不运行。三条路径继续使用不可取消的 `run_blocking`，`rollback.rs` 不再自行调度 raw Database；该切片当时记录 AgentLayer、ReActEngine 其他职责的 raw Database 仍在，resume attachments、compaction summary、Tools/UI 不在本切片。

阶段 7 的 MemoryRuntime committed-event 消费设计已由 ADR 0259 采纳；Phase 7.1 已完成 SessionStore 独立 event cursor/有界 durable replay/生命周期清理（ADR 0261）、按序处理核心、启动时已有 cursor 回放、bounded live/replay recovery runner，以及 AgentLayer composition/dispatcher recovery readiness barrier（ADR 0262、0263）。interval、pause hook trigger、compact-summary episode extraction 与周期 maintenance 调度已经由 typed intent/atomic episode write + durable producer/outbox/runtime schedule 接管（ADR 0264、0265、0266、0267）。Agent prompt/recall 查询现由 `MemoryRecallStore` 收口（ADR 0304）；ActionService 的持久化接口现由 `ActionStore` 收口（ADR 0305）；本段是早期设计快照，MemoryWorker 生产 raw Database 路径后由 ADR 0312 收口（见阶段 3 当前状态）；本设计不改变 recall 或 rollback facts 语义。

早期补充切片记录：阶段 3 的 transcript batch writer 已在 `7775e11` 进一步只依赖 `SessionStore`（ADR 0255），Ingress/recovery 消息路径、失败会话 action-step 清理与生产会话创建也已通过 SessionStore typed port 收口（ADR 0256、0258、0260）；阶段 7 的无调用 recall 转发已在本轮删除（ADR 0254），compaction-summary extraction 已完成 per-episode durable marker/outbox（ADR 0266），周期 maintenance 的调度策略已归 MemoryRuntime（ADR 0267），Agent prompt/recall 查询已通过 MemoryRecallStore 收口（ADR 0304）。这些切片不改变 facts/recall/rollback 语义；完整 Job lifecycle 仍未完成，当前未决项见 Phase 7.1 验收与未决风险。MemoryWorker 生产 raw Database 路径已由 ADR 0312 收口。

阶段 3 历史切片补充（2026-09-25，ADR 0298；ReActEngine raw Database ownership 后由 ADR 0299 移除）：ReAct pause/continue/error 共用的只读 event-boundary cursor 检查经 `SessionStore` 异步端口调度，Memory 复用 `load_replay_state`，保留可选 cancellation 与原 `run_blocking_cancellable`/`run_blocking` 分支；Agent 保留 boundary 返回值、warning 与指标语义，无投影或事件写入。该切片当时记录 `ReActEngine.db` 仍由 compaction-summary episode 写入使用，并在 summary 达到既有长度阈值时写 extraction marker。

阶段 3 历史切片补充（2026-09-25，ADR 0299、0300；MemoryWorker 生产 raw Database 路径后由 ADR 0312 收口）：compaction summary episode 与 pending extraction marker 通过 MemoryStore 调度既有原子事务；ReActEngine 继续拥有 trim/empty/长度门槛和持久化成功后的 wake 顺序，MemoryWorker 继续拥有 extraction live outbox。ReActEngine 的持久化边界改为只依赖 SessionStore + MemoryStore，AgentLayer 删除直接持有的 raw Database 字段；当时 MemoryService 和 MemoryWorker 其他既有 DB 路径不在该切片迁移。MemoryService 私有 backing Database 后续由 ADR 0383 删除，生产实例只接收 typed stores。

阶段 3/7 小切片补充（2026-09-25）：fact/summary durable outbox marker 的 enqueue、pending restore、条件 ack 与 summary episode read 经 MemoryService 共享的 MemoryStore 执行；MemoryWorker 继续负责 live projection、inference 和逐 job 退避。ack 失败时 durable marker 保留并重试，停机取消不提前确认。fact extraction 算法和其他维护路径保持不变（ADR 0301）。

阶段 3/7 小切片补充（2026-09-25）：MemoryWorker 的已知事实上下文通过 MemoryService 持有并注入的 MemoryFactStore 有界读取；Memory 层先执行可见性过滤，再保留有效置信度顺序并截断，Agent 的 prompt 行格式、subject 前缀、sanitize 和错误降级保持不变。其他 MemoryWorker Database 路径不迁移（ADR 0307）。

阶段 3/7 小切片补充（2026-09-25）：普通 session fact extraction 经 MemoryFactExtractionStore 读取消息/步骤投影、读取与写入节流时间戳、读取与推进用户消息 cursor；窗口构造、模型调用、事实持久化策略与取消后保留 durable outbox marker 的语义不变。该切片完成时事实批量候选校验/写入随后经 MemoryFactStore 完成，summary episode cursor/共享节流与维护路径仍未迁移；事实写入及确定性维护由 ADR 0309/0310 接续，LLM maintenance 由 ADR 0311 接续。此后 summary cursor/throttle 已由 ADR 0312 收口；当前生产 MemoryWorker 不再使用 raw Database，embedding catch-up 继续通过 MemoryService 的 MemoryEmbeddingStore。

阶段 3/2 历史切片补充（2026-09-25，ADR 0300；MemoryWorker 生产 raw Database 路径后由 ADR 0312 收口）：resume 的初始消息 id、attachments、`media_inputs` 与 session 全量 attachments 由 `SessionStore::session_resume_media` 在一个 blocking closure 中读取；保留消息顺序、首个 user 选择、空媒体与错误映射。Agent 继续负责 canonical initial input、fresh-run/resume 分界和 managed asset lease/register，事件流仍是恢复 authority；该切片当时未迁移 memory_index/MemoryWorker 其他 Database 路径，后者现经 typed stores，embedding index 由 MemoryService 持有 MemoryEmbeddingStore。

阶段 3/1 小切片补充（2026-09-25，search-final 后续收口见 ADR 0385）：ReAct 不再经 Agent `TranscriptBatchWriter` 提交 storage-shaped batch；`SessionCommitted` 承载事件和领域投影意图，SessionStore 在同一事务内事件优先、投影失败整体回滚，提交后才广播及按 sequence 发布 UI（ADR 0336）。保留的直接消息路径是 ingress seed、error partial、terminal action-result 和当时存在的 UI-only ask/confirm notice；该旁路已于 2026-10-02 移除。搜索合成 final 的消息投影现与 owning ToolCall event 同事务提交；其他新增可恢复 transcript 文本必须使用 `SessionCommitted`。

## 3. 不变量与禁止事项

### 3.1 不变量

- session-local 可变运行态只能由 actor 任务修改；supervisor 只负责 registry、admission、生命周期和 handle。
- 一次 run 在 actor 任务内执行；`SessionState::react_run` 持有 active future，future 独占 run-local `ReActState`，由 actor loop 与 mailbox 一起轮询（ADR 0214、0382）。
- active run future 不借用整份 `SessionState`；工具/模型/数据库等待期间，外部命令仍可更新队列、交互和生命周期状态。
- ReAct live transcript 的 Agent 提交 `SessionCommitted`；SessionStore 在同一事务内先追加 session events、再更新物化投影、提交后广播。Agent 再按已提交 sequence 发布 committed UI event，随后更新 in-memory canonical；assistant Thought 的共享 ID step projection 可在发布后单独写入并由 replay 修复。
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

### 2026-09-29 阶段状态复核

阶段 1–8 标记为**已完成**；阶段 9 的发布验收保持开放。阶段完成表示相应阶段已采纳的核心所有权/契约边界落地并有对应回归记录，不代表全局重构已经完成。本次阶段 7 的 dependency recovery 与 terminal transcript 边界见 ADR 0392、0393；阶段 8 的 IPC codegen 与 UI owner 收口见 ADR 0394。阶段 3 的全仓 typed-output 契约审计见 `architecture-output-contract-inventory.md`。

| 阶段 | 状态 | 完成范围与保留项 |
|---|---|---|
| 1：SessionActor 热运行态 owner | 已完成 | Actor task 单一拥有 `SessionState`；`SessionState::react_run` 持有 active run future，future 独占捕获 run-local `ReActState`，同一 actor loop select future 与 mailbox。pending provider/tool/storage await 不借用整份 `SessionState`；删除活动 session 的取消、run-exit join、durable row 删除顺序有 supervisor 集成回归（ADR 0214、0382）。 |
| 2：恢复、回滚与事件/投影边界 | 已完成 | `session_events` 成为恢复权威；rollback/projection cutoff 与已知 ingress/recovery 例外有明确 owner 和回归覆盖。既有两天 unanchored-user 扫描改为 `pending_session_inputs`：用户输入落库时原子建 marker，`UserInject` event 提交时原子 ack，恢复不受停机时长影响（ADR 0416）。search-final messages projection 并入 owning ToolCall commit，不再有 ReAct 内容行的直接投影旁路（ADR 0385）。阶段内核心恢复/回滚验证已完成；全新 profile 下的完整应用恢复/发布验收仍归阶段 9。事件保留和容量上限是阶段 9 的独立后续决策，不承诺永久保留（ADR 0389）。 |
| 3：存储 domain ports 与 typed projection | 已完成 | Session/Memory/Usage/Action typed-store 迁移及 App cleanup 路径收口完成。App composition root 创建 typed memory stores；生产 Agent 路径不把 raw `Database` 传入 `MemoryService`。ActionService 稳定 status/list projection 由 typed views 暴露；该阶段列明的 JSON 边界按 ADR 0383 处理，全仓 typed-output 审计仍由全局验收跟踪。 |
| 4：Tools capability runtime 与 Agent ports | 已完成 | `AgentLayer` 与 `SessionSupervisor` 接收显式端口 bundle；session runner 通过完整 `ToolExecutionPort` 与 `ToolExecutionContext` 提交 session/tool/input/cancel/step identity，`ToolAuthorizationPort` 单独准备 live policy request。prompt/catalog/execution/observation/overlay/asset 的 manager adapters 由 app composition root 创建；生产 Agent runtime 不依赖具体 `ToolsManager`。执行前授权与 turn catalog snapshot 语义不变（ADR 0374、0384、0388）。 |
| 5：Runtime config apply ownership | 已完成 | ConfigService、Settings/model/Tools 同域写入 gate、phase planning 与 durable-first 部分失败语义已落地；普通会话/工具在重启前继续使用各自 live runtime，允许不同 consumer revision，回归验收要求见 ADR 0372；显式 retry、补偿/rollback 与 auto-restart 不属于当前契约。 |
| 6：LLM Router 请求对象化 | 已完成 | 请求对象、执行器拆分、descriptor/capability 映射与 usage owner 契约已由 ADR 0354 最终审计关闭。 |
| 7：Job 生命周期与 MemoryRuntime | 已完成 | MemoryRuntime 应用级所有权、scheduled dependency durable relation/result 与启动 watcher 恢复已落地；scheduled tool completed/failed 通过共享 ActionCompletion outbox、Agent transcript 投影和 ack 交付。跨 kind owner/策略边界见 ADR 0393；完整通用 executor、统一 action deadline、owner token/续租与自动 replay 是明确非目标（ADR 0392、0393）。 |
| 8：IPC 单源生成与 UI 编排 | 已完成 | Rust handlers/Serde DTO 生成 TS request/response contracts；command drift checks、ask/input、startup recovery 与复杂 view/scroll owners 已收口（ADR 0394）。 |
| 9：Common、性能与发布验收 | 未完成 | 2026-09-29 完成 Windows 磁盘 SQLite 容量曲线、retention/WAL/VACUUM 行为、`SQLITE_FULL` 原子失败/释放后重试、隔离 APPDATA 首启 bootstrap 和旧 schema 拒绝/按文档重置测试；合成 payload 不替代实际数据分布和物理盘满测试。真实 GUI 交互、安装升级/卸载仍未验收；当前 host 无可操作 CUA 窗口且缺少 NSIS 构建工具（ADR 0395）。Common 拆分与性能优化按实测数据决定。 |

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

### 阶段 1（已完成）：SessionActor 成为唯一热运行态 owner（P0）

目的：落实 ADR 0214，不再让 `resume.rs`/`ReActEngine` 维护第二份 session-local 运行态。

目标结构：

```text
SessionSupervisor
  registry / admission / lifecycle / handles

SessionActor
  SessionState { queues, interaction, runtime, react_run: Option<ActiveReactRun> }
  ActiveReactRun future { owns run-local ReActState / hot transcript }
  actor task { mailbox select + SessionState.react_run future }

TurnEngine / ReAct loop
  run-local progression over captured ReActState; actor retains mailbox access while future awaits

SessionStore
  event append + projection + committed publication boundary
```

工作项：

- 把一次 run 的启动、恢复和退出驱动迁入 actor task；
- 由 `SessionState::react_run` 持有 active run future；future 独占捕获热 transcript、canonical、branch points 与 retry nudge；Actor 生命周期 token 与逐 run 的 cancellation token 分开，dispatcher claim / direct run admission 时创建新的 run token；
- 删除 `EnsureStreamId`、`RecordUsage`、`EstimateTokens`、`AppendTokenEstimate`、`ResetTokenEstimate` 等内部 mailbox 命令；
- `next_run_id` 在 actor 内递增；usage 由 `UsageRuntime` 持有，stream identity 与 token estimate 由 run-local `ReActState` 持有；这些内部状态不通过 actor mailbox 往返（ADR 0219–0223、0276、0278）；
- 将外部命令限制为 Submit、Steer、ResolveInteraction、Cancel、BackgroundResult、生命周期/快照/消息交互；
- 让 actor 在 provider/tool/timer await 期间 `select!` 外部 mailbox；active run future 不借用整个 `SessionState`，因此 mailbox 可在 future pending 时更新队列与生命周期字段；
- 删除 `resume.rs` 中直接构造 `ReActState` 的生产路径，保留纯 replay/project helper；
- 保持 X12、confirm/ask、tool order、LoopExit、partial generation 和 committed UI sequence 语义不变。

主要文件：`crates/agent/src/session/actor.rs`、`dispatcher.rs`、`resume.rs`、`react/loop.rs`、`react/turn.rs`、`react/context.rs`、相关 integration tests。

验收：actor 运行期间 submit/steer/cancel 不阻塞；`SessionState::react_run` 独占持有 active run 与 run-local 热状态；两个 session 的热 transcript 不共享；resume/rollback/compaction 后不存在旧 generation 命中。对应 actor 回归见 ADR 0382。

回滚：单提交回滚；不改变数据库 schema、IPC 或用户数据。若迁移不完整，必须整体回退 run 所在任务、SessionState 字段和 mailbox 命令，不能保留半套。

### 阶段 2（已完成）：恢复、回滚和事件/投影边界再收口（P0）

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

事件存储增长是恢复契约之外的后续容量议题，不重开阶段 2 的完成状态：当前事件随 session 保留，不按 compaction root 裁剪；既有 whole-session retention 默认 90 天且可设为 0 禁用，不构成字节上限。阶段 9 的存储设计边界见 ADR 0389。

### 阶段 3（已完成）：存储 domain ports 与 typed projection（P1）

目的：把过宽 `Database` facade 和稳定业务 JSON 接口收窄。

工作项：

- 在 `haven-memory` 内建立 `SessionStore`、`TranscriptStore`、`UsageStore`、`ActionStore` 的最小公开面；
- 逐步删除 Agent/Tools/App 的 `Arc<Database>` 传递与上层 `conn()` 使用；
- 统一 `persist_llm_call(session_id, LlmCallUsageInput)`；cache accounting 与 `llm_usage.call_kind` 运行时输入已完成 typed enum 收口（ADR 0270–0271），其他 call purpose 表达另行收敛；
- 将 ActionService 的 board/status/list 输出收敛为 `ActionView`、`ActionKind`、`ActionState`、`ActionOutput` 等 DTO；动态 tool args/MCP payload 仍保留 `Value`；
- 保留 projection/cache invalidation/事务细节在 Memory 内部；
- 为每个删除的旧 Database API 删除无调用测试和旧文档入口。

本轮（ADR 0374）已完成：启动恢复、一次性 retention、上传引用和每日 cleanup task 不再捕获
raw `Database`，而是使用 `SessionStore` typed ports；AgentLayer 从组合根显式接收 `ToolsManager`，
SessionSupervisor 仅向 Agent 内部提供 authorization/action 窄 capability。保留的执行 facade、
prompt/catalog adapters 在 ADR 0374 时仍按阶段 4 跟踪，后由 ADR 0388 收口。阶段 3 已列范围验收（ADR 0383）进一步将 memory typed stores 的创建移至 App composition root；生产 `MemoryService` 不接收 raw `Database`，Agent/Tools 的生产路径不持有上层 raw `Database`。ActionService 的稳定 status/list API 现为 typed projection。以上不代表全仓 stable-output DTO 与 JSON 边界审计完成；该审计状态见全局验收表。

主要文件：`crates/memory/src/` repositories/database、`crates/agent/src/` usage/session、`crates/tools/src/action_service.rs`、`crates/app-binary/src/commands/`。

验收：除 App composition root 创建 typed stores 外，上层生产业务路径不持有 raw `Database` 或取得 raw connection；稳定 Action status/list 跨层返回值为 typed projection；本阶段涉及的 JSON 仅用于明确的 tool/event/provider/MCP/Skill wire 或动态 schema/payload 边界。内存数据库测试覆盖事务、回滚和空结果；阶段 3 范围验收见 ADR 0383，全仓 typed-output 审计状态见全局验收表。

### 阶段 4（已完成）：Tools capability runtime 与 Agent ports（P1）

目的：让 Agent 依赖能力接口，而不是理解 ToolsManager 内部服务结构。

工作项：

- 基于现有 `ToolServices`、`OperationRegistry`、`AuthorizedExecutor`、`PlatformRuntime`，为一次执行引入 `ToolExecutionContext`；
- Agent 只依赖最小的 operation catalog/executor、authorization、memory/session 读取端口；
- 删除 Agent 中 `executor.get_tools()`、逐个 setter/bind 和服务 locator getter；
- 不为每个内部服务制造 trait；只有跨 crate、需要替换/测试的能力才进入 ports；
- 将运行时 snapshot 整体替换，避免 `RwLock<Option<Arc<_>>>` 半初始化状态；
- 保留交互式确认在执行前，补安全负向测试。

主要文件：`crates/tools/src/manager.rs`、`tool_runtime.rs`、`execution.rs`、`registry.rs`、`crates/agent/src/layer.rs`、`session/`、`crates/app-binary/src/runtime.rs`。

验收：Agent runtime owners 不依赖 ToolsManager 或其 service container；只在 composition boundary 创建 manager adapters；启动后执行能力 bundle 完整注入；工具目录 snapshot、执行前授权、取消、step identity 和 timeout 行为不变。执行/授权、prompt/catalog/observation、overlay 与 asset lease adapters 现由 app composition root 创建；落实见 ADR 0384、0388。

2026-09-25 ToolsManager façade 审计（ADR 0345）：当前源码入口是 `manager.rs`、`execution.rs` 与 `catalog.rs`。授权请求构造曾与 `AuthorizedExecutor` 共处，且 live 与 snapshot lookup 各自维护未知工具策略 fallback；现由 crate-private `ToolAuthorizationPolicy` 准备 typed request，`AuthorizationEngine` 仍由调用方实时评估，交互确认仍先于执行。session overlay 的 mutable registrations/version 仅由 `SessionCatalog` 持有；catalog snapshot 是其不可变 turn 投影。asset metadata/pending/session leases 由同一个 `ManagedAssetRegistry` 状态持有，`ToolServices` 与 builtin/manager 克隆共享其内部状态。故不把 overlay、lease 或 projection 再拆为并列 owner。该审计提出的 context/Agent ports 由 ADR 0384、0388 分阶段收口。

### 阶段 5（已完成）：Runtime config apply ownership（P1）

目的：明确不同配置入口的 edit owner 与 runtime apply owner，并由 typed plan 唯一决定阶段依赖顺序。

工作项：

- Settings 使用独立的 typed `SettingsApplyPlan` / `SettingsRuntimeApplyCoordinator` 计算 runtime targets 与有序 phases；共享 `RuntimeConfigCoordinator` 继续拥有 gate 以及 Router/media prepare→publish；
- model mutation 的 durable edit、Router target 与完整 prepare→publish 已由 `RuntimeConfigCoordinator` 拥有（ADR 0323）；
- 先校验和持久化 snapshot，再按模型/媒体/工具/MCP/日志/hotkey 依赖应用；
- 已确定不做整体 runtime snapshot 原子替换、补偿或回滚；apply 失败保留已完成及失败 phase 的实际副作用，普通会话/工具继续按 live owner 状态运行到用户重启（ADR 0351、0372）；
- 将 phase/failure 观测归 Settings coordinator，命令只提供既有 runtime owner 的 phase 执行回调；命令保持 `Result<T, String>`；
- 增加应用顺序、半失败、失败元数据和敏感字段不泄漏测试；补偿/回滚测试待策略确定后新增。

主要文件：`crates/common/src/config/service.rs`、`crates/app-binary/src/commands/settings.rs`、`runtime.rs`、`bootstrap.rs`、MCP/tools wiring。

验收：settings command 不再持有 phase target/order/failure tracker；Router/media 从同一 committed snapshot prepare→publish，其他副作用保持依赖顺序，失败路径可诊断。

当前边界：model 命令的完整 apply 路径已统一；Settings 的 target/phase plan、执行顺序与失败观测已归 coordinator。security/MCP/context/logging/hotkey 等实际副作用仍由既有 owner 执行，并按原顺序半失败；失败记录不代表补偿成功。已确认并实现 durable-first 半应用失败语义：不新增 compensation/rollback/显式 retry，重启从磁盘配置初始化；重启前普通会话与工具调用继续使用各自 owner 的 live 状态，不等待重启，也不保证所有 consumer 处于同一 config revision；SkillsExec-only 标记重启并跳过 live Skills phase，混合 Skills 变更仍走既有 live phase（ADR 0351、0372）。

阶段 5 重启前运行验收要求（ADR 0372）：注入 Router prepare 失败和较晚 phase 失败，分别验证普通会话与普通工具操作在失败返回后、用户重启前仍可继续；验证之前成功或失败 phase 的实际副作用保留、失败后的 phase 停止、durable config 保留且同一 payload 不重试。还需验证已有 turn 使用其不可变 tool catalog snapshot、新 turn 读取当前 live catalog、authorization 与执行遵循各自 live owner；接受这些 consumer 暂时对应不同 config revision，不以全局版本一致作为通过条件。重启恢复从 durable config 初始化另行验收。

2026-09-25 失败策略审计（ADR 0351）：`SettingsApplyOutcome` / `SettingsApplyObservation` 已提供 typed phase outcome 与诊断上下文；target 复用 `RuntimeConfigApplyPlan`，phase 顺序只有 `SETTINGS_APPLY_PHASE_ORDER` 一份，因此不再抽取通用 failure report/plan validator。新增回归固定 Settings/model apply 失败后 durable edit 保留、相同输入不隐式重试、每个可传播 fatal 的 Settings phase 停止后续调用；warning-only event 保持独立测试。审计发现 Tools admin 的 MCP、Skills、Logging、ToolSettings 写入另有 runtime apply/局部补偿路径，且不持有 `config_apply_gate`；ConfigService 锁不覆盖 save 后 runtime apply，跨入口可能并发。是否收敛这些 writer、是否允许部分应用、显式重试/重启与补偿边界仍需产品/架构决策；本切片保持现有行为。

### 阶段 6（已完成）：LLM Router 请求对象化（P2）

目的：减少 router 公共包装入口，不改变 provider adapter。

已完成的请求对象切片：普通与 tools 完整请求统一为 `CompleteRequest`（ADR 0234）；聚合 streaming 主入口使用借用式 `StreamRequest`，cancel/hooks 作为独立执行控制（ADR 0241）；embedding 与 health_check 主入口使用 `EmbeddingRequest`、`HealthCheckRequest`（ADR 0242）。Router chat 转发入口及仓库内调用点已删除，不提供兼容别名。请求对象只承载路由数据；Router 保留路由、配置 snapshot、permit、熔断、规则与 health/cooldown 状态，内部执行器复用既有策略与结果投影入口。聚合 stream 的取消豁免保持单独分支（ADR 0246）。

工作项：

- 已完成第一步（ADR 0319）：在 ModelDirectory route filtering 使用 crate-private `RequestDescriptor` 显式并列逻辑用途 `RequestKind` 与 provider/model `Capability`；`LlmCallKind` 继续作为独立 usage owner。`RequestKind` 仍保留原配置/route key 与序列化形式；
- 已完成（ADR 0316）：crate-private `ModelDirectory` 拥有 provider client map、primary route 表、按 request 选择/解析 client 与 model id、configured route 判定及 capability/endpoint/context-window metadata lookup；Router 的 config snapshot 是唯一配置真源，health/circuit/rate-limit/semaphore、stream rules、retry/timeout/usage/cancellation 和执行编排继续由 Router 持有；
- 已完成（ADR 0318）：crate-private CallExecutor 接管已解析 client/model 的 plain/tools complete 和非空 embedding 的内容校验、现有 retry/total timeout 与 Router outcome 投影调用；Router 仍决定路由并持有全部可变运行态，empty embedding 仍先于 route/permit 快返。
- 已完成第一刀（ADR 0327）：crate-private `StreamExecutor` 只接管 raw `chat_stream` 的 validate、既有 retry/total timeout、最终 outcome closure 投影与 `PermitStream` 包装；Router 仍拥有 route/client 选择、permit acquisition/cooldown、circuit、config snapshot 和健康状态。retry 仅覆盖建流，permit 保持到返回 stream drop。
- 已完成第二刀（ADR 0328）：crate-private `AggregatedStreamExecutor` 接管聚合流的共享 `StreamContext`、首次 `on_chunk` 交付前重试、StreamRule guidance retry、聚合总 timeout 与 final outcome handoff；`streaming.rs` 继续负责单条 provider stream 消费/聚合。Router 保留 client/model 选择、两处配置读取时点、permit 生命周期、stream rule 状态及 health/cooldown 投影；Cancelled 仍跳过 health failure。
- 已完成（ADR 0329）：Router 将 `RequestDescriptor` 贯穿到 CallExecutor 的 complete/embedding、raw StreamExecutor 和 AggregatedStreamExecutor；ModelDirectory 仍以 `RequestKind` 为 route key，并拒绝与 primary route descriptor 不一致的执行请求。
- 审计结论（ADR 0339）：`health_check(HealthCheckRequest)` 与 native transcription 已在 Router route/permit boundary 由同一个 `RequestDescriptor::from(RequestKind)` 进入 capability 过滤；health adapter 和已选 client 的 native `transcribe` 之后不再推断能力。native STT 的 `UnsupportedCapability` fallback 重新走独立 `AudioChat` route。无需再包一层 executor 或把 descriptor 暴露给 provider adapter。
- metadata/config helpers 保持 `RequestKind` 有意为之：它们从 Router 唯一的 `RouterConfig` snapshot 调用 `RouterConfig::route` 或读取 adapter 本地 capability profile，不发 provider LLM 请求，也不投影 usage/health。`connection_status` 与 `prewarm_all` 是独立的显式 health probe，不属于只读 metadata helper。
- 已完成最终契约审计（ADR 0354）：`RequestKind` 已同时表达当前逻辑请求和配置 route key；`RequestDescriptor::purpose` 是同一 route purpose 的执行期承载，不需要平行 public `CallPurpose`。唯一 capability 映射仍为 `RequestKind::required_capability()`。Agent/Tools 在 owner 已知处显式设置 `LlmCallKind`，Memory 独立保存 route kind 与 owner；Router 不从 route/capability 推断 owner。移除 `PrimaryRoute` value 内冗余的 purpose 副本，public DTO 与 route key 兼容性、fail-closed capability 和 health/circuit/rate-limit/usage 行为不变。Phase 6 请求契约审计项关闭。
- 删除只转发参数的 chat/chat_request/output_cap/stream wrapper；
- provider adapter 只做 wire mapping，保持 golden fixture。

主要文件：`crates/llm/src/router.rs`、`model_directory.rs`、`types.rs`、`request_pipeline.rs`、`streaming.rs`、Agent/Tools 调用点。

验收：四类能力各有 request object 和负向 capability 测试；ModelDirectory 覆盖生产 credential/capability filtering、注入 route filtering、无 route、shared model identity 与 endpoint/context-window metadata；provider wire 与 usage/stream 契约不变；无重复 retry/usage 入口。

2026-09-25 切片进展（ADR 0316）：ModelDirectory 已接管模型 client 与 primary route 目录，并按生产/注入构造保持对应 credential 与 capability 过滤；endpoint/context-window metadata 通过借用 Router 的单一 config snapshot 查询。Router 仍拥有所有执行状态和策略。

2026-09-25 历史切片记录（ADR 0318，当时尚未完成 raw/aggregated streaming execution ownership 与 RequestKind/capability/usage-role 契约；raw/aggregated executors 后由 ADR 0327、0328 完成，契约后由 ADR 0354 关闭）：crate-private CallExecutor 只接收 Router 已解析的 model identity/client 与单一 RequestPolicy，接管 plain/tools complete 和非空 embedding 的校验、retry、总 timeout 及通过 Router 闭包完成的 health/rate-limit outcome 投影；permit wrapper 只负责并发与 cooldown 等待，避免同一 429 cooldown 重复投影。embedding empty-input 仍在路由前快返。未完成：raw/aggregated streaming execution ownership，以及 RequestKind 的 capability/call-purpose/UI usage role split。无配置、持久化或 wire 重置要求。

2026-09-25 历史切片记录（ADR 0319，当时 descriptor 仍待贯穿其他执行入口；后续见 ADR 0329、0339、0354）：ModelDirectory 构造 primary routes 时以 `RequestDescriptor` 将逻辑用途/原 route key 与显式 `Capability` 分开；production 仍要求凭据及能力匹配，注入 route 仍要求能力匹配。完整请求继续通过原 `RequestKind` 选配置和模型；配置字符串与 `LlmCallKind` usage owner 不变。此为语义类型第一步；完整 descriptor 向其他 Router 请求 DTO、streaming、embedding/health-check、metadata/config helpers 和仓库调用点迁移仍待独立评估。

2026-09-25 历史切片记录（ADR 0327；聚合流执行边界后由 ADR 0328 完成，purpose/usage-role 契约后由 ADR 0354 关闭）：raw stream 建流执行已迁入 `StreamExecutor`，复用 `request_pipeline` retry/timeout 和 Router outcome closure；permit 包装仍覆盖 stream 完整对象生命周期。聚合 streaming retry/guidance/cancellation/callback orchestration 保留在原 Router/`streaming.rs` 路径；aggregated stream executor 与 descriptor 全贯穿仍待后续切片。

2026-09-25 切片进展（ADR 0329）：`RequestDescriptor` 从 ModelDirectory route table 解析一路传入 complete/embedding、raw stream 与 aggregated stream executor；route key 仍为原 `RequestKind`，能力不匹配或 route descriptor 不一致时 fail closed。descriptor mapping 继续委托唯一的 `RequestKind::required_capability()`；usage role 仍由 Agent/Tools 调用方所有。

2026-09-25 审计进展（ADR 0339）：health check 和 native transcription 已在 Router 执行路由边界使用同一 descriptor capability mapping；metadata/config helpers 仅做配置或本地 adapter profile 查询，测试确认不会产生 LLM/health call、usage 修改或 health/cooldown 投影。因此这轮不新增语义 wrapper。剩余工作仅是未来是否拆分 public DTO route key 与独立 call-purpose 类型，以及是否需要由 Agent/Tools 显式传递 usage role；本轮保持这些公开/调用方契约不变。

2026-09-25 Phase 6 最终契约审计（ADR 0354）：确认 `RequestKind`、`RequestDescriptor::purpose` 和能力映射并非多份策略真源；route key 与 descriptor purpose 复用同一请求事实，capability 由唯一映射生成。`LlmCallKind` 是正交 usage owner；同一 Chat route 可记录为 Tool 或 Media。删除 route value 重复存储的 purpose，加入 usage owner 正交性断言；不引入 public `CallPurpose` 或 Router usage-role 参数。Phase 6 的请求 purpose/usage-role 未决项关闭。

### 阶段 7：统一 Job 生命周期与 MemoryRuntime（P2）

目的：减少后台/定时任务重复状态，并把记忆后台编排移出 Agent ReAct。

工作项：

- 在现有 `actions` 表和状态模型上把 trigger 与 execution 分开，后台任务是 `Immediate` trigger，定时任务是 `At/After` trigger；
- 统一 claim、cancel、timeout、retry、tail output、completion outbox 和 UI projection；
- messaging 不并入 Job，仍是独立 transport domain；
- `MemoryRuntime` 监听 committed session event，负责 fact extraction/maintenance/index catch-up；
- `MemoryWorker` 的 FastChat 调用已通过小型 `MemoryInferencePort` 注入（ADR 0247）；Agent recall 查询已由 `MemoryRecallStore` 提供（ADR 0304）；ordinary/summary 事实抽取状态、批量写入与确定性/LLM maintenance persistence 已通过专用 stores 收口（ADR 0308–0312）。ReAct live transcript 提交已由 ADR 0336 收口；embedding catch-up 继续由 `MemoryService` 提供。

状态：committed-event consumer 架构设计已完成并采纳（ADR 0259）；Phase 7.1 的 SessionStore cursor/replay、`MemoryRuntime::process_event` 顺序处理、启动时已有 cursor 回放和 `run_until_cancelled` bounded live/replay recovery 已实现，并由 AgentLayer 在 dispatcher recovery 前装配与启动（ADR 0261、0262、0263）。interval、pause trigger 与 compact-summary extraction 已通过 durable producer/outbox 接入，周期 maintenance 调度已由 MemoryRuntime 负责；MemoryWorker durable outbox 已加入逐 job retry/backoff 和应用停机 cancellation boundary，marker 持久化与读取/ack 现归 MemoryStore（ADR 0264、0265、0266、0267、0268、0269、0301）。ActionService 的全部 action 持久化经 ActionStore 调度，后台终态与 outbox 仍由同一事务提交，transcript durable 后才 ack（ADR 0305）。Phase 7 终态内核切片（ADR 0317）让后台与 scheduled 共用终态构造、时间戳语义、认领判定和进程内提交 guard；各自 CAS/outbox、重试、process kill、timer/consumer 回滚和事件顺序仍分开。ADR 0321 让两类按 session 清理共用 typed live-owned action 选择与串行遍历；background-only 与 explicit full cancellation 的范围、family-specific callback、background terminal board cleanup 和各自错误处理顺序保持原样。ADR 0325 将 completion DTO、receiver、broadcast 与 scheduled pending-fire claim/lease recovery 收至 crate-private `action_completion` transport；ADR 0332 又让 background outbox claim 与 scheduled fire claim 共用纯 typed `ActionLease<T>` 决策核心，同时保留 SQLite `BEGIN IMMEDIATE`/CAS、30 秒 durable outbox lease、15 分钟 scheduled in-process lease、terminal/outbox ack 与 timer rollback 边界。ADR 0334 将 background/scheduled 终态持久化修复的 deadline/attempt/backoff/stop decision 收至纯 typed `ActionPersistenceRetryPolicy`；生产路径仍无 deadline 或 retry budget，scheduled 每次 store 操作的 3 次/50 ms 重试、CAS/outbox/timer rollback 和 Agent completion delivery retry 各留原 owner。ADR 0338 收口 tail 长度策略与 bounded snapshot，ADR 0343 抽取 scheduled trigger 输入 policy，ADR 0344 审计 UI projection；三者均保留 kind-specific 执行、完成副作用和 owner。两类 claim 现有身份仍只是稳定的 `action_result_id` / `action_id`，没有独立 claimant owner token 或续租操作。ADR 0352 穷举状态转换与 terminal claim 矩阵后未发现可再抽取的跨 kind 纯转换判断：background admission 直接为 running，scheduled 独占 waiting→running；终态前后的重复检查处在不同 CAS/投影边界。审计另发现 `ActionService::set` 的 scheduled admission cleanup 谓词会从内存 registry 移除 Running entry（注释只描述 terminal cleanup）；该问题已由 ADR 0353 修复为只回收 terminal entry，保留 Running row 供 Agent terminal callback 使用。完整 Job 模型仍需处理 trigger/execution 分离、timeout/claim identity/恢复等边界；dependency-waiting 的 durable relation 与 watcher restart 语义已确认（ADR 0375），但尚待实现。本阶段不新增 action-level timeout、owner token/lease renewal、自动 replay 或新 Job 状态语义。MemoryRuntime 的 committed-event/durable outbox 仍是独立 lifecycle（ADR 0259）。usage runtime 的 cache accounting 与 `llm_usage.call_kind` 运行时输入均已完成 typed input 收口（ADR 0270、0271）；durable read model 和 IPC 继续使用既有字符串字段。

MemoryRuntime 启动所有权后续校准（ADR 0367）：当前不再使用 `run_until_cancelled` 便利入口。ApplicationRuntime 通过 MemoryStartup 执行 typed prepare、注册一次性 live consumer task，并在注册成功后交出 readiness token；底层 bounded replay 与 durable outbox 行为不变。

主要文件：`crates/tools/src/action_service.rs`、`action_terminal.rs`、`action_lifecycle.rs`、`crates/agent/src/memory_worker.rs`、`memory_service.rs`、`memory_index.rs`、`crates/memory/src/`。

验收：阶段目标原先提出完整 Job lifecycle 与统一 UI projection；截至 ADR 0352，已完成终态内核、session cancellation skeleton、completion transport ownership、跨 kind typed claim/lease core、终态持久化 retry decision、tail policy/snapshot、scheduled trigger policy 和 UI projection 审计。新增状态表测试穷举 `ActionStatus` transition graph 及 terminal claim source/target；审计确认现有可共享纯判断已有唯一 owner，没有安全的全生命周期转换核。background/scheduled terminal UI 副作用按 ADR 0344 保持分开。仍待明确的是 `Immediate`/`At`/`After` 与 execution 的完整分离、是否需要 action-level deadline/claimant identity 与 restart recovery；dependency-waiting 规则已由 ADR 0375 确认，durable relation 和 restart watcher 的实现仍待完成。在这些实现前不引入 action-level timeout、owner token/续租或自动 replay。ADR 0352 发现 `ActionService::set` 会删除 Running scheduled 内存 row；ADR 0353 已修复为只回收 terminal entry，并覆盖 running completion/cancel、terminal 后清理、no-consumer recovery 与 restart handling。ActionLease 单测覆盖 claim success/conflict、expiry/invalidation 和 token mismatch；ActionPersistenceRetryPolicy 单测覆盖 deadline、retryability、attempt/budget、cancel/terminal 与 backoff；outbox 测试覆盖过期 claim 恢复及 result identity ack，ActionService 测试覆盖 background durable retry/outbox publication 与 scheduled terminal retry、跨 receiver 去重、terminal 清除和 no-consumer rollback；ADR 0338 增加 tail 字符截断边界、增量顺序、terminal snapshot、取消清理与 App output event 敏感字段隔离测试；ADR 0343 增加 trigger 互斥、delay/due-at 边界、watch id 归一化、future/horizon 与既有错误语义测试。记忆失败不改变 ReAct turn 结果；重启、重复 outbox、取消和限额有测试。

2026-09-25 历史切片记录（ADR 0332；当时 tail policy 与 UI projection 尚未收口，后续分别见 ADR 0338、0344；完整 Job lifecycle 仍未完成）：background completion outbox 与 scheduled fire recovery 使用同一个纯 `ActionLease<T>` core 判断有效 claim、过期可重新 claim 和身份匹配；background 仍由 SQLite 30 秒 deadline/CAS 与 `action_result_id` ack 恢复，scheduled 仍由共享进程 map、15 分钟单调时钟 lease、`action_id` 及 terminal/no-consumer 清理恢复。未增加 owner token、lease renewal、schema 或 IPC；完整 Job lifecycle 与 timeout/recovery 决策仍待后续阶段。

2026-09-25 历史切片记录（ADR 0334；当时 tail policy 与 UI projection 尚未收口，后续分别见 ADR 0338、0344）：ActionService 两个终态持久化修复 worker 共用纯 `ActionPersistenceRetryPolicy` 决定 deadline、attempt/backoff 与 stop reason；生产 policy 继续无 deadline/预算，退避为 1 秒起步、指数增长、30 秒封顶。scheduled store call 的短重试、background terminal/outbox CAS、Agent durable result projection/ack、claim lease 与各自 rollback 不变。这里没有 action-level execution timeout，也没有自动重放失败 job；provider/LLM 和 Agent tool-call retries 仍属各自请求策略。完整 Job lifecycle 仍未完成。

2026-09-25 切片进展（ADR 0338）：`ActionService` 持有唯一 `ActionOutputPort` tail 字符上限策略，foreground shell card 与 background action 共用只读 tail factory、bounded `ActionOutputTail` 和不可序列化的 `ActionTailSnapshot`；stdout/stderr drain、配置采样时点、增量顺序和两种 UI event identity 保持。App 的 `action:output` adapter 只投影 bounded preview 字段，terminal snapshot 仍由原 `action:finished` / completion 路径发布。取消、terminal commit 和 shutdown 后释放 live tail；receiver lag/closed、durable ack 和 scheduled fire 行为不变。该切片只收敛 tail policy/snapshot，不完成 background/scheduled 全量 UI projection、trigger/execution 分离、action-level timeout、独立 owner token/续租或完整 Job lifecycle。

2026-09-25 历史切片记录（ADR 0343；scheduled UI projection 后由 ADR 0344 审计）：调用图确认 ActionService 承担后台 shell child process 执行，同时拥有 scheduled admission/fire 与两类终态持久化；scheduled fire 实际执行仍由 AgentLayer/tool runner 负责并回报终态。ActionService 的 lifecycle sink 产生 task event，App bootstrap 投影为 ActionEvent；AgentLayer 的 background completion durable transcript/ack 是另一条 Agent 内部投影。`ScheduledTriggerRequest`/`ScheduledTriggerCandidate` 现作为纯 typed policy 独占定时输入归类、UTC due-time/剩余秒计算和配置 horizon 校验输入，ActionService 保留异步配置读取、ActionStore 顺序、内存注册、timer/watch、fire、事件及重试。后台 immediate trigger 尚未与执行器拆分；完整跨 kind lifecycle、timeout/claim identity/recovery 仍待独立契约决策；dependency-waiting 的跨重启语义后由 ADR 0375 确认，当前实现仍为进程内 watcher。

2026-09-25 Action UI projection 审计（ADR 0344）：`actionStore` 已用 action id 作为 background/scheduled 的共同索引；command rows 与 lifecycle events 通过 ADR 0335 的同一 runtime mapper，live-row 容量判定也共用 `waiting/running` 条件。`action:created/updated/output` 均作部分 upsert；refresh generation 与 state-version gate 只保护 hydration 竞态，不构成 event dedup。完成路径仍按 kind 分流：background finished 短暂 upsert terminal payload、调用既有 `finalizeBackgroundActionMessages` 更新绑定工具卡并按原条件 toast；scheduled finished 删除 pending row，Agent 的 `notification:show` 负责通知。Rust bridge 仅对 background 发布 bounded `action:output`；TaskCenter 的状态展示与取消仍依赖 kind。审计修复了 layout 中重复的 background transcript projection，继续复用 `actionStore.ts` 已测试 helper；没有可安全统一的跨 kind terminal reducer，也没有 durable event identity 可用于去重。

2026-09-27 action completion 通知设置（ADR 0373）：`notification.action_completed` 提供默认开启的独立应用内/Windows 开关，background 与 scheduled 共用。Settings、旧配置加载、event marker、UI toast owner 与 DesktopNotifications gate 已贯通；通用 Agent `notification:show` 保持原语义。background toast 仍只在 completed/failed 且 owner session 非当前会话时出现；scheduled completion 按原 title/body 通知。没有改变 action lifecycle、cancel/ack/retry 或 scheduled transcript；scheduled outcome 内容映射仍待决定。

2026-09-25 Action lifecycle transition-core 审计（ADR 0352）：完整 Job lifecycle 没有可安全抽取的另一层纯状态转换核。`ActionStatus::can_transition_to`、`can_claim_terminal`、`ActionLease<T>`、`ActionPersistenceRetryPolicy` 与 scheduled trigger policy 已分别收口。background 从 admission 直接进入 running；scheduled 才有 waiting→running fire。跨 family 的执行副作用、CAS/outbox、无 consumer rollback、terminal retry 和 UI projection 不等价；提交前后重复检查保留为不同边界的竞态校验。穷举状态图/终态准入表测试固定当前契约。完整 Job 模型当时仍等待 trigger/execution、deadline/identity/recovery 与 dependency watcher 的显式决策；dependency-waiting 决策后由 ADR 0375 确认，durable 实现仍待完成。不增加 timeout、owner token/续租、自动 replay、跨重启 watcher 或状态语义。

2026-09-25 scheduled admission cleanup 修复（ADR 0353）：`ActionService::set` 只回收 scheduled terminal 内存 entry，保留 Waiting 和 Running entry；Running action 在新 schedule admission 后仍可由 Agent completion/cancel callback 完成。terminal cleanup 仍在后续 admission 时发生且不删除 durable history；no-consumer rollback/recovery、startup waiting restore 与 stale Running restart failure 语义不变。没有 schema、IPC、CAS/outbox/lease、event order、ID 或 background 行为变化。

2026-09-26 Phase 7.1 验收快照（其 Action lifecycle 未决项由 ADR 0392、0393 于 2026-09-29 更新）：见 ADR 0259、0261、0262、0263、0264、0265、0266、0267、0268、0269、0301、0304、0305、0307、0308、0309、0310、0311、0312、0362、0367。该快照记录已完成持久化端口、按序处理核心、启动回放、bounded live/replay runner、app-owned typed dispatcher readiness handoff、interval/pause producer、summary per-episode durable outbox、MemoryRuntime 的 maintenance schedule policy、MemoryStore marker persistence/read/ack ownership、MemoryWorker retry/backoff、ApplicationRuntime 的 prepare/live/schedule task cancel/join 与 Worker shutdown 接线、Agent recall/query 的 MemoryRecallStore、MemoryWorker known-facts prompt 读取与事实批量写入的 MemoryFactStore、普通 session fact extraction 状态与投影的 MemoryFactExtractionStore、确定性及 LLM maintenance persistence 的 MemoryMaintenanceStore 和 ActionService 的 ActionStore。MemoryRuntime 对象和 startup/live task 的 app ownership 与 dispatcher readiness barrier 已由 ADR 0367 完成；AgentLayer 只接收已注册 consumer 的 MemoryReady，不构造或长期持有 runtime。此前记录的 dependency-waiting durable relation/restart watcher 已由 ADR 0392 实现；通用 Job lifecycle 与 deadline/owner token/lease renewal 等边界由 ADR 0393 确认为非目标。summary extraction 的 episode cursor 与共享 throttle KV 现经 MemoryFactExtractionStore 读写；生产 MemoryWorker raw Database 使用已清零。模型维护 query/write 均经 MemoryMaintenanceStore，embedding catch-up 沿用 MemoryService 的 MemoryEmbeddingStore。

2026-09-29 阶段 7 完成（ADR 0392、0393）：scheduled dependency relationship 与有界 result summary 写入 schema v30；启动时先把遗留 running actions 标为 failed，再恢复 waiting watchers，CAS 保证依赖满足后只触发一次。scheduled tool completed/failed 终态与 outbox 在同一事务提交；outbox reconciliation 可从 action row 恢复丢失投递，Agent 使用稳定 action result ID 和共享不可信 JSON envelope，持久化 transcript 后 ack。Continue 保留原 session input transcript，取消不投影；不新增 action deadline、owner token/lease renewal、通用 executor、自动 replay 或 UI event。Rust fmt/check/clippy/workspace tests 全部通过。
### 阶段 8：IPC 单源生成与 UI 编排收口（P2）

目的：减少 Rust wire DTO、TS contract、mapper、文档之间的重复维护，同时保留一个权威 session store。

工作项：

- Rust wire DTO 作为生成 TypeScript contract/字段映射的唯一来源；保留前端运行时校验；
- 命令数量不为减少复杂度而强行合并；事件按 `session/agent/action/recording/app` envelope 收敛；
- 已完成（ADR 0313）：将 `+page.svelte` 的 session submit、resume reload、rollback、continue、switch/终态内存回收、end/interrupt 编排抽到 typed `ChatController`；
- 已完成（ADR 0315）：聊天页 session/app/agent/usage handler map 与异步注册生命周期抽到 typed `chatEventController`；`events.ts` 是共享注册入口并调用各领域 mapper；
- 已完成（ADR 0320）：聊天页 model/effort/web-search 操作及 typed payload、成功状态更新、通知、失败处理与 refresh-suppression 收口到纯 TypeScript `chatModelOperations`；页面只注入 Svelte state callbacks 并传入 toolbar。`chatModelSync` 仍拥有 discovery/settings 同步。
- 已完成（ADR 0330，本切片；unknown enum 降级由 ADR 0380 删除）：session lifecycle wire event 使用单一 `contracts/session.ts` mapper，删除重复的 TS wire-interface 镜像；mapper 校验必需字段、忽略未知扩展字段，并校验当前 status/waiting-reason 值。此处仍保留手写的内部 camelCase DTO 与映射代码，不代表 Rust DTO 到 TypeScript 的生成已完成。
- 已完成（ADR 0335，本切片；unknown status 降级由 ADR 0380 删除）：Action board 的 `list_actions` rows 与四个 action lifecycle channels 共用 Rust `ActionEvent` wire DTO 和 `contracts/action.ts::mapActionPayload`。Command store 与 event listener 共用运行时 validator/mapper；未知 status/kind/必需字段错误 fail closed，忽略未知附加字段。Action completion outbox 与动态 JSON `tool_args` 保持现有独立边界。
- 已完成（ADR 0340，本切片）：录音事件审计确认 `contracts/recording.ts` 的类型是消费侧 camelCase DTO，不是重复的 snake_case wire interface；`mapRecordingEvent` 是唯一字段转换点，布局 listener 只经 `recordingEventListeners` 调用它。新增跨七个 channel 的审计回归覆盖，保留未知附加字段忽略、未知 VAD 字符串透传和已有畸形值默认，不修改 Rust DTO、生产 mapper、事件 channel 或生产顺序。
- 已完成（ADR 0341，本切片）：Rust `Settings` 是全量配置 DTO 的唯一结构定义；此前 SettingsView、布局、聊天页和 `chatModelSync` 多处直接消费 `invoke('get_settings')`。现在所有读取共用 `settingsCommand.ts::loadSettings` 和 `contracts/settings.ts::parseSettingsPayload`，仅验证根对象并原样保留嵌套 snake_case 字段、未知字段与未来枚举值；null/非对象仍沿用既有 no-op，invoke 错误原样走既有 catch，错误通知顺序不变。`hotkey:rebind` 已由 `contracts/app.ts::mapAppEvent` 单点转 camelCase，回归测试固定映射；`settingsSaveAction`、`settingsGuard` 是纯 UI 行为 helper，无重复 settings store 或第二个 update serializer。无需 Rust DTO 改动，也不引入 codegen。
- 已完成（ADR 0344，本切片）：background/scheduled action UI projection 审计确认 `actionStore`、mapper、live-row 判定和生命周期 upsert 已共享；finished 的 background transcript finalization 与 scheduled cleanup/Agent notification 有意保持分开。layout 改为复用已测试的 `finalizeBackgroundActionMessages`，补充 live refresh 跨 kind 与 scheduled 不进入 background tool-card projection 的回归测试。event id 仅是 Tauri envelope id，当前没有 lifecycle event dedup；不改 Rust wire、channel、ActionService 生命周期或事件顺序，不引入 Job reducer。
- 已完成（ADR 0346，本切片；unknown enum/status 行为由 ADR 0380 收紧）：app-shell 的 `appEventListeners` 与 ToolsView MCP/Skills 单条 listener 共用 `mapAppEvent` adapter；`mapAppEvent(unknown)` 校验 Tauri envelope 与 app payload 必需类型，畸形事件被丢弃并记不含 payload 的 warning。bootstrap/tray status、Skills operation (`refresh` / `auto_refresh` / `toggle`)、MCP status 与 interaction kind/status/risk level 只接受当前 wire 值；MCP 与 interaction 显式投影并忽略额外字段，options 仍按当前 Rust DTO 对空数组省略的约定默认成空数组。MCP toast、ToolsView refresh、通知、settings/hotkey 顺序与副作用 owner、Rust channel/payload 不变；无 Rust DTO 或全局 codegen 变化。
- 已完成（ADR 0347，本切片；unknown enum-like 行为由 ADR 0380 收紧）：Rust `event.rs` Agent DTO 保持 wire 权威；删除 `contracts/agent.ts` 重复的 snake_case wire interfaces，并让唯一 `mapAgentEvent` 从 `unknown` 校验 envelope、必需字段、固定工具 outcome/retry/idempotency/scope 值与嵌套 payload，再映射为 camelCase。未知附加字段忽略，显式动态扩展值保留；`agentEventListeners` 丢弃畸形 payload 并只记录通道级 warning。布局与聊天页共用唯一 `appSessionReducer`；通知/media-plan 的不同副作用 owner 与 usage 未知 `call_kind` 的日志后跳过行为保持。另记录现存 `SessionCompleted`/`SessionError` 主事件和 secondary `session:updated` 双发，聊天页终态 cleanup 有重叠；该问题由 ADR 0349 审计，继续保留原行为和通道语义，因没有共享 occurrence identity 且 `session:updated` 也承载独立终态更新，暂不做去重。resume normalizer、Rust wire、channel、顺序和全局 codegen不变。
- 已完成（ADR 0348）：Action board 活跃 `list_actions`/`cancel_action` 调用通过 `actionCommands.ts`，命名 request/result contract 与 ADR 0335 的唯一 Action mapper 共用；无 UI 调用者的 `list_action_history`/`delete_action` 不因此新增 wrapper。已完成（ADR 0350、0376）：session lifecycle field mapping、reducer/transcript/usage projections 与 resume interaction normalizer 经审计；live interaction mapping 与 snake_case resume DTO 保持独立。Settings shape 与所有 `get_settings` 读取已由 ADR 0341 收口，诊断 response parsers 见 ADR 0007；update payload 仍由 SettingsView 的单一 builder 构造。
- 已完成（ADR 0357）：memory fact/recall 的四个活跃命令共用 `memoryCommands.ts` 与命名 request/response DTO；Rust `Fact` / `MemoryRecallItem` 的 snake_case wire shape 原样传给现有消费组件，响应附加字段与 invoke rejection 保持透传。IPC contract script 对比 Rust command 参数/DTO 字段与 TypeScript contracts，并固定 helper 无绕行。事实与 recall 的旧 `any`/重复局部结果类型已移除；没有第二个 camelCase mapper。Session history 与 Settings memory maintenance 是另外的 command families，不在本切片内。
- 已完成（ADR 0314）：将 `sessionReducer.ts` 按 lifecycle/transcript/interaction/usage/stream 拆成内部 reducer module；外部 API 和单一 `sessionStateStore` 订阅保持不变；
- 已完成（ADR 0322）：页面与布局不再把完整 `SessionReducerState` 镜像到 `$state`；通过相等性门控 selector 订阅同一个 `sessionStateStore` 的 sessions、active session ID、活动 transcript/usage、interactions 和必要的 error/termination 切片。selector 只缓存当前结果，引用不变时不通知，最后一个 listener 离开时释放 root subscription；reducer state ownership、dispatch、事件顺序均不变。
- 已完成（ADR 0368）：`discover_models` / `discover_all_models` 共用命名 request、Rust `ModelInfo` 的 TS wire contract 与 `modelDiscoveryCommands.ts` typed direct-forward boundary；ModelSettings discovery、chat default model sync 与 MediaSettings STT discovery 不再直接 invoke。缓存、同 URL in-flight 合并、空列表、过期请求忽略、未知扩展字段、错误传播及刷新通知顺序保持原样。`check_llm_connection` 的唯一 layout 调用继续使用现有 report normalizer；`get_performance_metrics` 保持 diagnostics helper，不并入 discovery family。
- 已完成（ADR 0369，2026-09-26 catalog 切片）：ToolsView 的 catalog reads 与 `reset_tool_circuits` 通过 typed `toolsCommands.ts` helper；builtin manifest 仍由 `parseToolManifest` 唯一投影，`setToolManifests` 返回同一批解析行供 cache/card 共用。MCP/Skill list 保持 snake_case、动态 schema、未知附加字段与原刷新/错误语义；MCP status 变体由 ADR 0381 收紧。
- 已完成（ADR 0370）：Settings 的日志信息、日志尾部、shell availability、API-key status 与 performance metrics 读取统一经过 typed `diagnosticsCommands.ts`。日志/shell/API-key 响应继续使用 `contracts/settings.ts` 的既有唯一 parser；metrics 不做字段投影，新增的动态诊断字段原样保留。renderer metrics provider 仍由 `performanceMetrics.ts` 持有，SettingsView 的读取顺序、tail limit、通知与错误流程未变。IPC 脚本对照 Rust command registry、请求/响应 DTO 与 typed helper，并拒绝 UI 直接 invoke；无 Rust handler、DB、ID、X12 或 codegen 变化。
- 已完成（ADR 0373）：审计确认 Tauri、`actions`/`schedule` 工具、定时 worker 与 Agent completion 共用一个 `ActionService` runtime owner，`ActionStore` 是生产持久化写边界；`schedule.set` 创建新 action，没有 update-existing 或手动 trigger IPC。Tauri cancellation 按 kind 限定，terminal delete 仍只处理历史；UI 未调用 history/delete 命令。`ToolConcurrency` 不构成跨入口锁。发现 terminal history delete 可与终态投影/outbox 发布或 retry 交错，外键级联可能移除未确认 completion；pending completion 在 ack 前拒绝删除、ack 与 delete 共用 SQLite writer 原子边界的策略已由 ADR 0374 实现；本段保留 ADR 0373 审计时的历史发现。后续没有改变 action 生命周期或 IPC 行为。
- 已完成（ADR 0394）：ask/input composer 分流由 `chatAskInteraction` 拥有；startup resume target、auto-restore 与列表刷新由 `chatSessionStartup` 拥有；scroll follow、RAF、ResizeObserver 与 composer clearance 由 `chatViewController` 拥有。页面只接线；既有 replay/reducer、ChatController 和 event mapper 语义不变。

主要文件：`crates/app-binary/src/events.rs`、`ui/src/lib/contracts/`、`ui/src/lib/sessionReducer.ts`、`ui/src/lib/sessionReducer/`、`ui/src/routes/+page.svelte`、`+layout.svelte`、scripts。

验收：`scripts/check-ipc-contracts.ps1` 从 Rust handlers、`Serialize`/`Deserialize` DTO 与 Serde 属性生成 command map，并验证无漂移；同时核对 74 个 command name 在 handler、注册表、前端边界和文档之间一致。`scripts/check-ipc-events.ps1` 核对 40 个 event channels。生成器对不支持的 Rust/Serde wire shape fail closed；生成的静态类型不替代前端 runtime validators。最终验收记录见 2026-09-29 阶段 8 收口。

2026-09-25 切片进展（ADR 0313）：Controller 只通过 typed invoke/submit/reducer/session-snapshot/通知与 UI callback dependencies 执行会话异步流程；`continueSession.ts` 与 `resumeMessages.ts` 保持纯策略/message projection 边界。页面保留 input-router/ask 决策、传入 `chatEventController` 的 typed callback wiring、model sync、resume target/auto-restore、新会话入口及 dialog/loading/menu/scroll 状态。纯 Vitest 覆盖 rollback 两分支、continue 两种策略、interaction preservation、created-session selection 和失败/重复请求保护。其余 Phase 8 工作仍按上列范围推进。

2026-09-25 切片进展（ADR 0314）：`SessionReducer` 内部实现已按 lifecycle、transcript、interaction、usage、Agent stream 与共享 replay/state helper 拆分；原 facade 继续拥有跨域 resume/clear 组合、observable wrapper 和唯一 writable store。回归覆盖 resume + pending interaction + usage restore/live、stream reset + chunk sequence、error + termination 刷新。

2026-09-25 切片进展（ADR 0315）：`chatEventController` 拥有聊天页 handler map 组合及注册/释放生命周期；页面等待 listener ready 后才加载 settings 和恢复会话。`events.ts` 拥有共享 listener registration 与领域 mapper 调用入口；测试通过注入 registration port 覆盖通道、ready 和释放竞态。

2026-09-25 历史切片记录（ADR 0320；其后 ADR 0330、0335、0340、0341、0346、0347、0348、0350 分域审计/收口对应 contracts 与 mapper）：`chatModelOperations` 拥有三个 toolbar model 操作的 typed payload、状态更新、通知、错误处理与 refresh suppression；页面仅接线，`chatModelSync` 继续拥有 settings/discovery。全局 Rust DTO → TypeScript codegen 尚未引入，是否引入仍待决策。

2026-09-25 切片进展（ADR 0322）：`createSessionSelectorStore` 只观察唯一 `sessionStateStore`，按 `Object.is` 对当前选择结果门控通知，并在 selector 最后一个订阅者释放时断开 root subscription。页面迁移 sessions、active session ID、活动消息、usage、interactions、error/termination；布局迁移 sessions、active session ID、interactions。活动消息按 reducer 当前 active ID 选取，缺失消息复用稳定空数组。完整 root state 不再广播到这两个路由的 `$state` 镜像；复杂 view state、ask/input 决策、启动恢复和 replay 仍走既有路径。无 reducer ownership、状态转换、事件顺序、IPC 或持久化变化。

2026-09-25 历史切片记录（ADR 0330；当时列出的后续域已分别由 ADR 0335、0340、0341、0346、0347、0348、0350 审计或收口；unknown status/reason 降级由 ADR 0380 删除）：以 app-binary session event DTO 为 wire 权威，`contracts/session.ts` 是 session lifecycle snake_case→camelCase 的唯一前端 mapper；删除重复 TS wire interfaces，用运行时校验拒绝 malformed required fields、丢弃未知事件、忽略新增字段；旧版曾把未知 status 降为 `error`、未知 waiting reason 降为 `null`，现在未知值会丢弃事件。`events.ts` 的两个 session listener 路径共用该 mapper；chat controller/handler/reducer 仅接收已映射 DTO。channel、payload、顺序、幂等和 UI 行为不变。其余 event/command DTO mirror 及 session 内部类型/字段映射的生成收口仍待后续阶段。

2026-09-25 历史切片记录（ADR 0335；其后 ADR 0340、0341、0346、0347、0348、0350 分域审计/收口当时列出的 recording、Settings、app/agent event、活跃 action command 与 session mapping 项；unknown status downgrade 由 ADR 0380 删除）：以 app-binary `ActionEvent` 作为 Action board 与 lifecycle wire 权威；`list_actions` command rows 和 `action:created/updated/output/finished` 通过 `contracts/action.ts::mapActionPayload` 共用唯一 runtime validator/mapper。`actionStore` 与 `events.ts` 两个消费路径删除不安全的 wire casts；错误必需字段、未知 kind/status 丢弃，未知扩展字段忽略，warning 不含 payload。字段、排序、分页、running/terminal 投影、event/command 注册、通知和 completion outbox 顺序不变。Action completion outbox 是独立的 agent 内部完成类型，`tool_args` 仍为原始 JSON 扩展，不纳入 UI ActionEvent。当前全局 Rust→TypeScript codegen 未引入，其他 command families 仍待审计。

2026-09-25 切片进展（ADR 0340）：Rust recording DTO 是 wire shape 的唯一权威；前端只定义 route-facing camelCase DTO，并通过 `mapRecordingEvent` 转换。审计确认没有第二份 wire interface 或消费方字段映射，因此不再提取新 validator/mapper。回归测试固定七个 channel 的集合与映射字段，确认未知附加字段丢弃、VAD 的未知字符串透传、既有畸形值回退、listener 按接收顺序同步分发。Rust 生产者、channel、payload、错误内容、ID 关联及 started/stopped/transcription 顺序不变；不引入 codegen。

2026-09-25 切片进展（ADR 0341）：Rust `haven_common::config::Settings` 继续作为全量设置结构的唯一权威；TS 没有维护第二份嵌套 schema，但多处 raw `invoke('get_settings')` 曾绕过边界。所有读者现经一个 command helper 和开放式根对象 validator；unknown fields/enums、snake_case 配置字段、null no-op、command rejection 传播及现有错误通知顺序保持。`hotkey:rebind` 沿用 `mapAppEvent` 的 snake_case→camelCase 映射并补测试；诊断命令 parser、单一 update 表单 builder、dirty snapshot、save affordance 与 leave guard 行为不变。无 Rust/wire 变更、无全局 codegen。

2026-09-25 切片进展（ADR 0346，unknown status 行为由 ADR 0380 收紧）：Rust `events.rs` 继续拥有 app-shell wire DTO/channel，`contracts/app.ts::mapAppEvent` 仍是唯一字段映射点。ToolsView 原先对 `mcp:status_change` / `skills:status_change` 使用 raw `registerOne`；现在 typed `registerAppListener` 与批量 `appEventListeners` 共用 adapter，布局内无副作用的 Skills listener 已删除。MCP 的 layout toast 和 ToolsView refresh 仍是独立副作用；MCP event mapper 现在只接受当前 status variants，并只投影已知字段。`InteractionRequestedEvent` 在 session resume command 的另一条 normalizer 路径不属于本切片，app DTO mirrors 仍是手写 contract；没有 Rust IPC 变化或 codegen。

2026-09-25 切片进展（ADR 0347，unknown enum-like 行为由 ADR 0380 收紧）：审计 `event.rs`/`event_bridge.rs`、agent/session/interaction/usage/error stores 与 layout/page listener 后，确认所有 Agent 消费者都经 `agentEventListeners`；Agent 页面与布局订阅 channel 互斥，并共用一个 session reducer。移除 `AgentWirePayloadMap` 及 wire interface 镜像，`mapAgentEvent(unknown)` 校验 envelope、必需字段和 nested result/media rows；未知扩展字段仍忽略，固定工具 outcome/retry/idempotency/scope 只接受当前值，动态 JSON、notification 空文本默认、usage 未知 `call_kind` 的既有 error log + skip fallback 保持。listener malformed warning 不包含 payload；事件、toast、系统通知、session、usage 和 media plan 副作用顺序/owner不变。另发现 SessionCompleted/SessionError 经 primary channel 和 secondary `session:updated` 双发，聊天页终态 cleanup 有重叠；后续审计结论及暂不去重原因见 ADR 0349。未改 resume interaction normalizer、Rust DTO、IPC channel 或 codegen。

2026-09-25 切片进展（ADR 0348）：Action board 的活跃 `list_actions`/`cancel_action` 调用统一经 `actionCommands.ts`。`list_actions` 返回值先按 `unknown` 接收，再复用 `mapActionPayload` 生成 camelCase DTO；畸形顶层仍 no-op，畸形行仍由 store 发通用 warning 并跳过。`cancel_action` 使用命名 `CancelActionRequest` 与 `Promise<boolean>`，请求字段和 rejection 保持原样。store 不再直接 invoke；IPC contract 脚本固定 mapper 与 command boundary 不被绕过。未触碰无 UI 调用者的 `list_action_history`/`delete_action`、Rust command/DTO、wire payload、DB、错误语义或 UI 行为；不引入 codegen。

2026-09-26 切片进展（ADR 0357）：`list_facts`、`add_fact`、`delete_fact` 与 `recall_memory` 使用 `memoryCommands.ts` 和命名 request/response types；`MemoryView` 不再直接 invoke 这四个命令。Rust `Fact` 与 `MemoryRecallItem` 仍是 wire authority，前端 DTO 保留当前 snake_case 消费字段，薄 helper 不转换、不筛字段、不包装错误；测试固定扁平请求和附加响应字段，IPC contract script 校验参数与 DTO 字段并固定直接转发及调用边界。Session history、`run_memory_maintenance`、Rust commands、DB/ID/X12 与 UI 行为不变；不引入 codegen。

2026-09-26 切片进展（ADR 0358）：活跃 session history UI 调用（`get_sessions`、`search_history_filtered`、`get_session_for_resume`、`get_last_conversation` 及重开/删除/清空/重命名）统一经 `sessionHistoryCommands.ts` 和命名 request/response DTO；`resumeMessages.ts` 复用同一会话恢复 wire contract，删除重复的局部 resume row/usage interfaces，MemoryView 移除历史行与筛选参数的 `any`。helper 原样转发扁平参数、响应与 rejection；不添加 validator 或 mapper。页大小、排序、snake_case 字段、resume/reopen 顺序、通知/错误处理和未知附加字段保留。审计确认旧 `get_history`、count/search variants 与 export 在 UI 无调用者，仅保留契约登记，不创建 dead helpers；无 Rust handler、wire、DB/ID/X12 改动，也不引入 codegen。

2026-09-26 切片进展（ADR 0368）：`discover_models` / `discover_all_models` 的活跃 UI 调用统一通过 `modelDiscoveryCommands.ts` 和命名 request/result types；Rust `ModelInfo` 与 TS response contract 的字段经 IPC script 对照，helper 原样转发 flat 参数、空结果、扩展字段和 rejection。`modelDiscovery.ts`、`chatModelSync.ts` 与 `MediaSettings.svelte` 的调用状态机、缓存/in-flight、STT role 参数、错误/通知顺序均保持。`check_llm_connection` 已有单一 `normalizeLlmConnectionReport` 和唯一 layout caller，无重复 mapper；performance metrics 已由 diagnostics helper 所有，不并入本切片。没有 Rust DTO、command、wire、router、DB/ID/X12、Settings apply 或 codegen 变化。

2026-09-26 切片进展（ADR 0370）：SettingsView 原先直接 invoke `get_log_info`、`read_log_tail`、`check_shell_available` 和 `get_api_key_status`，同时已有唯一 response parser；性能指标已由 `performanceMetrics.ts` 集中 invoke 并采集 renderer counters。本切片将五个读取入口归到 `diagnosticsCommands.ts`，沿用旧 parser 与动态 metrics passthrough；命名 request 和 DTO 字段、无绕行规则加入 IPC contract check。日志尾部请求限制、API-key 仅 presence flags、shell fallback、错误传播及 UI 通知/顺序保持，不包含 settings writers 或 connectivity probe。

2026-09-26 切片进展（ADR 0371）：审计 `continue_session`、`interrupt_session`、`end_session`、`rollback_session` 与 `resolve_confirmation` 的 Rust handler、command registry 和 UI 直接调用。ChatController 是前四项唯一聊天编排 owner；shell confirmation 保留在 layout，因确认弹窗需跨工作区可见。补齐 rollback/confirmation 命名 request DTO，以 `satisfies`/JSDoc 固定调用参数；IPC contract check 对照 Rust 参数字段与 TS DTO，并锁定直接 invoke owner。保留 rollback/continue in-flight 锁、请求字段、notification/error 与 optimistic confirmation 顺序；不加薄 helper，不改 session history、resume normalizer、ask/input、event dedup、DB/ID/X12 或 codegen。

2026-09-26 切片进展（ADR 0372）：Settings/model/provider apply 审计确认 `ConfigService` 是配置真源，Settings 与 model 共用 `RuntimeConfigCoordinator` gate；`SettingsRuntimeApplyCoordinator` 只持 phase/order/observation，shared target mapper 不重复。SettingsView 单一 `update_settings` builder 复用开放式 Rust-owned `SettingsPayload`；IPC script 固定 Rust `Settings` 参数、registry 和唯一 caller，不生成或复制 nested schema。ConfigLoader 以同目录临时文件 rename 保存，ConfigService 在保存成功后增 version/publish；保存后的 apply failure 保留 durable config，重复 payload 不重试且无 auto-restart。产品进一步确认仅变更 `SkillsExec` 时跳过 live Skills apply、只保存并标记重启生效；Settings/model apply 与同运行时配置域的 Tools 管理操作统一串行。上述行为的具体 runtime/UI 实现留给后续切片。

2026-09-27 实现进展（ADR 0351、0372）：App composition root 为 Settings/model 与 Tools AdminServices 注入同一个 runtime config gate；Tools mutation service 持锁覆盖 durable edit、live side effect 和 catalog rebuild。永久权限保存/live apply、Tauri MCP refresh/reconnect 也进入该 gate。纯 SkillsExec 不运行 live Skills phase；Skills+SkillsExec 同时保留 live Skills phase 与 Skills restart-required。durable 后 apply failure 返回部分 apply 错误，保留磁盘配置、不重试，Settings UI 显示已保存和重启恢复说明。移除 Tauri wrapper 重复 catalog rebuild/monitor start，保留确需手工 monitor 的 refresh/reconnect 路径。未改配置格式/数据库 schema。

2026-09-28 配置保存语义与失败后运行语义跟进（ADR 0372）：保存成功定义为配置序列化、同目录临时文件写入和路径替换成功；不承诺操作系统崩溃或突然断电后的稳定介质持久性，也不增加 Windows 专用 flush。Settings 成功及部分 apply 失败文案统一改为“已写入配置”。apply 失败至重启前普通会话和工具可继续运行，使用其 runtime owner 当前状态，接受跨 consumer 的暂时混合 revision；turn catalog 仍按已有 immutable snapshot 语义。阶段 5 补充对应回归验收条件。

2026-09-28 阶段 5 apply 失败观测（ADR 0351）：Settings 的 shell、context limits、tool settings 与 Router publish 会把 builtin catalog rebuild 拒绝作为 typed failure 返回 coordinator；Agent 同步 settings mutex poisoning 也可传播并带 phase/failure kind 记录。Router publish 的 catalog 失败标记 Agent Router 已发布，并保留对应 `llm:config_changed` event；后续 phase 停止。明确没有失败分支的 InputPipeline/SessionExecutor 内存更新继续按 infallible 操作处理，MCP 连接和 monitor 仍由 MCP manager status/warning 表达。durable config、无自动 retry/compensation、部分 apply 与重启恢复策略不变。

2026-09-27 Tools IPC follow-up（ADR 0369）：ToolsView 的 MCP/Skills add/update/remove/toggle、reconnect、refresh、open-directory 与 enable commands 全部走 typed `toolsCommands.ts` helper；IPC script 校验 Rust command registry、handler 参数、McpServerConfig/McpRefreshResult wire fields、helper request/response signatures，并禁止 ToolsView raw/dynamic invoke 绕过。McpServerConfig 是固定 snake_case DTO，动态 JSON schema 保留在 `ToolSchema = unknown` 边界，不增加 config parser。保留 native admin authorization、MCP/Skills status event producer、通知/错误处理、optimistic toggle 和 list refresh owner/顺序；`refresh_mcp_servers` contract 校正为 renderer-triggered reconcile，未改 handler/wire。后续安全切片已收口此发现：`refresh_mcp_servers` 与 `reconnect_mcp` 现复用 typed native admin request、AuthorizationEngine 与确认 receipt，并在共享配置 gate 内复核目标；拒绝或待确认时不产生连接副作用（ADR 0369，2026-09-27）。cargo fmt/check/clippy/workspace tests、UI check/836 tests/build 与两个 IPC scripts 全部通过；cargo test 在唯一目录 `target/test-data/tools-mutation-ipc-20260927-run02/APPDATA` 隔离运行。无配置、数据库或重置要求。

2026-09-25 切片进展（ADR 0349，后由 ADR 0386 收口）：审计 `SessionCompleted`/`SessionError` 的 primary channel 与 secondary `session:updated` 顺序及 chat/layout/MemoryView 消费后，确认聊天页有真实重复终态清理和部分 reducer selector 重复通知；消息 finalization、preview clear 与刷新有值幂等/合并路径，但 inactive usage eviction、interaction/step-block reducer dispatch 会重复产生新 map。Rust 桌面通知与 layout 终态 toast 仍各只由 primary 处理，`session:updated` 还承载 ingress 等独立终态投影。原审计拒绝按 session/status 或 payload 推断去重，并暂缓共享 occurrence identity；具体实现及保留的独立终态路径见 ADR 0386。

2026-09-28 修复切片（ADR 0386）：终态 primary 与 secondary payload 共享通过 `new_id("occ")` 生成的短期 `occurrence_id`；聊天页精确匹配 paired projections，第一个处理的 channel 运行 cleanup，另一个跳过；standalone terminal `session:updated` 仍完整清理。reducer 对未变化的 status/termination/error projection 返回原状态，避免无效 selector 通知。布局 busy/model 状态、通知 owner、channel 顺序及 MemoryView refresh coalescing 保持；无数据库、durable sequence 或 resume 变更。

2026-09-25 切片进展（ADR 0350，后续由 ADR 0376 校准）：审计确认 session lifecycle wire→camelCase 只由 `contracts/session.ts::mapSessionEvent` 执行；live interaction event 与 resume response 是不同 DTO，保持不同 mapper。该审计曾保留 resume camelCase 兼容、coercion 和默认值；当前恢复响应只接受 Rust snake_case DTO，畸形行继续过滤。没有改 Rust DTO、IPC、事件顺序、X12、dedup 或 session behavior。

2026-09-27 兼容清理（ADR 0376–0379）：删除已无独立 owner 的会话错误 facade；移除只在测试中调用的 MemoryWorker runtime 探测/同步 enqueue 双路径，durable marker 失败不再落入 RAM 队列；删除仅测试使用且重复 SQL 的 `complete_scheduled_action` helper；恢复 interaction normalizer 收敛为唯一 snake_case DTO；缺失 session 的 transcript 写入从静默空成功改为外键错误，且不产生事件/投影。无持久化契约、配置或 schema 变化，不需数据重置。
2026-09-27 event contract 收紧（ADR 0380）：session/action/app/Agent event mapper 校验当前固定 status、reason、交互/MCP status、工具 outcome/retry/idempotency/scope 值和省略式 Option wire 形状；固定 DTO 校验失败时丢弃该行或事件。保留动态 payload 的既有显式边界和未知附加字段忽略行为。无 Rust wire、持久化、配置或 schema 变化，不需数据重置。
2026-09-27 Tools UI 契约收紧（ADR 0381）：MCP client status 精确对应 Rust enum；Agent Action/Observation 的 nullable tool_call_id 与 Observation renderer/result 按当前 DTO 必填形状校验；无法由事件或当前 manifest 确定 renderer 的历史 JSON 结果改由通用卡片展示，删除按结果形状猜测 renderer 的旧分支。动态 schema 与 MCP snapshot 的未知附加字段仍透传；无数据库、配置、持久化 payload 或 Rust wire 变化，不需重置数据。

### 阶段 9：Common 收缩、性能剖析和发布验收（最后）

目的：只有在所有权和稳定边界稳定后，才决定是否拆 crate 和做性能优化。

工作项：

- 根据依赖图决定是否把 common 拆成 contracts/config/media/platform；若只是移动复杂度则不拆；
- 对 actor mailbox、event replay、reducer broadcast、LLM request、Job/Memory outbox 做 profiling；
- 为 `session_events` 与 SQLite/WAL 测量真实磁盘增长及清理后的文件行为，决定是否需要字节上限、归档/导出、容量告警，并验证低磁盘时 durable append 的失败观测和恢复策略（ADR 0389）；
- 只根据数据加入 bounded cache、selector 或批处理；每个缓存写清容量、失效和取消；
- 用全新数据目录完成启动、设置、会话、工具、媒体、任务、恢复、回滚、升级重置和卸载验收；
- 更新 `architecture.md`、`stability-refactor-plan.md`、ADR 索引、发布/重置说明。

#### 2026-09-26 第一轮审计（ADR 0359）

- 将 Windows Job Object 子进程 containment 从 `haven-common` 移入无内部依赖的 `haven-platform`；MCP 与 Tools 两个独立进程 adapter 直接依赖该平台叶子。`haven-common` 不再直接依赖 `windows-sys`，containment 的创建、attach、drop 和非 Windows no-op 行为保持不变。
- 暂不拆分 config、media、types 或 prompts：这些模块仍跨多个独立 crate 共享稳定类型；`ConfigService` 由 app 与 Tools admin 共用，`MediaType` 同时参与 app ingress 与 MediaAsset 契约。拆分会增加服务层依赖或分散权威定义。
- `cargo metadata` 未发现 feature 开关；只有 app-binary 有 build script。工作区没有 Cargo bench target 或 Criterion 等采样基准框架。现有 ADR 0178/ReAct metrics 提供固定内存 phase histogram、错误/重试计数、context queue gauge，以及 UI frames/chunks/drops 导出；它不观测 actor mailbox queue、reducer 广播成本、durable event replay 的分布或完整 Action/Memory outbox 延迟。已有 event replay 1k/10k/100k 单测基准和多域行为回归；ADR 0359 记录复跑命令、可观测字段和未覆盖指标，不据此做缓存、selector 或 batching 优化。

#### 2026-09-26 第二轮性能基线切片（ADR 0360）

- 扩充既有 session event replay 内存 SQLite fixture：1k/10k/100k 历史输入分别对 full read 与 compaction active suffix read 预热 2 次、交错测量 21 对，打印微秒 p50/p95；fixture 建立不计时。此输出只是 test profile、热内存数据库读取的局部观测，不表示磁盘、冷启动或生产延迟。
- actor mailbox、UI reducer broadcast、Action completion outbox 与 Memory fact extraction outbox 继续只复跑行为测试并明确指标缺口。现有 fixture 尚不能隔离目标阶段成本，因此不加生产 timer、自制异步负载、依赖或性能阈值；不改变重试、取消、顺序、wire、UI 和 X12 语义。

#### 2026-09-28 Actor 饱和公平性验收（ADR 0390）

- 补齐 SessionActor 饱和 mailbox 下的行为验收：默认公平调度 run 与外部命令；持续负载时验证 run 继续推进、排队 sender 完成 Snapshot 往返、Cancel 能应答并结束 run。同步 direct-run release 使用独立容量 1 的信号通道，重复 release 合并且不创建发送 task。超时只约束测试活性，不构成生产延迟 SLA。

2026-09-29 阶段 9 核心流水线性能剖析补齐（ADR 0360）：增加可手动复跑的 actor mailbox、UI reducer、LLM router 与 Action/Memory outbox profile；记录命令往返/dispatch/request/outbox 阶段的 p50/p95、队列深度/订阅通知数和吞吐。Rust outbox 使用临时磁盘 SQLite，LLM 仅用无网络 mock；profile 不加生产 timer、阈值或 runtime policy。单机数据与 mock/fixture 限制见 ADR 0360；Windows GUI profile、安装升级/数据库重置与卸载验收仍开放。

#### 2026-09-26 最终验收审计（ADR 0361）

- 总体验收仍未完成：durable transcript recovery、SessionActor 单 task 所有权与阶段 3 的 raw Database/Action typed projection 验收满足（ADR 0382、0383）；App composition root 可持有 raw `Database` 创建 typed stores。阶段 4 的执行 facade、prompt/catalog/observation adapters 在本次 2026-09-26 审计时仍依赖 `ToolsManager`，后由 ADR 0388 收口；runtime failure semantics、Rust→TypeScript codegen 等全局条件仍有未决项。
- 本轮 Rust workspace 与 UI 的格式、测试、类型检查、clippy、生产构建及 IPC validators 全部通过。AppState 测试启动的媒体和上传清理根已改为显式临时目录；测试使用仓库 `target` 下的隔离 `APPDATA` 根。详见 ADR 0361 的命令与证据。
- 全新用户配置下的 GUI 启动、模型设置、媒体/任务流程、升级重置和卸载未执行；仓库当前没有 disposable profile / VM 自动验收入口。阶段 9 在完成一次性 Windows profile 或 VM 中的发布验收前保持开放。
- 本轮不拆分 `haven-common`、不引入新的 runtime 性能优化，也不实现已确认的 Settings 同域串行/SkillsExec 重启生效策略；审计当时仍未选择 Settings recovery/retry、完整 Job lifecycle、session occurrence identity、codegen 或剩余 UI command-family 策略。session occurrence identity 已由 ADR 0386 解决；其余未决项继续由各自 ADR 跟踪。

2026-09-27 状态校准（ADR 0351、0372）：上述 9-26 审计快照中的 Settings 同域串行/SkillsExec 语义“留后续实现”状态已由本轮切片 supersede。当前实现覆盖 Settings/model 与同域 Tools admin writer 串行、SkillsExec-only restart-required、durable-first 部分 apply failure/no automatic retry/重启从磁盘恢复；更广范围的显式 retry 或 phase compensation/recovery policy 不在本切片内。

## 5. Agent 委派策略

- 只把有明确写集和退出条件的阶段交给一个 Agent；模型固定 `gpt-6-luna`、reasoning `xhigh`。
- 不让两个 Agent 同时修改同一文件；独立审查和测试可以并行。
- 每个代码 Agent 必须：先读规范/相关 ADR；增加或保留回归测试；删除旧路径；运行适用门禁；提交单一目的 commit；报告改动文件、测试和遗留风险。
- 主线程集成前检查 `git diff`、`git diff --check`、依赖方向和旧入口搜索；必要时补测试或拒绝补丁。
- 每一阶段完成后再派发下一阶段；不得用“先留下兼容层”绕过退出条件。

## 6. 全局完成定义

全部阶段不等于文件变少；以下是截至 2026-09-30 的最终验收状态（基础审计见 ADR 0361，阶段 1/3 更新见 ADR 0382、0383，阶段 4/7/8 与阶段 9 容量更新见 ADR 0388、0392–0395、0404，memory/runtime 与配置更新见 ADR 0364、0367、0351、0372、0416）：

| 条件 | 状态 | 审计结论 |
|---|---|---|
| durable session 恢复只依赖事件回放 | 满足 | `session_events` replay 是 transcript 恢复权威；未进入事件流的已接受输入由 `pending_session_inputs` 持久跟踪，并与 `UserInject` 提交原子 ack，不依赖停机时间或内容匹配（ADR 0416）。 |
| MemoryRuntime 对象与 prepare/live task 由应用持有 | 满足 | AgentStartup 将唯一 `MemoryStartup` 交给 ApplicationRuntime；prepare/replay 后注册 live consumer，注册成功才将 typed MemoryReady 交给 dispatcher。周期维护、manual pass、worker shutdown 和任务 join 使用原顺序（ADR 0367）。 |
| session-local mutable state 只有 actor owner | 满足 | `SessionState::react_run` 持有 active future，future 独占捕获 run-local `ReActState`；actor loop 在同一个 select 中轮询 future 与 mailbox，pending await 不借用整份 state（ADR 0214、0382）。 |
| raw Database 不穿透仓储边界 | 满足（生产边界） | App composition root 持有 `Database` 并创建 typed stores；生产 Agent 路径不把 raw `Database` 传入 `MemoryService`。`MemoryService` 仍有 `#[cfg(test)]` 私有 `test_database` fixture handle 和仅供测试的 `From<Arc<Database>>` 转换；`MemoryService::new(MemoryServiceStores, …)` 仍是组合根构造入口，接收 typed capabilities。repository stores 自身仍封装其 backing Database（ADR 0383）。 |
| ToolsManager execution/adapters 不穿透 Agent 端口边界 | 满足 | App composition root 用唯一共享 `ToolsManager` 创建 manager-backed prompt/catalog/execution/authorization/observation/overlay/asset adapters 并注入 typed bundles；`haven-agent` 生产 runtime owners 只持有端口和共享的 typed authorization/action capabilities。完整 execution context、live authorization request 准备和 manager adapter 映射由 ADR 0384、0388 收口。 |
| stable domain outputs typed，动态 JSON 边界有明确注释 | 满足 | 跨 crate ports/services、已注册 Tauri command responses 与 40 个 event channels 已逐项分类并记录 owner；明确保留动态 JSON 的 tool/provider/MCP/config 扩展边界（`architecture-output-contract-inventory.md`、ADR 0391）。 |
| 配置、工具、模型、任务、记忆的 runtime replacement/失败/取消语义有测试 | 部分满足 | 各域已有局部回归；Settings/model durable-first 部分 apply failure、无自动 retry、重启从磁盘恢复、SkillsExec restart-only 与同配置域串行已实现（ADR 0351、0372）；scheduled dependency restart recovery 与结果投影已有专门回归（ADR 0392、0393）。不承诺跨所有 runtime owners 的统一替换矩阵，也不引入通用 Job executor、deadline、owner token/续租或自动 replay。 |
| Rust/TS IPC 生成与校验一致 | 满足 | Rust handler 与 Serde DTO 生成 command request/response contracts；静态类型、运行时 validators 和 74 command/40 event drift checks 均有门禁（ADR 0394）。 |
| Rust 与 UI 格式、测试、类型、lint/build 门禁通过 | 满足 | 阶段 8 门禁由 ADR 0394 记录：Rust fmt/check/clippy/workspace tests 与 UI check/874 tests/build 均通过。本轮阶段 9 容量探针另通过 `haven-memory` 全量测试、check 与 Clippy。 |
| 文档、ADR、重置说明、Git 历史和工作区状态可审查 | 满足 | ADR 0361、0367、架构、路线图和发布/重置文档记录本次证据与限制；提交后复核 Git 状态。 |

阶段 9 的 Windows profile/VM GUI、安装升级、数据库重置和卸载验收仍是单独的发布条件；容量 probe 与 workspace checks 不证明这些桌面流程。阶段状态以未决 ADR 及下方验收记录为准，不因质量门禁通过而宣告全局完成。

2026-09-28 跨层输出审计收口：本节此前将 “stable domain outputs typed，动态 JSON 边界有明确注释” 标为部分满足；现按 [跨层输出契约清单](architecture-output-contract-inventory.md) 将该项关闭为满足。清单逐项记录已确认生产输出的 producer、consumer、稳定性、Serde/JSON 边界、结论与 owner，覆盖跨 crate ports/services、全部 71 个 Tauri 命令成功签名和 40 个事件 channel。AdminServices 固定形状由 ADR 0391 类型化并在既有 tool wire 边界序列化；任意配置 path、工具参数/结果、provider payload、MCP 协议载荷等保留有 owner 的动态 JSON。清单中的 further-review 项均已分类和具名 owner，作为后续切片跟踪，不阻止本项验收。本验收不要求穷举无生产消费者的每个 public helper，也不以仓库清零 `Value` 为目标。

2026-09-29 阶段 8 收口（ADR 0394）：Rust handlers 与 Serde DTO 生成 request/response command types，输入按 Deserialize/default/Option/skip 语义独立生成，输出保持 Serialize shape；运行时 validators 继续校验 IPC payload。Generator 16 项测试、feature Clippy、71 command/40 event drift checks、Rust fmt/check/clippy/workspace tests、UI check/874 tests/build 均通过。阶段 9 Windows profile/VM 发布验收仍开放。

2026-09-29 阶段 9 验收进展（ADR 0395）：完成文件型 SQLite/WAL 容量曲线、retention/WAL/VACUUM 行为及 `SQLITE_FULL` 原子失败/重试。最新 workspace release app 在独立 APPDATA profile 首启成功，schema v30、integrity check 与外键检查通过；将 `user_version` 设为 29 的旧版本号 fixture 被明确拒绝，按重置步骤删除 DB/WAL/SHM 后配置保留、新库重建成功。`tauri_plugin_single_instance` 要求同一 Windows 用户下顺序运行 profile。真实 UI 交互受 CUA auth token unavailable 阻断；机器缺 `makensis.exe`，当前用户 winget 安装未找到适用 NSIS，尚未构建当前安装包或测试安装升级/卸载。旧 MSI/NSIS 仍是 2026-09-14 产物；上述 smoke 不等同物理盘 ENOSPC 或 GUI 发布验收。

2026-09-29 Session 存储容量策略收口（ADR 0404）：保留按 `created_at` 清理整场会话的 90 天默认与 `0` 禁用语义，不裁剪保留期内的 `session_events`，不做自动 `VACUUM`，不宣称每会话/数据库字节上限或最低磁盘需求。容量探针补充按生产 TranscriptRecord 字段形状构造的合成混合事件分布；比例和文本长度只是工程场景，不代表真实用户数据，Windows 文件型 SQLite 观测值见 ADR 0395。event store 的事务体和 `COMMIT` 失败均尝试回滚，且只广播已成功提交的事件；SQLite `SQLITE_FULL` 与 disk I/O failure 在会话错误界面给出不同恢复提示。Windows 物理盘 ENOSPC 和真实桌面恢复体验仍待 disposable profile/VM 验收。

2026-09-29 配置契约清理（ADR 0405）：删除旧 `[memory].history_retention_days` 搬迁、旧配置名/权限 detector 与明文凭据导入；当前配置直接反序列化，未知字段及磁盘上的明文凭据会备份原文件并以默认值启动。安全凭据引用仍在启动时 hydrate；不匹配版本时只重建 `config.toml`，数据库、媒体和其它数据可以保留。
