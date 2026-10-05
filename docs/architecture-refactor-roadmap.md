# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；SessionUsage 累计上限契约、session-scoped KV 孤儿清理 owner、summary marker 单一原子生产路径、Tools 测试归属、Input→Tools 测试反向依赖、MCP/Skill 直调授权策略来源、MCP 管理操作网络策略来源、X12 例外消息写入口、ADR 编号索引完整性、actorless session action lifecycle 清理（ADR 0507）、Ask reducer state ownership 收口（ADR 0508）、终态 Ask 清理事件归属（ADR 0509）与 ReAct phase 来源 session 身份（ADR 0510）已完成；当前无 Active 结构切片；Windows 发布验收为独立开放签核门
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

| 状态 | 当前项 |
|---|---|
| **Next** | 暂无可安全直接实施的结构切片；先按 §5.5 复核新证据。显式 end 取消失败问题仍需确定可见契约，见 Deferred。 |
| **Active** | 无。 |
| **Deferred** | 显式 end 的 action 持久取消失败语义；当前不统一改为 checked 或 best-effort，等待完整失败契约与协调方案。 |
| **Gate** | Windows 发布验收 Open，见 §5.1。 |

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

**本轮完成 — [ADR 0506](adr/0506-mcp-admin-connection-network-policy.md)：**`mcp_add/update/toggle/reload` 会按参数或启用状态新建/重建连接，但模型 operation view 曾落入 `NetworkAccess::None`，绕过 `Restricted` 下的 opaque-network 授权拒绝；原生请求则是 `Opaque`。两侧现从同一 `OperationContract` 读取分类；list/disconnect/remove 明确为 `None`。没有拆 Admin surfaces 或新增动态授权层。

**本轮完成 — [ADR 0509](adr/0509-terminal-ask-cleanup-event-owner.md)：**首个认领终态清理的事件通道同时清除活跃会话 Ask，覆盖独立抵达的 `session:updated` completed/error；配对主副事件保持 first-wins，不重复清理。Svelte 检查 0 error/0 warning，Vitest 122 files/981 tests 通过；不改 backend、IPC 或持久化契约。

**本轮完成 — [ADR 0510](adr/0510-session-scoped-react-execution-phase.md)：**全局最近 phase 补充其 source session 身份；Composer 和 submit steering 只读取 active session 对应 phase。先以红测复现后台 phase 会把空 transcript 的首条输入标为 steering，再修复；Svelte 检查 0 error/0 warning，Vitest 122 files/983 tests 通过。不改 wire 或持久化契约。

**Deferred — 显式 end 的 action 取消失败语义（2026-10-05）：**`end_session_inner` 的 actorless 路径使用 checked cleanup，已驻留 Actor 的路径使用 best-effort cleanup；ActionService 的故障注入测试确认单条 scheduled action 持久取消可以失败。ADR 0507 明确规定删除 fail-closed，但没有定义显式 end 的失败契约。改成统一 checked 会存在已部分取消 action、end 返回错误而 session 仍运行的情形；维持 best-effort 又允许 session Completed 时有 action 仍 Waiting。此问题有生命周期证据但需要先决定显式 end 的用户可见语义，因此暂不进入 Active；只有产品/命令契约确定 end 失败时 session 和所属 action 应保持何种状态后，再补 Actor/actorless 故障注入回归并评估事务/补偿边界。

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

