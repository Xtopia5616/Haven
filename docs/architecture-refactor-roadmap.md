# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；SessionUsage 累计上限契约、session-scoped KV 孤儿清理 owner、summary marker 单一原子生产路径、Tools 测试归属、Input→Tools 测试反向依赖、MCP/Skill 直调授权策略来源、MCP 管理操作网络策略来源、X12 例外消息写入口、ADR 编号索引完整性、actorless session ToolRun lifecycle 清理（ADR 0507）、Ask reducer state ownership 收口（ADR 0508）、终态 Ask 清理事件归属（ADR 0509）、ReAct phase 来源 session 身份（ADR 0510）、Windows 子进程先入 Job 再恢复（ADR 0513）、显式 end 失败重试契约（ADR 0514）、Skill venv 子进程 containment（ADR 0515）、MCP prompt index 类型化（ADR 0516）、Memory fact marker generation-safe ack 与有界 durable outbox/session recovery（ADR 0107/0259）、App-owned Shell/hotkey/VAD stop 后处理调度收口（ADR 0517）、MemoryWorker durable outbox lifecycle 私有 owner（ADR 0518）、Session grant/resolve append failure retry 契约测试（ADR 0519）、SessionActor interaction replay fail-closed（ADR 0520）、Session confirmation 决议与恢复唤醒原子提交（ADR 0521）、Pending Session 恢复失败后的退避重试（ADR 0522）、Router/media 共享构造（ADR 0525）、Admin 共用风险等级单一来源（ADR 0528）、单一 `session:lifecycle` 契约与 Memory Fact response DTO（ADR 0529）、类型化 ToolRun lifecycle events（ADR 0530）已完成；当前 Active 为全项目领域术语与架构角色命名收敛（§5.7），暂无 Next；Windows 发布验收为独立开放签核门
> 更新日期：2026-10-07
> 范围：Agent/Session、Memory、Tools、LLM、App IPC 与 UI

本文件只记录当前阶段、仍未完成的工作和验收条件。历史实现切片与决定见对应 ADR；提交历史用于追踪实现过程。

## 1. 目标

减少重复的业务状态与跨层所有权，让持久化、运行时、UI 投影和安全决策各有明确 owner。重构允许删除测试版本中的旧契约，但任何数据、配置、IPC 或安全语义变更都必须有可复核的迁移/重置说明。

## 2. 当前架构基线

- `session_events` 是会话 transcript 的恢复与回滚权威；`messages`、`session_steps`、用量记录和 UI 事件是投影或实时通道。ReAct snapshot 已删除。
- 一个 session 的可变运行态由 `SessionActor` 持有；一次运行的 `ReActState` 是 run-local scratch。
- Agent、Tools、Memory、LLM 和 App 之间通过 typed stores、ports 与 composition root 接线；动态 JSON 限于 provider、MCP、Skill、配置扩展和工具参数/结果等明确边界。
- Settings/model 配置应用、后台/定时任务、MemoryRuntime 与 Rust→TypeScript IPC 契约已有各自的 owner 和 ADR。精确职责以[架构文档](architecture.md)、[跨层输出契约清单](architecture-output-contract-inventory.md)及对应 ADR 为准。

## 3. 长期不变量

1. 一个稳定业务事实只有一个权威来源；投影不得反向成为恢复来源。
2. durable event 只有成功提交后才能发布；外部副作用在事务外执行，并通过稳定 identity 处理迟到或重复结果。
3. Rust/数据库/事件字段使用 snake_case；只在跨端边界映射为 camelCase。
4. 授权必须在副作用执行前由后端 owner 校验；UI 不能绕过安全网关。
5. 删除旧入口时，连同无调用的兼容分支、测试和文档入口一起删除，并说明受影响数据如何处理。
6. 每项重构按独立目标审查，跑适用门禁；阶段完成记录在对应 ADR，不在本文件追加逐日实施日志。

## 4. 分阶段状态

### 阶段 0：基线、契约和观测（先行）

**已完成。** 固定工具链、测试隔离、IPC 校验和可复跑的行为/性能基线已建立；验证细节见对应 ADR 与 CI 配置。

### 阶段 1（已完成）：SessionActor 成为唯一热运行态 owner（P0）

**已完成。** SessionActor 持有会话状态与 active run；actor loop 同时推进运行与 mailbox。见 ADR 0214、0382、0390。

### 阶段 2（已完成）：恢复、回滚和事件/投影边界再收口（P0）

**已完成。** `session_events` 是恢复权威；pending input 的持久路由、Ask 恢复、回滚截断和提交后 UI 发布各有明确边界。待投递输入在 durable marker 中保存 answer/follow_up 路由，恢复不重算；见 ADR 0207、0209、0210、0385、0416、0440、0458。

### 阶段 3（已完成）：存储 domain ports 与 typed projection（P1）

**已完成。** Agent 生产路径通过 typed stores/ports 访问会话、记忆和任务数据；跨层输出清单记录保留的动态 JSON 边界。见 ADR 0383、architecture-output-contract-inventory.md。

### 阶段 4（已完成）：Tools capability runtime 与 Agent ports（P1）

**已完成。** 工具执行、目录、授权和服务适配经显式 ports 接入；组合根负责构建 manager-backed adapters。见 ADR 0384、0388。

### 阶段 5（已完成）：Runtime config apply ownership（P1）

**已完成。** 配置写入 gate、apply plan、部分失败与重启恢复语义已有唯一 owner。见 ADR 0351、0372。

### 阶段 6（已完成）：LLM Router 请求对象化（P2）

**已完成。** 请求描述、能力路由、执行器与 usage owner 的契约已审计收口。见 ADR 0327–0329、0354。

### 阶段 7：统一 Job 生命周期与 MemoryRuntime（P2）

**已完成。** MemoryRuntime 由应用管理；scheduled dependency 恢复和终态 transcript 投影复用既有 outbox。通用 Job executor、统一 ToolRun deadline、owner token/续租和自动 replay 不在当前设计范围。见 ADR 0367、0392、0393。

### 阶段 8：IPC 单源生成与 UI 编排收口（P2）

**已完成其定义范围。** Rust handler/Serde DTO 生成 TypeScript command contracts；聊天 ask/input、启动恢复与滚动/observer 编排分别委托稳定 owner。事件运行时校验与授权策略仍由各领域按变更持续审查，不构成待补的全局 codegen 阶段。见 ADR 0394。

