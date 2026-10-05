# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；SessionUsage 累计上限契约、session-scoped KV 孤儿清理 owner、summary marker 单一原子生产路径与 process 流读取测试归属已收口；当前无 Active 结构切片；Windows 发布验收为独立开放签核门
> 更新日期：2026-10-05
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

**已完成。** MemoryRuntime 由应用管理；scheduled dependency 恢复和终态 transcript 投影复用既有 outbox。通用 Job executor、统一 action deadline、owner token/续租和自动 replay 不在当前设计范围。见 ADR 0367、0392、0393。

### 阶段 8：IPC 单源生成与 UI 编排收口（P2）

**已完成其定义范围。** Rust handler/Serde DTO 生成 TypeScript command contracts；聊天 ask/input、启动恢复与滚动/observer 编排分别委托稳定 owner。事件运行时校验与授权策略仍由各领域按变更持续审查，不构成待补的全局 codegen 阶段。见 ADR 0394。

### 阶段 9：Common 边界与性能复核（条件式）

**状态：条件式候选，不是必须完成的最后一阶段。** 已有性能 profile、文件型 SQLite 容量观测及 `SQLITE_FULL` 注入测试；后续 Common 拆分或性能优化只在依赖图、重复 owner 或同负载 profile 提供明确收益证据时立项。桌面发布验收是独立签核门，见 §5.1；已有基线不能代替当前安装包验收。见 ADR 0359–0361、0404。

## 5. 未完成事项

状态按执行性质区分：**Gate** 是发布前签核条件；**Next** 是下一项结构目标；**Candidate** 尚未进入实现队列，只有证据与退出条件明确后才立项。结构性目标一次只推进一个 active slice；行数下降、文件变少或 crate 变少不是完成指标。

### 5.1 Windows 发布验收（Gate / Open）

按 ADR 0395 的验收范围，在隔离的 Windows profile 或 VM 上使用当前构建完成并记录：

- 首启、Settings、会话、工具确认、媒体、后台/定时任务、恢复与回滚的真实 UI 流程；
- 当前安装包的安装、升级、卸载，以及卸载后的用户数据保留行为；
- 物理磁盘空间耗尽时的错误分类、用户提示和恢复操作。

用最新代码和安装产物复跑适用的 Rust/UI 门禁，并在 ADR 中记录实际构建版本、schema、环境、结果与限制。ADR 0395 中早期 profile 和门禁结果是当时的历史证据，不能当作当前工作树或当前安装包的通过结论。

可以提前准备隔离 profile、安装器和物理盘耗尽环境；最终验收必须使用交互生命周期等 IPC/UI 变更完成后的最新构建。此项是发布签核门，不阻塞不影响发布路径的独立模块整理。

### 5.2 交互生命周期所有权（Complete）

[`ADR 0424`](adr/0424-interaction-lifecycle-ownership.md)（配套 [`ADR 0423`](adr/0423-confirmation-wait-expiry-and-acknowledgement.md)）已完成 Session、ScheduledAction 与 AppCommand 三类确认 owner 的收口：Session 按 `session_id` 定位 actor，ScheduledAction 按 `action_id`，AppCommand 按 request ID；resolve 不跨 owner 扫描。运行时 owner 显式传递，session durable event 形状不变；确认期限由 owner 按绝对 `expires_at` 仲裁，ScheduledAction 的批准/取消由持久执行 claim first-wins 仲裁。Session resolve append 成功后才推进 actor，批量确认与 Paused 状态在同一 SQLite 事务提交。Session grant 与 resolve event 仍是两次可重试 durable write；重启时 running action 不自动 replay。旧 sentinel、fallback 与 renderer `timed_out` 入口已删除，不增加 schema/reset。后续只在出现新回归或职责变化时重开；实现、替代方案、退出条件和门禁记录见 ADR 0423/0424。