已复核热点包括 `+page.svelte`、`SettingsView.svelte`、`admin.rs`、`llm/router.rs`、`inbox.rs`、Tools/Agent 根模块、`react/mod.rs`、`layer.rs`、`session/mod.rs`、`resume.rs`、`session/tool_runner.rs`、`react/stream_step.rs`、`commands/session.rs`、`app_state.rs`、`MemoryView.svelte`、`ToolsView.svelte`、`ToolResultCard.svelte`、`InputRouter.svelte` 与 `+layout.svelte`。`tool_runner.rs` 的近期 churn 属于 ADR 0424 同一轮 owner 收口，确认与 ActionService 分持待决请求和执行/取消 claim，当前保留原边界；`stream_step.rs` 的流生命周期队列、checkpoint 与 retry 需保持协同，搜索响应投影则由本轮 ADR 0476 收回 turn owner。`resume.rs`、ToolsView 和 InputRouter 保留各自会话恢复、管理页与统一 composer 边界。InputRouter 的异步附件读取曾允许发送越过读取完成点且并行读取可能超限，现已阻止读取期间提交并预留附件名额，回归由 UI 测试覆盖。ToolResultCard 同时展示 ask 与 tool output，但 ask selection、pending interaction、dismissal 分属 controller/reducer/page owners；近期连续修改的 pending reopen、选项投影、transcript 结算与历史恢复已由 ADR 0508 收拢响应和结算状态到 reducer，ADR 0509 补齐终态副事件独立到达时的 Ask 清理。`resolvedAskResponses` shadow 已删除，`resolvedAskIds` 继续只承担当前批次防重复提交；现无 Ask Candidate，不提取 Ask 卡片或 crate。MemoryView 单次 resume 参数遗漏已在 `487ff9e` 修复；`+layout` phase store 订阅清理也已修复。

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

### 5.6 长期滚动执行周期（跨迭代周期）

本路线按问题证据推进，不按日历、crate 数或文件行数承诺完工。已完成切片的设计、替代方案、验证与回滚分别记录在 ADR；本节只保留未来推进顺序和当前落点，避免把完成日志变成第二份历史索引。

| 步骤 | 长期工作流 | 进入条件与交付物 | 退出条件 |
|---|---|---|---|
| 0 | **证据复核与分流（每轮入口）** | 在切片完成、同类回归出现、依赖/API 变化或准备发布时复核。形成一项候选说明：问题、不变量、owner、源码/历史证据、影响面、停止条件和适用门禁；依 §5.5 选择 Next、Deferred、关闭或无候选。 | 只有一个 Next 或明确无候选；不把体量、单次审计或旧历史快照直接转成 Active。 |
| 1 | **契约与生命周期收口（当前关注）** | 先解决会话终态、所属 action、Actor 在场与否等状态不一致所暴露的契约缺口。当前候选是 §5.3 的显式 end 取消失败语义；先定义命令/UI/event/action 的失败结果，再决定是否需要状态协调、重试或补偿。交付物是故障矩阵、明确契约、回归测试和 ADR。不得只把 Actor 分支改成 checked，也不得将当前 best-effort 行为默认为产品契约。 | Actorless、驻留 Actor、运行中 Actor、Action claim 先后竞争和持久化失败都有一致且可测试的可见结果；若无法在不破坏响应性及 first-wins 的前提下给出安全方案，则维持 Deferred，不做局部补丁并继续审查其他候选。 |
| 2 | **权威来源与跨层不变量** | 检查持久状态、事件、运行态、投影和 UI 是否仍各有单一 owner；只有真实漂移、明确风险对应的失败注入缺口、重复回归或绕过权威入口时才切片。交付物为窄范围回归/故障测试、冲突入口删除和不变量文档更新。 | 回归固定不变量且不增加第二真源；跨 crate/跨端变更通过相应完整门禁。 |
| 3 | **稳定 owner 的职责收口** | owner 稳定后，若同一边界重复回归、跨职责共同修改或测试放错位置持续增加维护成本，迁移一条完整垂直链。交付物优先是私有模块/API 收窄、测试归属调整和旧入口清理，不预先按大文件切片。 | 调用和测试落到真实职责 owner，重复规则或跨边界修改减少，外部契约、依赖方向及运行语义保持不变；收益不能说明则关闭候选。 |
| 4 | **模块成熟后再评估 crate/API 边界** | 只有模块 owner 已稳定，且存在独立消费者、真实依赖方向问题或可复核构建/迭代成本时才评估 crate 拆分。交付物包括依赖图、API/消费者映射；若声称构建收益，须有同环境基准。 | 提取后依赖单向、API 稳定、消费者不用反向依赖或重复 adapter，并证明维护/构建收益；任一不满足就保留现边界。 |
| 5 | **性能与容量** | 仅在同负载 profile 复现有用户意义的成本时优化；交付物为固定场景的前后指标，并遵守 SQLite 容量、失败恢复与资源上限契约。Windows 发布验收独立保留在 §5.1，不作为结构重构阶段的退出依赖。 | 优化结果超过噪声且达到目标，否则关闭候选；没有当前测量就不以“降复杂度”为名做性能改动。 |