### 阶段 9：Common 边界与性能复核（条件式）

**状态：条件式候选，不是必须完成的最后一阶段。** 已有性能 profile、文件型 SQLite 容量观测及 `SQLITE_FULL` 注入测试；后续 Common 拆分或性能优化只在依赖图、重复 owner 或同负载 profile 提供明确收益证据时立项。桌面发布验收是独立签核门，见 §5.1；已有基线不能代替当前安装包验收。见 ADR 0359–0361、0404。

## 5. 未完成事项

状态按执行性质区分：**Gate** 是发布前签核条件；**Active** 是当前唯一实施切片；**Next** 是已有证据、等待前一切片完成后再进入的结构目标；**Candidate** 尚未进入实现队列，只有证据与退出条件明确后才立项。证据未达到准入条件时允许没有 Active/Next；在 §5.6 的复核触发点到来前不制造实现任务。结构性目标一次只推进一个 Active slice；行数下降、文件变少或 crate 变少不是完成指标。

| 状态 | 当前项 |
|---|---|
| **Active / Next** | **Active：全项目领域术语与架构角色命名收敛**，见 §5.7。本轮已开始全仓盘点 Rust、Svelte/TypeScript、IPC 与架构文档；先建立统一词表，再基于 owner/生命周期/不变量证据逐域决定保留、改名或合并。暂无后续 Next。 |
| **Gate** | Windows 发布验收 Open，见 §5.1。 |

### 5.1 Windows 发布验收（Gate / Open）

按 ADR 0395 的验收范围，在隔离的 Windows profile 或 VM 上使用当前构建完成并记录：

- 首启、Settings、会话、工具确认、媒体、后台/定时任务、恢复与回滚的真实 UI 流程；
- 当前安装包的安装、升级、卸载，以及卸载后的用户数据保留行为；
- 物理磁盘空间耗尽时的错误分类、用户提示和恢复操作。

用最新代码和安装产物复跑适用的 Rust/UI 门禁，并在 ADR 中记录实际构建版本、schema、环境、结果与限制。ADR 0395 中早期 profile 和门禁结果是当时的历史证据，不能当作当前工作树或当前安装包的通过结论。

可以提前准备隔离 profile、安装器和物理盘耗尽环境；最终验收必须使用交互生命周期等 IPC/UI 变更完成后的最新构建。此项是发布签核门，不阻塞不影响发布路径的独立模块整理。

### 5.2 交互生命周期所有权（契约与恢复失败语义均已完成）

[`ADR 0424`](adr/0424-interaction-lifecycle-ownership.md)（配套 [`ADR 0423`](adr/0423-confirmation-wait-expiry-and-acknowledgement.md)）已完成 Session、ScheduledToolRun 与 AppCommand 三类确认 owner 的收口：Session 按 `session_id` 定位 actor，ScheduledToolRun 按 `tool_run_id`，AppCommand 按 request ID；resolve 不跨 owner 扫描。运行时 owner 显式传递，session durable event 形状不变；确认期限由 owner 按绝对 `expires_at` 仲裁，ScheduledToolRun 的批准/取消由持久执行 claim first-wins 仲裁。Session resolve append 成功后才推进 actor，批量确认与 Paused 状态在同一 SQLite 事务提交。Session grant 与 resolve event 仍是两次可重试 durable write；重启时 running ToolRun 不自动 replay。旧 sentinel、fallback 与 renderer `timed_out` 入口已删除，不增加 schema/reset。交互 replay 错误不再伪装为空 actor，见已完成的 ADR 0520；实现、替代方案、退出条件和门禁记录见 ADR 0423/0424/0519/0520。

**非原子窗口复核（2026-10-06；ADR 0519 已完成）：** Session-owned resolve 路径先持久化 session-scope grant 并更新当前授权引擎，再由 SessionActor 追加 `interaction_resolved`。本轮以 SQLite 故障注入固定现有契约：第二步失败返回可重试错误，已批准 grant 仍持久且在当前授权引擎可见，原 actor 的请求保持 pending/Paused，不产生 resolved event；恢复写入后同一请求可重试完成且不重复 grant。**不据此启动事务重构**，不改变 grant-before-resolve 顺序、SessionStore/SessionActor owner 或其他确认 owner 契约。actor 在两步间停止的 stale/retry 语义受 actor registry 与 mailbox 时序影响，契约尚未定义，暂时 Deferred。

**交互 replay 故障收紧（ADR 0520，已完成）：** ADR 0294 曾规定 replay 失败时 warning 后用空交互注册 actor；这与 ADR 0502 确立的“交互 lifecycle 只从 durable events replay”恢复权威不一致。`install_actor` 现先 replay，成功后才恢复该安装路径上的 durable session grants 并注册 actor；replay 失败向调用者传播，actor 保持未注册，使显式加载可重试。`load_pending_sessions` 对单个会话的安装失败记录 `session_id` 并继续其余 pending sessions；pending 列举本身失败仍传播。故障回归验证坏 event 不会经失败的 actor 安装路径启用 grant，坏会话不会阻断健康会话恢复。全局 Security apply 的 grant 重应用继续遵循 ADR 0402；本切片不增加自动重试，不改 event payload、schema、IPC 或其他确认 owner。适用 workspace 门禁均已通过。

**最新切片结果（ADR 0521，已完成）：** 最后一个 pending confirmation 的 resolve/expiry event 与 Paused→Pending 现由 SessionActor 在同一事务提交；提交后才更新 actor 状态，SessionSupervisor 再发布一次 `SessionResumed`、入队并唤醒。SQLite status trigger 故障注入证明失败时 event/request/status/wake 均不前移，同 request 重试成功；多项确认与 expiry 路径也有回归覆盖。适用 workspace 门禁已通过。

**最新切片结果（ADR 0522，已完成）：** Immediate dispatcher 与 deferred App bootstrap 现在共用整批恢复重试 owner；退避由 250ms 指数增长至最多 30s，直到取消。首次 deferred 读取由 ApplicationRuntime 持有，bootstrap 等待其结果；发生错误后注册可取消 retry task，并仍能进入 Ready。Pending actor 的重试扫描会幂等补入队并唤醒，单 actor replay 错误不触发整批重试。Agent 回归覆盖瞬时批次错误后只 dispatch 一次、空批次成功、backoff 取消和队列 wake；AppState 注入 deferred 首次错误，验证 retry 注册、Ready 和 shutdown join。适用 workspace 门禁通过。步骤 0 重看后，`session_events.rs`、ToolRunService、crate/API 与性能候选仍没有准入证据，保留 Deferred；依赖图仍为 11 个 crate、30 条单向边。