**非原子窗口复核（2026-10-05；已知限制，不进 Active）：** Session-owned resolve 路径先持久化 session-scope grant 并更新当前授权引擎，再由 SessionActor 追加 `interaction_resolved`。若第二步失败，授权仍生效、请求仍 pending、该次请求的原工具调用不启动，renderer 因命令错误保留待处理卡片；进程重启会同时恢复持久 grant 与仍 pending 的交互。现有 ADR 已明确不承诺这两次写入原子性，当前没有稳定后重复回归。暂未找到专门覆盖“grant 成功、resolve append 失败”或其间 actor 停止的故障注入测试；这是已知验证缺口，不单独触发事务重构。只有当产品契约要求 resolve 错误意味着授权也未接受，或出现 grant 与 UI/执行状态冲突造成的实际回归时才重开；届时先固定失败语义并补齐故障注入，再评估同一 SQLite 事务内的 grant+event commit，继续由单一 SessionStore/SessionActor 协调，不拆分事务 owner。

### 5.3 内部模块边界整理（持续按证据复核 / 当前无 Active 切片）

这不是 crate 拆分目标；只在职责与稳定 owner 边界能证明维护收益时做私有模块整理。已完成切片的实现范围、验证与回滚记录以 ADR 为准：SessionStore 只读历史 façade 及测试归属（[0466](adr/0466-session-history-read-facade-module.md)、[0479](adr/0479-session-history-test-ownership.md)）、session-scoped KV 孤儿清理 predicate 单一 owner（[0480](adr/0480-session-kv-orphan-cleanup-owner.md)）、LLM provider schema projection（[0467](adr/0467-llm-tool-schema-projection-module.md)）、Memory maintenance pass（[0468](adr/0468-memory-worker-maintenance-pass-module.md)）、managed-media 生命周期与 producer/GC/Files 登记协调（[0469](adr/0469-app-managed-media-lifecycle-module.md)、[0470](adr/0470-generated-media-write-gc-gate.md)、[0473](adr/0473-files-rich-path-generated-media-gc-gate.md)）、录音 ID 交接和 Shell overlay controller（[0471](adr/0471-recording-session-id-handoff.md)、[0472](adr/0472-recording-overlay-controller.md)）、架构依赖清单门禁（[0474](adr/0474-architecture-dependency-inventory-gate.md)）、Memory fact sensitivity 规则单源化（[0475](adr/0475-single-source-fact-sensitivity-rules.md)）、ReAct 搜索响应投影归入 turn owner（[0476](adr/0476-react-turn-owns-search-context-projection.md)）。

**本轮完成 — [ADR 0476](adr/0476-react-turn-owns-search-context-projection.md)：**不依赖 stream state、仅由 turn response 处理调用的 server-side search context 投影与 outcome 已移入 `turn.rs`，identity 回归随实现迁移。StreamForwarder、队列、checkpoint、重试和 mixed tool+search 的既有时序留在原 owner；该切片的实施边界、测试与回滚见 ADR 0476。

**本轮完成 — [ADR 0477](adr/0477-agent-action-result-delivery-module.md)：**Agent 的 background/scheduled-result delivery consumer、专属 session-status helper、不可信结果 envelope formatter 及 formatter 测试移入私有 `layer/action_result_delivery.rs`。`ActionService` 仍拥有 completion outbox 与 ack 能力，SessionSupervisor 仍拥有队列/状态，ReAct 仍拥有 live transcript 的 durable projection；scheduled-fire 执行路径留在原处。当前无 Active 结构切片。

**本轮完成 — [ADR 0479](adr/0479-session-history-test-ownership.md)：**只读历史 façade 的六个行为测试移入 `session_events::session_history::tests`，使查询实现与其 API/查询语义回归在同一模块定位。测试 fixture 保持内存数据库；title 写入与缓存失效、聚合恢复投影及 append/rollback 原子性测试仍留在各自 owner。没有暴露测试 helper 或改动生产契约。