以上是循环复核的先后顺序，不是一次性瀑布项目：每轮从步骤 0 重新分流，完成一个切片、同类问题复现或准备发布时再审查证据。当前步骤 0 复核后，步骤 1 只有一个 Deferred 候选、没有 Active 实现切片；近期审计未在步骤 2–5 找到满足准入条件的新候选。未解决的契约问题不阻止继续寻找独立且证据充分的工作，但任何时候只实现一个 Active slice。发布验收 Gate 与结构重构并行，按 §5.1 独立关闭。

**当前执行位置：** 架构阶段 0–8 已完成；滚动执行周期位于 §5.6 步骤 0“证据复核与分流”。当前没有 Active 实现切片；显式 end 的 action 取消失败语义保持 Deferred，等命令/UI/action 的可见失败契约明确后再决定实现范围，不能用单分支改为 checked 代替设计。近期审计未找到新的合格 crate 或内部拆分候选；crate 边界仍为 11 个内部 crate、29 条单向边。Windows 发布验收仍是独立 Open Gate。后续每个切片结束、同类回归出现或准备发布时按 §5.5 重审，不按行数、crate 数或日历生成工作。

2026-10-05 对 `AppState`/`ApplicationRuntime`、UI shell/Composer 与 SessionStore 非事务 lifecycle façade 的只读复核均未发现 owner 稳定后的重复边界回归。SessionStore 生命周期 wrapper 大多是 typed `run_blocking` 转发，实际 actor/确认/delete policy 由 Agent `session/status.rs` 持有；搬移 wrapper 不会改变 owner 或调用链，故不新增 Active 项。重开条件见 §5.3 的启动编排、Composer、全局布局与 SessionStore 边界观察结论。

AppState/runtime 与 UI shell/Composer 的并行只读复核没有发现可准入的候选。2026-10-05 后续只读审计覆盖 SessionActor、facts/embedding 存储、Agent 事件投影、SessionStore 写侧与 ReAct facade：未发现稳定 owner 后的事务原子性重复回归或可验证的新 crate 收益。ReAct 中两条 usage 字段映射目前没有漂移，写入路径语义不同，重开条件见上。事件投影审计发现一条无调用方的 Compaction 直发入口，已按 ADR 0482 删除并完成 workspace 门禁。ActionService 的六项 process 流读取测试按 ADR 0483 收回实现 owner；ActionOutputTail 容量测试归入 `action_output.rs`，重复快照覆盖已删除（ADR 0484），生命周期和终态投影测试继续由 ActionService 持有。crate 边界复核没有找到可抽取的新 crate，但发现 Input 通过单项兼容测试反向 dev-depend Tools；此边已由 ADR 0485 移除，Cargo 生产图仍为 11 crate、29 条无环依赖。App 命令与 Tools adapter 的 MCP/Skill 直调策略现共用 `OperationPolicy::external`（ADR 0486）。随后策略审计发现模型可见的 MCP Connect 因 `NetworkAccess::None` 漏掉授权层的前置策略拦截，已由 ADR 0495 统一模型与 native 来源并补授权回归；复核又找到 add/update/toggle/reload 同类漂移，已由 ADR 0506 收敛并保留 manager 的 Deny 防线。X12 三类例外写路径现由受限 `SessionStore` 端口表达（ADR 0496）；编号冲突及 22 项索引遗漏现由 ADR 0505 修正并持续受 CI 校验。仍不按 crate 行数制造拆分工作。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