### 5.3 内部模块与 crate 边界（按证据复核）

本节只保留未解决项与已复核候选的重开条件；已完成切片的背景、决定和验证以对应 ADR 为准。私有模块整理与 crate 拆分都必须通过 §5.5 准入，不以文件或 crate 体量为目标。

#### 已复核候选与已知限制

| 状态 | 问题与当前证据 | 重开条件与边界 |
|---|---|---|
| **高风险 Candidate，暂缓** | `session_events.rs` 同时协调 event append、transcript projection、rollback、cache invalidation 与 commit 后 broadcast。历史基线快照为 2026-10-05、HEAD `e81af24`：5,520 非空物理行（2,989 production / 2,531 tests），当时近 45 天 68 次提交触及。代码复核基线 `c34a977`（2026-10-06）复算为 5,551 非空行（3,020 production / 2,531 tests）；近 45 天共 69 次提交触及。历史基线之后只有 `f72e5fe` 修改该文件，增加 34 行分页 session-id 高水位、cancellable page 与 memory cursor baseline 适配入口；event append、transcript projection、rollback 与事务边界未变。此前复核的 `3bc807d`、`b0e47ba`、`a9b03a4` 是一次 SessionStore 边界收口中的不同缺口，`408564a` 将 confirmation CAS 与 event append 保持在同一事务 owner；截至该代码复核基线未发现修复后同类事务不变量再次回归。`230a2e3` 已将只读历史测试移入 `session_history` 测试子模块。 | 仅在事务核心与无关 façade 反复耦合修改、相同原子性/rollback 缺陷修复后复发，或剩余测试无法按真实职责隔离且能证明收益时重开。event append、projection、rollback、cache invalidation 和 post-commit broadcast 继续由单一 SessionStore 协调，不暴露事务内部或引入第二恢复来源（[ADR 0466](adr/0466-session-history-read-facade-module.md)、[ADR 0479](adr/0479-session-history-test-ownership.md)）。 |

**显式 end 的基线观察（实施前）：** actorless 路径清理失败会返回错误且不推进 session 状态，但多条 ToolRun 可能已部分取消；idle/running Actor 路径先永久取消 actor lifetime，随后 best-effort 清理失败只记日志，仍写 `Completed`，Tauri 成功事件因此发出。run-exit 会移除 Actor，但不重试 ToolRun 清理；失败的 scheduled ToolRun 可能仍保持 Waiting 并保留 timer。claim 已获胜的 ToolRun 按 ADR 0424 保持运行，不属于取消错误。上述契约与实现已由 [ADR 0514](adr/0514-explicit-session-end-failure-contract.md) 完成并通过联合门禁；background 仍保留 ADR 0507 定义的有限 best-effort 重试。

契约、主要协调决定与验收结果见 ADR 0514；end 的数据库状态、Actor/run、Tauri event 和 UI selection 已联合验证，覆盖 actorless、resident/idle、running/stuck run、单项失败、多项部分失败、重试、claim-wins/cancel-wins、confirmation 与 direct-run admission。此处不再保留 Active 项。

#### 已复核但不进入 Next