**本轮完成 — [ADR 0480](adr/0480-session-kv-orphan-cleanup-owner.md)：**`sessions::delete_old_sessions` 与 Memory maintenance 原先各自维护相同的 orphan session-scoped `kv_store` DELETE/owner 解析规则；现由 `kv_store` 的 connection-level helper 持有唯一 SQL，两个调用点继续使用各自已有连接。历史上新增 event cursor 与 episode marker 时，两份谓词曾需同步修改；本切片消除该重复 owner，不改变删除时序或事务边界。

当前边界决定：Common 拆分维持 [ADR 0359](adr/0359-common-boundary-and-profiling-baseline-audit.md) 的暂缓结论；Tools crate 拆分没有独立依赖边界或消费者收益；SessionStore 继续独占 event append、投影和 rollback 事务协调，`event_cursor` 与 `last_msg_at` 双时钟、提交后发布均不得分散；`LOCAL_TOOL_SECURITY_MATRIX` 仍是生产权限提示的 operation 白名单，保留在 `security.rs`。管理 surface、LLM router、授权沙箱和 inbox 崩溃恢复边界按现有 owner 保留，具体依据见相关 ADR。

**2026-10-05 Tools 热点复核（均不准入拆分）：** `tool_contract.rs` 共 2,468 行，生产契约到第 1,940 行，后续为同模块测试。它把 `Tool` / typed adapter、operation policy、result metadata 与执行协议放在同一个共享执行契约 owner；registry/security 已在 `6b22da2` 拆出，`OperationSpec` 单源策略及 manifest 契约由 [ADR 0213](adr/0213-operation-spec-single-policy-source.md) 固化。近期跨 contract/view/builtin 的共同修改是在收敛该契约，尚无稳定后重复漂移或独立消费者收益；进一步搬入 sibling modules 只会改代码位置。若 policy/schema drift 再次导致回归，或出现独立消费者，再重新评估。

`builtin/admin.rs` 共 3,665 行，生产 operation/schema/native request/output 边界到第 1,615 行，其余为测试。`AdminServices` 的副作用和固定输出生产者已位于 `admin_services.rs`；`admin.rs` 继续单独拥有 operation contract、request bridge 与工具输出序列化，符合 [ADR 0391](adr/0391-admin-services-typed-output-projections.md)。近期修改覆盖 MCP 授权、诊断日志上限、Skill 名称校验等不同纵向功能，没有显示稳定 owner 后的重复边界故障。若 MCP 管理授权/刷新反复回归，或出现独立复用方，再评估 `admin/mcp.rs`；当前不拆五类 surface。

`builtin/messaging.rs` 共 2,518 行，主测试模块从第 1,326 行开始。生产部分是单一模型可见 `agent` 工具：共享参数和 15 个操作适配至 `haven_messaging::MessagingService`；领域消息生命周期已在 [ADR 0069](adr/0069-messaging-service.md) 收口，并由 [ADR 0396](adr/0396-messaging-domain-crate.md) 提取为独立 crate。近期没有再次出现跨层重复 owner 或稳定后边界回归。仅按 operation 拆 schema/handler 或另拆 crate 暂无收益；若 schema 与执行适配之后独立演进并导致契约漂移，再复核私有模块边界。

`memory_worker.rs` 按非空行统计为 3,176 行（约 1,367 行生产代码、1,809 行测试）。近期已按 [ADR 0468](adr/0468-memory-worker-maintenance-pass-module.md) 隔离定期 maintenance pass，并在 `880ec96` 将 pass 构造器收窄为显式四项 capability；此后没有足够历史证明要继续拆。`MemoryRuntime` 持有恢复与调度，worker 持有 extraction/outbox，`MemoryMaintenanceStore` 与 `fact_inference` 分别持有持久化和提案 gate；现有测试 fixture 与 outbox 测试共享较多。prefetch 失败重试与事实/marker 原子提交的缺陷已各自修复一次，没有稳定后重复回归，因此不再拆 prompt-prefetch 或搬测试。另发现的三个无 workspace 生产调用 summary marker-only API 已由 [ADR 0481](adr/0481-remove-summary-marker-only-enqueue.md) 删除；测试 fixture 现通过 episode+marker 原子入口建数据，Worker/Store/Database 的读取、恢复、ack 与清理能力保留。该清理没有形成进一步拆分 `memory_worker.rs` 的理由。