- **Tools 与安全边界：** `tool_contract.rs` 继续作为共享执行契约 owner；`builtin/admin.rs` 由 AdminServices 承接副作用，Admin 保留 operation/request/output contract；messaging adapter 继续复用 `haven_messaging`。只有稳定后再次出现 policy/schema drift、同边界回归或独立消费者，才重新评估私有模块（[ADR 0213](adr/0213-operation-spec-single-policy-source.md)、[0391](adr/0391-admin-services-typed-output-projections.md)、[0396](adr/0396-messaging-domain-crate.md)、[0506](adr/0506-mcp-admin-connection-network-policy.md)）。
- **工具运行时三个集合各自保留（已复核，暂不合并）：** `ToolRegistry` 是已安装工具的权威注册表，保留注册顺序、拒绝重复名并维护 global version；`DeferredToolCatalog` 保存尚未激活、供发现和按需加载使用的定义；`SessionToolOverlay` 是某个 session 当前可执行的附加工具集合，loader batch 负责幂等、预算准入与 session version。三者分别表达注册、延迟发现和 session 执行作用域，不能因 lookup/list/definition projection 外观相近而统一成一个通用 Catalog。Provider 定义、校验、manifest 与执行的共享 turn-level immutable view 已由 `ToolCatalogSnapshot` 承担；共同规则若真实重复并导致漂移，再评估窄 helper。ADR 0145/0148 的 provider visibility 与 session 隔离不变（本轮名称对齐见 [ADR 0533](adr/0533-tool-runtime-nomenclature-alignment.md)）。
- **MCP prompt index 类型化（已完成，ADR 0516）：** 固定的 `name/tool_names` 摘要由 `McpServerIndexEntry` 沿 Tools→App adapter→Agent prompt port 传递；工具数从 names 派生，capability resolver 不再解析拼接描述。该类型不序列化到 IPC/provider/MCP wire；工具 schema 和结果保留 dynamic JSON。只有该投影新增稳定字段或出现新的独立消费者时再复核。
- **Admin 风险等级 parity 与单一来源（ADR 0512、0528 已完成）：** ADR 0512 为 20 个 model/native 共用操作补齐完整操作集合和风险等级 parity 门禁，native-only `mcp_reconnect` 与 `mcp_refresh` 保留独立测试。随后 ADR 0528 将共享风险等级收敛到 `OperationContract.risk_override`：Admin typed metadata 按规范 operation 名读取同一风险值，缺少显式风险时 fail closed 为 High；parity 门禁继续覆盖共享操作集合和 contract 完整性。两个 native-only 操作仍独立为 Medium。没有修改现有风险值或确认语义。
- **Windows 子进程 containment 启动顺序（已完成，ADR 0513）：** MCP stdio、Shell、Skill 与后台 ToolRun 过去都在进程已运行后才加入 kill-on-close Job，MCP 还在 spawn 后才创建 Job；因此子进程可能在加入前派生不受 Job 管理的后代。`haven-platform::ProcessContainment` 现在负责命令挂起标志、Job 分配、唯一初始线程核对与恢复，失败时终止进程；adapter 继续拥有命令策略、管道与取消/等待生命周期。Windows 测试覆盖挂起时不执行、运行后派生后代并由 Job 回收，以及线程发现失败时 fail closed。若新增受管进程入口绕过此 API或出现进程树残留回归，再重开审查。
- **Agent 与 Memory：** SessionActor 继续独占可变 session state；ReAct stream/checkpoint/retry 保持协同；MemoryRuntime、worker、maintenance store 与 fact inference 按既有 owner 分工。Fact marker generation-safe ack 与有界 durable outbox 页面、持久退避、事实/摘要公平调度和有界 session recovery 已完成（ADR 0107/0259）。交互恢复/队列计数、buffer 顺序、prompt prefetch 等其余边界仍按新证据重开（[ADR 0214](adr/0214-react-run-inside-session-actor.md)、[0424](adr/0424-interaction-lifecycle-ownership.md)、[0468](adr/0468-memory-worker-maintenance-pass-module.md)、[0475](adr/0475-single-source-fact-sensitivity-rules.md)、[0476](adr/0476-react-turn-owns-search-context-projection.md)、[0481](adr/0481-remove-summary-marker-only-enqueue.md)）。
- **MemoryWorker durable outbox owner（已完成，ADR 0518）。** durable `MemoryStore`、scanner、retry/ack 与 lifecycle 状态收口到私有 `MemoryOutbox`；`MemoryWorker` 保留组合 facade、推理、prefetch 与 MEMORY-fence dirty 状态。Outbox 使用三方法 inference handler，不持有完整 Worker；共享 root cancellation 保持 scanner/prefetch shutdown 边界。marker、64 项分页、公平调度、CAS、退避、poison repair 与 restart recovery 均保持。没有 schema/API/IPC 或 crate 变化。若剩余 prefetch/inference 后续出现重复取消、容量回归或跨职责耦合，再依 §5.5 单独复核，不按文件行数继续拆分。
- **App 与 UI：** AppState/runtime、Composer/InputRouter 与 Ask/reducer/event owners 近期未发现稳定后重复边界回归；Ask 响应结算、终态 Ask 清理和 execution phase 来源身份已收口（[ADR 0508](adr/0508-ask-response-reducer-ownership.md)、[0509](adr/0509-terminal-ask-cleanup-event-owner.md)、[0510](adr/0510-session-scoped-react-execution-phase.md)）。App 冷启动与配置热更新重复构造 Router/STT/TTS/OCR/ImageGen 的实现已由 [ADR 0525](adr/0525-shared-router-media-client-builder.md) 收到 App 私有 builder：逐能力错误仍由启动 caller 降级或由热更新 prepare 拒绝，发布顺序仍由各 caller 持有。配置或 client 类型扩展时只需维护一份构造参数映射；若后续出现旧响应覆盖新状态、跨 session 状态泄漏或同一 lifecycle 回归，再按证据复核其他 owner；不提取仅按页面/operation 分类的模块。
- **Tauri 输出 DTO：** 2026-10-05 复核发现 history/search 已将 Memory `Session` 映射为 App-owned `SessionRecordDto`；`SkillInfo` 在 Skills crate 中明确定义为 bridge/UI snapshot；`Fact` 仍由 `list_facts`/`add_fact` 直接用 Memory repository 类型序列化，但 ADR 0357 将 Rust `Fact` 明确规定为 wire authority，前端通过命名 contracts 消费现有字段。当前没有字段意外暴露、DTO 漂移或独立 wire 消费者的回归证据，因此维持现有边界，不进入 Next。只有需要不同于存储实体的 renderer 字段/命名、发生未审阅的字段外泄/破坏性变化，或出现独立消费者时，才评估 App-owned Fact DTO 与显式 mapper（[ADR 0357](adr/0357-memory-command-contract-boundary.md)；完整输出分类见 [跨层输出契约清单](architecture-output-contract-inventory.md)）。MCP refresh 把内部 `McpReconcile` 收窄为 `McpRefreshPlan`，不会把连接配置送过 IPC；`list_mcp_tools` 的 `McpServerSnapshot` 保留 settings editor 需要的 command/args/cwd/url 并遮蔽 env 值。当前按已存在的编辑契约保留；字段范围或 renderer 隐私要求变化时重新审查（完整边界见输出契约清单）。
- **Common 与 ToolRunService：** 依赖图仍是 11 个内部 crate、30 条单向边且无环（按 `docs/architecture.md` 的直接依赖清单复核）。Common 作为广泛复用的基础类型 crate 保持现状。`ConfigService` 是 ADR 0068 规定的有限有状态例外，只拥有配置快照、串行 typed patch、原子持久化及不含密钥的变更通知；运行时应用仍由 App 装配。开发规范已与该边界对齐。ToolRunService 仍与 Tools 的执行策略、`haven_memory::ToolRunStore`、Agent 授权/完成投影及 App 生命周期形成纵向调用链。2026-10-06 步骤 0 复核了 `34bab6f`、`b9fe655` 与 `19beef7` 的跨层改动：它们分别收口 scheduled claim/cancel 仲裁、actorless session ToolRun cleanup 和显式 end 重试，属于不同契约，已由 ADR 0424/0507/0514 验收；目前没有同一不变量在收口后再次回归的证据，因此这是热点观察信号，不是拆分 Next。若同一 claim/cancel/cleanup/end-retry 不变量再次回归、actorless 与 resident 路径重新出现语义分歧、出现重复状态 owner/反向依赖，或有独立消费者与同环境可复核的维护/构建收益，再重开此候选（[ADR 0068](adr/0068-versioned-config-service.md)、[ADR 0359](adr/0359-common-boundary-and-profiling-baseline-audit.md)、[ADR 0424](adr/0424-interaction-lifecycle-ownership.md)、[ADR 0507](adr/0507-session-owned-action-cleanup.md)、[ADR 0514](adr/0514-explicit-session-end-failure-contract.md)）。

**Crate 体量基线（2026-10-05）：**按 workspace `.rs` 文件非空物理行粗略统计，含注释；测试按测试路径及 `#[cfg(test)]` 模块归类，不是 AST 指标。

| Crate | 生产行 | 测试行 | 合计 |
|---|---:|---:|---:|
| `haven-tools` | 37,214 | 20,416 | 57,630 |
| `haven-agent` | 28,521 | 22,296 | 50,817 |
| `haven-llm` | 15,237 | 13,289 | 28,526 |
| `haven-memory` | 14,155 | 11,394 | 25,549 |
| `haven-app-binary` | 13,178 | 5,399 | 18,577 |

这些数字保留作规模背景，不构成拆分依据；优先看生产职责、消费者、依赖方向和可复核维护收益。

### 5.4 Common 拆分与性能优化（Candidate）

只有依赖图、重复 owner 或可复现 profile 表明存在明确收益时，才另立拆分/优化任务。crate 拆分须证明独立稳定 API、单向依赖边界及实际消费者收益；不得只为减少文件行数、构建目录或 crate 大小而拆 crate。性能比较使用相同工作负载、数据规模和环境记录前后结果；没有明显改善则关闭候选，不继续微调。

### 5.5 长期候选准入与复核

长期结构治理按“有证据的问题队列”推进，不预设日期或全仓重写目标。候选只有在出现下列至少一项时才进入审查：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、某热点在多个变更中频繁发生跨职责修改、依赖图暴露反向/多余依赖，或同负载 profile 显示可复现的资源/延迟问题。文件超过约 800 行或多于两个独立职责只触发复核，不单独证明要拆。

每个候选的短 ADR/评估要记录：现有 owner 与不变量、生产代码和测试的职责分布、调用/依赖边界、预期收益及观察方式、破坏面和停止条件、适用门禁、回滚方式。实施顺序固定为：证据确认 → 接受目标与切片 → 迁一条垂直调用链 → 删除旧入口 → 跑影响面门禁 → 对比 owner/依赖/性能指标 → 独立提交并更新状态。若只移动代码、扩大公共 API、暴露事务内部、增加第二权威来源，或验证不能证明维护/运行收益，立即停止并把候选记为“不需拆分/暂缓”。

长期执行按触发信号驱动而不按日历制造工作，优先级为数据/安全/生命周期不变量故障、重复跨 owner 回归或修改耦合、依赖/API 边界问题、最后才是有同负载证据的性能优化；行数和 crate 大小不计为准入分。仓库较大时可并行委派只读审计（例如依赖图、热点职责、事务/安全不变量），可由 sub-agent 或用户授权的其他对话承担；委派范围优先只读、交付具体文件/函数与反例，主执行者必须回到源码和门禁独立核验，不把审计结论直接当成实现要求。任何时刻只实现一个 Active 切片；跨对话协作不改变路线图、契约 owner 或提交责任。审查没有合格候选时，保留“无 Active 切片”状态并等待新证据，再继续同一套复核流程。

### 5.6 长期滚动执行周期（跨迭代周期）

本路线按问题证据推进，不按日历、crate 数或文件行数承诺完工。已完成切片的设计、替代方案、验证与回滚分别记录在 ADR；本节只保留未来推进顺序和当前落点，避免把完成日志变成第二份历史索引。

| 步骤 | 长期工作流 | 进入条件与交付物 | 退出条件 |
|---|---|---|---|
| 0 | **证据复核与分流（每轮入口）** | 在切片完成、同类回归出现、依赖/API 变化或准备发布时复核。形成一项候选说明：问题、不变量、owner、源码/历史证据、影响面、停止条件和适用门禁；依 §5.5 选择 Next、Deferred、关闭或无候选。 | 只有一个 Next 或明确无候选；不把体量、单次审计或旧历史快照直接转成 Active。 |
| 1 | **契约与生命周期收口（按证据推进）** | 显式 end 失败与重试（ADR 0514）、Skills venv 子进程 containment（ADR 0515）及 App-owned recording stop 调度（ADR 0517）已完成；下一轮须先经过步骤 0 证据复核，再决定是否进入本步骤。 | actorless、驻留 Actor、运行中 Actor、ToolRun claim 先后竞争和持久化失败都有一致且可测试的可见结果；direct run 从 Paused 入场先持久化 Running；rollback/continue 不得在 end closing marker 后改写 durable state；end 对 stuck run 仍响应，ADR 0424 first-wins 不变。 |
| 2 | **权威来源与跨层不变量** | 检查持久状态、事件、运行态、投影和 UI 是否仍各有单一 owner；只有真实漂移、明确风险对应的失败注入缺口、重复回归或绕过权威入口时才切片。2026-10-05 的 ReAct Fatal 双终态 producer 已收口：dispatcher 专用入口过滤 AgentEvent 重复错误，SessionSupervisor 的 `SessionSupervisorEvent` 经共同 TauriEmitter 投影，直接 run API 保留原行为（[ADR 0511](adr/0511-session-terminal-error-single-owner.md)）。后续交付仍是窄范围回归/故障测试、冲突入口删除和不变量文档更新。 | 回归固定不变量且不增加第二真源；跨 crate/跨端变更通过相应完整门禁。 |
| 3 | **稳定 owner 的职责收口** | MemoryWorker durable outbox lifecycle 已由 ADR 0518 完成：提取私有垂直 owner、收窄 inference capability、局部重试规则测试归属 Outbox，真实 Worker↔Outbox 恢复/ack/cancellation 组合测试留在 Worker。后续候选须先经过步骤 0 证据分流。 | 调用和测试落到真实职责 owner，重复规则或跨边界修改减少，外部契约、依赖方向及运行语义保持不变；收益不能说明则关闭候选。 |
| 4 | **模块成熟后再评估 crate/API 边界** | 只有模块 owner 已稳定，且存在独立消费者、真实依赖方向问题或可复核构建/迭代成本时才评估 crate 拆分。交付物包括依赖图、API/消费者映射；若声称构建收益，须有同环境基准。 | 提取后依赖单向、API 稳定、消费者不用反向依赖或重复 adapter，并证明维护/构建收益；任一不满足就保留现边界。 |
| 5 | **性能与容量** | 仅在同负载 profile 复现有用户意义的成本时优化；交付物为固定场景的前后指标，并遵守 SQLite 容量、失败恢复与资源上限契约。Windows 发布验收独立保留在 §5.1，不作为结构重构阶段的退出依赖。 | 优化结果超过噪声且达到目标，否则关闭候选；没有当前测量就不以“降复杂度”为名做性能改动。 |