**Crate 体量与边界复核（2026-10-05）：**按 workspace `.rs` 文件非空物理行粗略统计（含注释；测试按测试路径及 `#[cfg(test)]` 模块归类，非 AST 指标），Rust 源码约 222k 行。最大 crate 为 `haven-tools`，但体量同时来自多种内建能力与测试；依赖图本身仍是 11 个 crate、29 条单向内部边、无环，并与架构清单一致。

| Crate | 生产行 | 测试行 | 合计 |
|---|---:|---:|---:|
| `haven-tools` | 37,214 | 20,416 | 57,630 |
| `haven-agent` | 28,521 | 22,296 | 50,817 |
| `haven-llm` | 15,237 | 13,289 | 28,526 |
| `haven-memory` | 14,155 | 11,394 | 25,549 |
| `haven-app-binary` | 13,178 | 5,399 | 18,577 |

本次将 `ActionService` 独立成 crate 的想法评估后关闭为“现阶段不需拆分”：Agent 仍直接依赖 Tools 的 tool/auth 契约；ActionService 还共用 Tools 内部 shell/process/output policy，依赖 `haven-memory::ActionStore` 的持久状态，并与 Agent 的授权执行、完成投影及 App 生命周期形成现有纵向调用链。抽离需要新增更低层的进程/输出边界或 port，可能增加反向依赖和策略重复；近期 ActionService 与 ActionStore、Agent、App 的联动是 action 生命周期纵向演进，没有稳定后仍反复耦合 registry/security 的证据。只有出现真正不依赖 Tools 的 Action 消费者、同一边界回归重复发生，或受控构建/profile 证明拆分能降低实际迭代成本时才重开；它不进入 Active 队列。

已复核热点包括 `+page.svelte`、`SettingsView.svelte`、`admin.rs`、`llm/router.rs`、`inbox.rs`、Tools/Agent 根模块、`react/mod.rs`、`layer.rs`、`session/mod.rs`、`resume.rs`、`session/tool_runner.rs`、`react/stream_step.rs`、`commands/session.rs`、`app_state.rs`、`MemoryView.svelte`、`ToolsView.svelte`、`ToolResultCard.svelte`、`InputRouter.svelte` 与 `+layout.svelte`。`tool_runner.rs` 的近期 churn 属于 ADR 0424 同一轮 owner 收口，确认与 ActionService 分持待决请求和执行/取消 claim，当前保留原边界；`stream_step.rs` 的流生命周期队列、checkpoint 与 retry 需保持协同，搜索响应投影则由本轮 ADR 0476 收回 turn owner。`resume.rs`、ToolsView 和 InputRouter 保留各自会话恢复、管理页与统一 composer 边界。InputRouter 的异步附件读取曾允许发送越过读取完成点且并行读取可能超限，现已阻止读取期间提交并预留附件名额，回归由 UI 测试覆盖。ToolResultCard 同时展示 ask 与 tool output，但 ask selection、pending interaction、dismissal 分属 controller/reducer/page owners；ADR 0430 的 reopen 路径近期只经历一次纵向改动，AskInteractionCard 提取留作条件候选，待跨职责重复 churn 或回归再启动。MemoryView 单次 resume 参数遗漏已在 `487ff9e` 修复；`+layout` phase store 订阅清理也已修复。

**观察项复核（2026-10-05；本轮均未批准实现切片）：**

- **Tools 根模块 helper：** 文件约 402 行，近 45 天热点没有形成 helper 的重复跨职责修改；保留 crate 级共享规则与现有 feature owners。只有相关 churn 或回归复现后才考虑搬移。
- **ReAct media projection：** `types` / `react` 存在双向模块调用；Text fallback 会对同一输入确定性地重算一次 media plan。共同的 append helper 已避免 live/replay 分叉，目前没有性能 profile 或功能回归证据支持拆投影 owner或优化重算。若证据出现，优先评估将 event→canonical/round projector 与媒体 helper 一并收归 ReAct projection owner；单独优化时让一次投影同时返回 `ContentPart` 与表示元数据，并保留 snapshot-safe `MediaInput`、稳定 `asset_id`、路径脱敏及 durable MediaPlan 边界。
- **启动编排：** 9 月的 readiness 调整曾达到历史复核门槛；当前由 AppState 决定启动顺序、ApplicationRuntime 管任务生命周期、AgentLayer 管 dispatcher、bootstrap 触发并提供 Tauri emitter，边界已由架构文档和 ADR 对齐，之后未见同一路径重复回归。
- **`getTools()`：** `+layout` 与 ToolsView 双读服务不同生命周期，Rust manifest 源和前端 mapper/snapshot 各只有一个 owner；未发现旧响应覆盖新值或 UI 漂移。

以上候选只有在 §5.5 所列回归、重复 owner、调用边扩张或可复现成本出现后重开；纯行数减少不构成准入理由。

事件存储与 transcript projection 的写侧拆分仍是暂缓的高风险 Candidate。2026-10-05 复核时，`session_events.rs` 为 6,078 行（3,075 production / 3,003 tests），近 45 天有 66 次提交触及（约 +7,003/-925）；改动主要是 9/22–25 一轮 SessionStore owner 收口，以及 9/30–10/5 的 durable session facts 纵向演进，未发现边界稳定后事务不变量反复回归。体量和 churn 足以触发复核，但不证明搬进 child module 会降低维护成本。只读历史 façade 已由 [ADR 0466](adr/0466-session-history-read-facade-module.md) 拆出；后续只有观察到事务核心与无关 session façade 反复耦合修改、同一原子性/rollback bug 重复修复，或测试无法按真实职责隔离并能证明模块收益时，才重开实施评估。若准入，优先评估不参与 canonical event/projection transaction 的 lifecycle façade 和对应测试；event append、projection、rollback、cache invalidation 与提交后 broadcast 继续由同一 SessionStore owner 协调。若候选要求上层分别写 event/projection、暴露事务内部、引入第二恢复来源，或只有行数下降，立即停止。

`haven-tools/builtin/admin.rs` 的五个管理 surface 契约不按 operation 数量拆分；`llm/router.rs` 已有 request/stream executor；`security.rs` 拥有授权、receipt、禁用 operation 和路径沙箱；`inbox.rs` 的 registry、mailbox、archive 与崩溃恢复共用文件锁，暂不拆。热点约 800 行时先区分生产与测试代码，再记录保留理由或明确拆分边界；只搬行数不立项。内部整理保持外部 API、wire、schema 和运行语义不变，并独立提交。

**补充边界复核（2026-10-05；模块拆分均暂缓）：**