以上是循环复核的先后顺序，不是一次性瀑布项目：每轮从步骤 0 重新分流，完成一个切片、同类回归出现、依赖/API 边界变化或准备发布时再审查证据。ADR 0514–0522、ADR 0524–0525、ADR 0528 与 Memory fact marker generation-safe ack/有界 outbox（ADR 0107/0259）已完成；ReAct Fatal 双终态 owner 已在步骤 2 收口（ADR 0511）；Admin 风险等级 parity 门禁由 ADR 0512 完成，单一风险来源由 ADR 0528 完成。ADR 0520–0522 分别收紧 interaction replay fail-closed、confirmation event/status 原子提交和 pending-session recovery lifecycle；ADR 0524 统一一次性 ToolCall 与持久 ToolRun 的跨层命名；ADR 0525 统一 Router/media client 构造实现并保留 caller 错误策略。完成 ADR 0522 后，步骤 0 重新复核，`session_events.rs`、ToolRunService、crate/API 与性能候选仍未达到准入条件；当前 Active 已转为 §5.7 的全项目领域术语与架构角色命名审计。准备发布时仍单独关闭 Windows Gate（§5.1）。

**当前执行位置：** 架构阶段 0–8 已完成；ADR 0514–0522、0524–0525、0528 与 Memory fact marker generation-safe ack、有界 outbox/session recovery（ADR 0107/0259）均已完成。当前 Active 是 §5.7 的全项目术语/架构角色命名审计；其它结构候选仍按 §5.5 的触发信号分流，不按 crate 体量制造拆分任务。`session_events` 大文件候选与 ToolRunService lifecycle 均因近期没有收口后同类回归/独立消费者收益而 Deferred；三层 ToolRegistry scope 保留独立的准入、排序和版本语义，不做通用容器拆分；依赖边界为 11 个内部 crate、30 条单向边，无需更改方向。ReAct Fatal 双终态发布 owner 已由 ADR 0511 收口；Admin 20 个共用操作的 parity 门禁由 ADR 0512 收口，风险等级由 ADR 0528 统一到 `OperationContract`，两项 native-only 操作仍单独测试。Windows 发布验收仍是独立 Open Gate。

### 5.7 全项目领域术语与架构角色命名收敛（Active）

范围覆盖所有 Rust crate、Svelte/TypeScript、Tauri IPC/事件，以及对应架构/命名/ADR 文档；不是只整理 Tools 命名，也不以批量替换后缀为目标。

当前第一切片是建立全仓词汇基线：为领域实体、跨层契约、架构角色后缀和常见函数动词定义单一含义，并盘点同义多名、同名异义、真实职责重叠和仅共享外观的类型。`docs/naming.md` 已新增首版架构角色词汇与动作动词约定；它们用于本轮审计和迁移，存量命名是否符合仍须逐域核对。

#### 首轮全仓符号扫描与候选分流