- `SessionActor` 的命令、队列和调度共同读写唯一 `SessionState`，受公平调度及事件先提交后更新约束（[ADR 0214](adr/0214-react-run-inside-session-actor.md)、[0382](adr/0382-session-state-owns-react-run.md)、[0390](adr/0390-session-actor-fairness-and-bounded-release.md)、[0424](adr/0424-interaction-lifecycle-ownership.md)）。只有交互恢复缺陷重复出现或队列容量计数反复漂移时，才评估 actor 内的 `ContextQueues` owner。
- `facts.rs` 的生产部分保留稳定 Database 外观和跨写入、查询、维护共用的谓词规则；图写入、事实查询、维护和敏感规则已有清晰 owner，其大部分文件体量是契约/组合测试（[ADR 0019](adr/0019-memory-fact-graph-write-boundary.md)、[0020](adr/0020-memory-fact-query-ranking-boundary.md)、[0022](adr/0022-memory-fact-maintenance-boundary.md)、[0475](adr/0475-single-source-fact-sensitivity-rules.md)）。
- `embeddings.rs` 同时含底层向量与 episode FTS SQL，但由现有 recall owner 组合，历史未见 FTS 与向量策略反复共改（[ADR 0303](adr/0303-agent-memory-embedding-store-port.md)、[ADR 0304](adr/0304-agent-memory-recall-store-port.md)）；仅在过滤/排序规则出现重复 owner、同一边界引发回归、出现独立消费者或同负载 profile 暴露成本时重开。
- Agent `event.rs` 的 buffer/有序 chunk pipeline 与 durable transcript 提交、Tauri adapter、UI validator 分属不同阶段 owner，近期改动属于各自契约收口（[ADR 0336](adr/0336-react-session-committed-submission.md)、[ADR 0404](adr/0404-session-event-capacity-retention-and-recovery.md)）；只有 buffer/reset/tombstone 顺序重复回归或稳定职责反复跨域共改时，才评估搬入私有子模块。审计另确认未调用的 `EventDispatcher::emit_compaction_from` 会保留一条绕过 `CommittedUiPublisher` 的直接发布入口，已由 [ADR 0482](adr/0482-remove-unused-compaction-event-emitter.md) 删除；提交后的 Compaction 仍只由 durable sequence publisher 产生。
- `react/mod.rs` 约 1,988 行，其中约 1,283 行生产代码；它是 ReAct 能力组合 façade，turn、tool batch、retry、stream identity、usage tracker 与 transcript 等 owner 已按既有 ADR 分开。`record_tool_usage` 与 media usage 有相似字段映射，但分别服务 tool diagnostic batch 和 media per-call persistence，写入语义不同；当前无字段漂移或重复回归，不抽共享 mapper。若同一 usage 字段多次漏同步，或媒体投影策略分叉导致回归，再评估窄的共享规则 owner；单纯拆 settings/media/usage 文件不立项（ADR 0214、0223、0278、0382、0384、0388、0444、0476）。

这些文件不因体量进入 Active；入口、生产/测试分布与重开条件已经复核。

### 5.4 Common 拆分与性能优化（Candidate）

只有依赖图、重复 owner 或可复现 profile 表明存在明确收益时，才另立拆分/优化任务。crate 拆分须证明独立稳定 API、单向依赖边界及实际消费者收益；不得只为减少文件行数、构建目录或 crate 大小而拆 crate。性能比较使用相同工作负载、数据规模和环境记录前后结果；没有明显改善则关闭候选，不继续微调。

### 5.5 长期候选准入与复核

长期结构治理按“有证据的问题队列”推进，不预设日期或全仓重写目标。候选只有在出现下列至少一项时才进入审查：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、某热点在多个变更中频繁发生跨职责修改、依赖图暴露反向/多余依赖，或同负载 profile 显示可复现的资源/延迟问题。文件超过约 800 行或多于两个独立职责只触发复核，不单独证明要拆。

每个候选的短 ADR/评估要记录：现有 owner 与不变量、生产代码和测试的职责分布、调用/依赖边界、预期收益及观察方式、破坏面和停止条件、适用门禁、回滚方式。实施顺序固定为：证据确认 → 接受目标与切片 → 迁一条垂直调用链 → 删除旧入口 → 跑影响面门禁 → 对比 owner/依赖/性能指标 → 独立提交并更新状态。若只移动代码、扩大公共 API、暴露事务内部、增加第二权威来源，或验证不能证明维护/运行收益，立即停止并把候选记为“不需拆分/暂缓”。

长期执行按触发信号驱动而不按日历制造工作，优先级为数据/安全/生命周期不变量故障、重复跨 owner 回归或修改耦合、依赖/API 边界问题、最后才是有同负载证据的性能优化；行数和 crate 大小不计为准入分。仓库较大时可并行委派只读审计（例如依赖图、热点职责、事务/安全不变量），但审计结论须由主执行者回到源码与门禁核验，任何时刻只实现一个 Active 切片。审查没有合格候选时，保留“无 Active 切片”状态并等待新证据，再继续同一套复核流程。

### 5.6 长期滚动顺序（无日历承诺）

本路线是跨多个迭代周期的决策路径，不按“把所有大 crate 拆小”设完工日期，也不为每个大文件预留一次拆分。完成一个切片、出现同类回归或准备发版时重新审查证据；阶段表示先后依赖，未满足进入条件就停在当前阶段。

1. **已完成：SessionUsage 累计范围与重建一致性（[ADR 0478](adr/0478-session-usage-saturation-contract.md)）。** 结合 live `UsageTracker` 的 `u32::saturating_add` 和 `AgentUsage` 累计字段类型，确定 session summary 封顶于 `u32::MAX`；增量写入、legacy summary 读取和 detail 重建现已收敛到该契约。没有改变 schema 或 wire 类型。
2. **已完成：只读历史 façade 测试归属（[ADR 0479](adr/0479-session-history-test-ownership.md)）。** 六项查询语义测试随 `session_history` 私有模块归组；事务、历史缓存写失效与聚合恢复测试仍在各自 owner。
3. **已完成：session-scoped KV 孤儿清理单一 owner（[ADR 0480](adr/0480-session-kv-orphan-cleanup-owner.md)）。** retention purge 与 Memory maintenance 共用 `kv_store` 的 connection-level 清理 predicate。
4. **已复核暂缓：Tools 执行契约、Admin surfaces 与 messaging builtin。** 三者均达到职责复核线，但当前各有稳定 owner，抽取子文件不会形成更清晰的依赖边界。重开条件见 §5.3；行数、局部 churn 和 operation 数都不足以准入。
5. **已完成：移除 summary marker-only enqueue 入口（[ADR 0481](adr/0481-remove-summary-marker-only-enqueue.md)）。** Worker、Store 与 Database 的三处旧写 API 已删除，episode+marker 的原子写入成为唯一创建路径。独立 episode ack、session cleanup、worker retry/cancel 与 ReAct producer 的门槛和提交后 wake 保持不变；没有拆 `memory_worker.rs` 或 crate。
6. **已完成：移除未调用的 Compaction 直发 helper（[ADR 0482](adr/0482-remove-unused-compaction-event-emitter.md)）。** 删除无调用方的 `CompactionEventData` 与 `EventDispatcher::emit_compaction_from`，避免恢复一个绕过 durable commit 与 `CommittedUiPublisher` 的第二发布路径；Compaction wire event 和现有生产路径不变。
7. **已完成：process 流读取测试归属（[ADR 0483](adr/0483-process-stream-reader-test-ownership.md)）。** 六项不依赖 ActionService 的 cap/drain/tail/UTF-8 测试移入共享实现 owner `process.rs`；服务生命周期与终态投影测试继续留在 ActionService 测试模块。

#### 长期执行台阶与决策门

以下 A–E 是长期治理的执行台阶，不改变上文阶段 0–9 的架构阶段编号。