| 范围 | 证据与调用边界 | 当前分类 / 下一步 |
|---|---|---|
| Tools runtime | `SessionToolOverlay` 保存 session 当前可执行的附加工具；`ToolAuthorizationRequestResolver` 解析操作契约但不裁决权限；工具定义查询使用完整 tool-definition 名称。调用链覆盖 Tools、Agent、App adapter 与 prompt context。 | **已对齐名称**；授权仍由 `AuthorizationEngine` 决定，三种集合 owner 保持分离（ADR 0533）。 |
| Skills `SkillsEngine` 名称、通用快照查询与位置式 watcher fingerprint | 该类型本身声明为发现 Skill 的 registry，实际持有按名称索引的 Skill、enablement allowlist 与 catalog version；消费者覆盖 Tools、Agent prompt 和 App。`list/get` 分别返回 `SkillInfo` snapshot，`get_skill/list_skills` 返回 runtime Skill；文件 watcher 原返回 `(PathBuf, SystemTime, u64)`。 | **已收敛 Skills crate API**：类型改为 `SkillRegistry`，snapshot 查询改为 `list_skill_infos` / `get_skill_info`，enabled filter 改为 `enabled_skill_allowlist`；watcher 返回具名 `SkillFileFingerprint`。运行执行权仍在 Tools，App wire shape 与 config key 不变（ADR 0554）。 |
| Memory query cache | `QueryResultCache` 是 Database 持有的有界进程内 TTL/LRU 与 generation cache；不执行 SQL、不拥有 durable 写入。 | **已对齐名称**，与持久 `*Store` 分开，保持原失效语义（ADR 0534）。 |
| LLM 的 STT 适配 | `LlmSttClientAdapter` 把 provider `LlmClient` 转接为消费者所需的 `SttClient`，没有额外桥接状态或独立生命周期。 | **已对齐名称**，保留两种客户端契约及现有 provider dispatch（ADR 0535）。 |
| Tools 对外入口 | `ToolsFacade` 组合多个 Tools owner，并由 Agent/App adapter 提供窄 ports；它暴露 execution/catalog/config/runtime/asset 调用，但不拥有 MCP、Skill 等资源的创建/重连生命周期。 | **已对齐名称**：Rust crate API、`facade.rs` 模块、Agent/App adapter 与构造入口统一使用 facade 角色；无 Tauri/IPC 变化（ADR 0536）。 |
| UI metrics contract | `generatedCommands.ts` 从 Rust `MetricsSnapshot` 生成固定响应字段；原 settings alias 却将相同响应退化为开放 `Record<string, unknown>`，丢掉已知字段类型。 | **已对齐名称与类型**：`PerformanceMetricsSnapshot` 以 generated DTO 为已知契约并与开放索引签名交叉，既能类型化访问已有字段，也保留未来扩展字段；移入 diagnostics contract，不改变 IPC（ADR 0537）。 |
| Common / LLM `RequestPolicy` 同名 | Common config 的 `RequestPolicy` 把 logical request 映射到 primary model；LLM 内部 `request_pipeline::RequestPolicy` 捕获一次调用的 retry 与 timeout runtime snapshot。 | **已改名**：LLM 私有执行快照叫 `RequestExecutionPolicy`；配置字段/JSON 名和路由选择不变，两个 owner 保持分离（ADR 0539）。 |
| Common / App `HotkeyConfig` 同名 | Common 的 `HotkeyConfig` 是实际生效并持久化的用户快捷键配置；App `desktop::HotkeyConfig` 只在 `ShellState::default` 中写入、无生产读取者，字段还保留 recording/toggle 双绑定旧形状。 | **已清理**：删除未使用的 ShellState 副本与孤立默认值测试；Common 配置和快捷键注册路径不变（ADR 0540）。 |
| LLM / Memory `LlmCallUsage` 同名 | LLM 的类型是单次 provider 调用的运行时元数据；Memory 同名类型则是包含实体 ID、session、step、成本和时间戳的持久明细，并作为 resume IPC DTO 生成 TS 类型。 | **已改名**：Memory 持久明细为 `LlmUsageRecord`，追加输入为 `LlmUsageRecordInput`；生成类型名随之清晰，JSON 字段、数据库与事件 payload shape 不变（ADR 0541）。 |
| Agent / Memory `SessionEvent` 同名 | Agent 枚举是 SessionSupervisor 向 App 广播的进程内交互/生命周期副作用通知；Memory 类型是按 sequence 持久化、供 replay/rollback 使用的事件行。Agent 原有 `DurableSessionEvent` re-export 暴露了二者命名冲突。 | **已改名**：Agent 广播类型为 `SessionSupervisorEvent`；Memory 持久行以 `SessionEvent` 从 Agent root 暴露，生产者、订阅者和 durable owner 不合并（ADR 0542）。 |
| UI `TaskKind` 把会话/执行模式/ToolRun 分类混在一起 | 前端类型额外包含 `foreground` 并将其映射为“会话”，但 `ToolRun` 生成契约仅有 `background/scheduled`；`foreground` 是执行方式，session 是对话实体。生产界面只渲染持久 ToolRun 分类。 | **已改名并收窄**：删除本地 `TaskKind` 与无实际调用的 `foreground` 映射，标签表直接以生成 `ToolRunKind` 为键；记录 execution mode 与 ToolRun kind 的术语边界（ADR 0543）。 |
| Memory `SessionEventStore` 过渡别名 | `SessionEventStore` 与 `SessionStore` 是完全相同的 Rust 类型；生产代码已使用 `SessionStore`，别名只剩测试、历史 ADR 和 crate re-export。 | **已移除**：测试与 Agent/Memory 导出统一使用 `SessionStore`；历史 ADR 保留旧名称以记录当时决策（ADR 0544）。 |
| Tools `ToolBox` 名称与单值共享句柄不符 | `ToolBox` 实际为 `Arc<dyn Tool>`，用于单个实现的注册、查找、授权及跨 crate 传递；不是集合，也不是 `Box<dyn Tool>`。 | **已改名**：统一为 `ToolHandle`，文档定义其共享引用角色；动态 dispatch、引用计数和 registry 生命周期不变（ADR 0545）。 |
| UI `ToolSource` 同名异约束 | `toolIdentity.ts` 的 `ToolSource` 是 builtin/skill/MCP 三类归一标签；`toolManifest.ts` 同名类型来自后端 manifest，parser 则保留任意非空字符串以便兼容未来来源值。 | **已改名**：开放契约值叫 `ToolManifestSource`，闭合的展示分类仍叫 `ToolSource`；parser 接受范围和 UI 分类行为不变（ADR 0546）。 |
| App `ConfigApplyGate` 过渡别名与 coordinator 字段名 | `ConfigApplyGate` 已是 `RuntimeConfigCoordinator` 的同类型别名；`ApplicationRuntime.config_apply_gate` 实际提供配置提交与 Router/media prepare/publish 协调能力，而 `RuntimeServices` 与 AdminContext 中同名成员才是原始共享 mutex。 | **已收敛**：移除内部兼容别名，将 ApplicationRuntime 成员改为 `config_runtime_coordinator`；底层 mutex 仍明确叫 `config_apply_gate`，加锁与配置应用顺序不变（ADR 0547）。 |
| Memory `StoredBranchPoint` 位置元组 | 恢复边界将事件 sequence、transcript cursor、step identity、`last_msg_at` 作为四元组跨到 Agent；rollback 查询也按 tuple index 读取，使双时钟和身份语义只能靠位置辨认。 | **已改为具名结果**：改为 `ActiveBranchPoint`，命名各字段并只保留所需的 `event_sequence`；恢复、回滚选择和投影 cutoff 规则不变（ADR 0548）。 |
| UI 集合读取 wrapper 使用 `get*` | `getTools()` 返回全部内置工具清单，`getSessions()` 返回会话摘要集合；同层对 MCP、Skills 等集合读取使用 `list*`，且它们不是按 key 读取单项。 | **已对齐动词**：wrapper 与 `ChatSessionStartup` 依赖端口改为 `listTools` / `listSessions`；保留 `get_tools`、`get_sessions` IPC 命令及返回形状（ADR 0549）。 |
| Memory 持久查询的多行结果使用 `get_*` | Memory `Database` 的 session messages/steps/usage、pending inputs 与 facts 查询返回 `Vec`；其中 facts 查询还与同层 `list_facts` 并存，而 `get_fact_by_id` 明确返回单项。消费者覆盖 Memory、Agent 与 Tools；进程内 cache getter 则按 cache key 返回一个槽值。 | **已对齐动词**：领域多行查询统一为 `list_*`（如 `list_session_messages`、`list_facts_by_subject`）；单项 `get_*` 与 cache-key `cache_get_*` 保留，SQL、排序、过滤、缓存及恢复语义不变（ADR 0551）。 |
| Agent 队列消费入口使用 `get_*` | `SessionSupervisor::get_follow_ups` / `get_steering` 实际调用 actor 的 `drain_*`，取回并清空待处理队列；返回空集合的第二次调用也被测试覆盖。 | **已对齐副作用动词**：公开入口与测试统一改为 `drain_follow_ups` / `drain_steering`，队列优先级、容量和消费时机不变（ADR 0552）。 |
| History IPC 集合读取使用 `get_history` | App handler 返回 `Vec<SessionRecordDto>`，但下游 `SessionStore::list_history`、UI 调用和同域分页/搜索动词表达的都是集合读取。生成契约将该命令传播到前端，contracts registry、IPC 文档和 output inventory 均登记当前名字。 | **已对齐跨层动词**：Tauri command、generated contract 与 UI wrapper 统一为 `list_history` / `listHistory`；请求响应 shape、分页、投影和错误语义不变（ADR 0553）。 |
| UI reducer 与 usage presentation 的 `SessionTokenStats` 同名 | reducer 类型要求完整累计状态与必需字段；presentation 类型则接受稀疏、字段可选的显示输入。`+page.svelte` 因而临时将展示类型导入为 `PresentationSessionTokenStats`。 | **已区分角色**：reducer 保留 `SessionTokenStats`，展示输入统一命名为 `SessionTokenStatsView`；字段和展示结果不变（ADR 0550）。 |
| UI runtime store 的作用域与状态类型归属 | `runtimeStateStore.ts` 同时声明 session ReAct 执行阶段、选中会话状态标签和 `RecordingOverlayState`；后者的唯一可变 owner 实际是 `recordingOverlayController`。`activeConversationStatusStore` 及 `WorkspaceStatus.conversationStatus` 也指向产品定义的 session。 | **已澄清 owner 与术语**：模块改为 `sessionRuntimeStore.ts`，展示值统一叫 `activeSessionStatusLabel`；`RecordingOverlayState` 移至录音控制器。ReAct phase 与选中 session 展示状态仍分开，各自作用域和消费者不同（ADR 0555）。 |
| UI session intent 的目标类型与存储键作用域 | `sessionIntentStore.ts` 同时保存一次性历史导航目标和显式新会话意图，但 `ResumeTarget` / `resumeTargetStore` 未标明实体，`NEW_ACTION_INTENT_KEY` 未说是哪种操作；本地存储实际记录的是“启动新会话时禁止自动恢复”的标记。 | **已明确实体与生命周期**：目标改为 `SessionResumeTarget` / `sessionResumeTargetStore`，键常量改为 `NEW_SESSION_INTENT_STORAGE_KEY`，localStorage 的既有字符串保持不变；SessionRail 空状态统一称“会话”。一次性恢复目标和跨重启新会话标记保留各自生命周期，不合并（ADR 0556）。 |
| 最近会话恢复 IPC 命令使用 conversation 术语 | `get_last_conversation` 返回 `Option<SessionResumeResponse>`，消费方只在 chat startup 加载最近的 `Session` transcript；底层读取已名为 `SessionStore::latest_session_record`。 | **已统一跨层术语**：命令、wrapper、测试、生成契约、安全目录、IPC 文档和输出清单改为 `get_latest_session_for_resume`；恢复顺序、响应 DTO、错误语义不变，无旧命令 alias（ADR 0557）。 |
| Session-scoped 授权使用“对话”描述作用域 | `PermissionScope::Session`、`session_id` 持久键和会话授权页面已明确使用 session；确认弹窗、设置页空状态、App 校验错误与多个 Rust 注释却称作 conversation。 | **已统一作用域词汇**：用户文案统一为“本会话允许/拒绝”，校验文字与注释明确 persisted/owning session；授权 key、scope、写入条件与撤销范围均不变（ADR 0558）。 |
| Input / App RecordingState 同名 | `haven_input::RecordingState` 是 Pending/Recording/Processing 采集生命周期枚举；App `get_recording_state` 响应是 shell/UI 的 `is_recording`、`is_toggle` 快照。两者是不同 owner、不同状态空间。 | **已改名**：App DTO 改为 `RecordingStatus`，保持 command 与 JSON 字段不变；区分 App wire view 和 Input lifecycle state（ADR 0538）。 |
| 配置 apply 计划与协调 | `RuntimeConfigApplyPlan` 从变更域映射 live/restart target；`SettingsApplyPlan` 再展开设置命令的有序 phase；`SettingsRuntimeApplyCoordinator` 持有 phase、失败和 router 发布观测。 | **保留并解释**：共享 target 投影，但执行顺序/失败观测 owner 不同，后两者由前者派生而非复制 apply 状态；合并会混淆配置影响映射与 settings 顺序流程。 |
| Chat UI 事件模块 | `createChatEventController` 管 listener 注册/释放；Session、Agent、Interaction、Usage handler 分别把不同事件映射到 reducer、局部 store 或通知。 | **保留并解释**：controller 管注册生命周期，handler 管分域事件投影；handler 之间职责、payload 和副作用不同，不因同一 chat route 合并。 |
| 其他已扫角色 | `McpManager`、`VenvManager` 各自拥有连接/环境资源生命周期；`ConfigService` 拥有串行 config patch 与持久化；Memory repositories 中的 `*Store` 持有 SQLite 访问；UI `InteractionOwner` 会在 boundary 转成 snake_case wire owner。 | **保留并解释**：后缀/同名本身不足以证明重复；UI 与 wire owner 分开是明确的字段转换边界，MCP/venv 的 Manager 也符合生命周期语义。 |

此表是候选分流清单，不是完整符号目录。尚未完成的 crate、IPC/event payload、UI controller/store 与函数动词审计仍在 §5.7 范围内；完成一域后更新本表并以 ADR 记录实际迁移。

每个候选必须记录源文件、真实消费者、状态 owner、生命周期/作用域、失败与恢复语义，以及是否触及 IPC/持久化/安全契约，并归类为：**保留并解释、改名、合并、拆分或暂缓**。只有职责、权威来源与生命周期确实重复的部分才合并；不同状态作用域即使共享数据类型或方法外形也可保留独立 owner。公共 Rust API、IPC、事件、数据库字段与用户可见术语分别遵守既有版本化/兼容与重置要求。

实施按领域切片：先完成符号/术语清单与依赖/消费映射，再确认候选，逐条迁移并更新调用点、测试、命名规范和架构文档；跨 crate、跨端或改变契约时按开发标准补 ADR 并运行相应门禁。首个 Tools 切片已将 per-session 执行集合命名为 `SessionToolOverlay`、将授权请求准备者命名为 `ToolAuthorizationRequestResolver`，并把取风险/策略/请求的 `get_*` 改为 `resolve_*`、将缩写 API `list_defs`、`list_enabled_builtin_defs` 与 `select_tool_defs_for_budget` 改为完整 tool-definition 名称（[ADR 0533](adr/0533-tool-runtime-nomenclature-alignment.md)）。这是局部迁移，不能视作 Tools 或全仓审计完成。每个切片完成后更新本节状态；全仓通过条件是：主要生产概念均有唯一规范词和可定位 owner，确认的重复职责完成合并或有明确暂缓理由，所有保留的相邻边界均能从命名与文档解释其不同之处。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