| 台阶 | 目标与进入条件 | 完成或停止条件 |
|---|---|---|
| A. 证据队列 | 先处理数据、安全、生命周期不变量问题；再审计重复 owner、同一边界的重复回归与不稳定调用边。`memory_worker.rs`、Tools `builtin/messaging.rs`、`tool_contract.rs` 与 Admin surfaces 已完成只读复核；对 `memory_worker.rs` 的审计另找出旧公开 marker-only API，与 ADR 0266/0299 的原子生产者决定冲突。 | 每个候选记录唯一问题、owner、证据与停止条件。没有合格证据就保持无 Active，不把文件复核自动升级成拆分任务；发现与既有不变量冲突的未调用入口时，允许按窄范围删除旧契约。 |
| B. Crate 内 owner 收口 | 仅当一个私有子域有独立稳定职责，且跨职责共改或回归能由该边界解释时，迁移一条完整垂直调用链。优先保持现有 crate API、事务、安全和恢复 owner 不变。ADR 0481/0482 是按既有 owner 决定删除冲突或未调用旧入口的窄切片；ADR 0483 是按被测实现 owner 收口测试的窄切片；它们均不构成逐文件拆分配额。 | 旧入口与重复规则删除；测试靠近真实 owner；行为和依赖方向不变；适用 crate 门禁通过，且审查能指出维护或正确性收益。若只是搬文件、测试难以独立验证或要暴露内部状态，则关闭候选。 |
| C. Crate 边界复核 | 只有内部模块 owner 稳定后，或依赖图出现真实问题，才重新评估 `haven-tools`、`haven-agent` 等较大 crate。先证明独立消费者、稳定 API、单向依赖和不重复业务策略；构建/开发成本收益要用同一环境的可复核对比。 | 提取后依赖图仍无环且更贴近业务消费者，消费者无需反向依赖或重复 adapter， workspace 门禁通过，并能说明收益。缺少独立消费者或收益不可测就不拆 crate；不设 crate 数或行数目标。 |
| D. 性能与容量 | 只有可复现的延迟、内存、磁盘或并发问题进入 profile；保留现有 SQLite 容量与失败恢复不变量。 | 同数据、负载、构建和环境比较前后指标；没有超过噪声且对用户有意义的改进就关闭，不继续微调。不得把无 profile 的结构搬迁包装成性能优化。 |
| E. Windows 发布签核 | 发布准备时独立执行 §5.1 的最新构建、安装生命周期、用户数据保留、真实 UI 流程和磁盘耗尽验收。该 Gate 可与不影响发布路径的单一结构切片并行准备。 | 把构建版本、schema、环境、实际结果与限制写入 ADR 0395；旧 profile 或历史验收不可代替当前安装包结果。未通过时保持 Gate Open，不据此发起无关架构拆分。 |

**当前执行位置：** 阶段 0–8 已完成；台阶 A 完成了对 Tools 契约/Admin/messaging 与 Agent memory worker 的复核。`SessionStore` lifecycle wrapper 和上述大模块的纯拆分均暂缓。ADR 0481 的旧 summary marker-only writer 已删除，workspace tests、check、严格 Clippy 与格式门禁通过；路线回到无 Active 状态。下一个结构切片仍由新证据触发，不按 crate 体量自动拆分。Common 拆分、Tools crate 拆分与通用 Job 抽象继续暂缓，直到相应门槛被新证据满足。

2026-10-05 对 `AppState`/`ApplicationRuntime`、UI shell/Composer 与 SessionStore 非事务 lifecycle façade 的只读复核均未发现 owner 稳定后的重复边界回归。SessionStore 生命周期 wrapper 大多是 typed `run_blocking` 转发，实际 actor/确认/delete policy 由 Agent `session/status.rs` 持有；搬移 wrapper 不会改变 owner 或调用链，故不新增 Active 项。重开条件见 §5.3 的启动编排、Composer、全局布局与 SessionStore 边界观察结论。

AppState/runtime 与 UI shell/Composer 的并行只读复核没有发现可准入的候选。2026-10-05 后续只读审计覆盖 SessionActor、facts/embedding 存储、Agent 事件投影、SessionStore 写侧与 ReAct facade：均未发现稳定 owner 后的重复回归、重复策略或可验证的子模块/新 crate 收益；模块拆分继续暂缓。ReAct 中两条 usage 字段映射目前没有漂移，写入路径语义不同，重开条件见上。审计发现的 fact query 注释漂移已修正；事件投影审计发现一条无调用方的 Compaction 直发入口，现已按 ADR 0482 删除并完成 workspace 门禁。ActionService 测试归属审计后，六项独立 process 流读取契约测试已按 ADR 0483 收回实现 owner；统一 ActionService 的后台/定时生命周期测试保留服务级归属。当前无 Active 结构切片；后续仍按 §5.5 证据队列审查，不按 crate 行数制造拆分工作。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
