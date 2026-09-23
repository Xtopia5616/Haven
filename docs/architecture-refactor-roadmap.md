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

- `resume.rs` 仍直接创建 `ReActState` 并驱动 `run_react_loop`；`SessionActor` 只是保存部分运行态；
- actor mailbox 仍包含 usage、stream identity、token estimate、messaging poll 等内部命令；
- resume 仍需协调 RAM 队列、ingress cursor、undelivered scan、partial promotion 和 interaction replay；
- Agent/Tools/App 仍有较宽的 runtime facade 和 `Arc<Database>` 传播；
- ActionService 的内部领域输出、usage 写入参数和 LLM router 请求入口仍存在重复包装；
- 设置命令仍手动编排多个 runtime 的更新；
- UI 页面和 reducer 已有边界，但编排代码仍过重，IPC 类型仍是 Rust/TS 双份维护。

### 2.1 已完成的降复杂度切片（截至 2026-09-24）

以下切片已经独立提交并通过对应门禁；它们是阶段目标的增量落地，不代表后续阶段可以跳过契约收口：

- `72b033e`：一次 session run 的驱动迁入 `SessionActor`，并补 actor 等待期间的 panic/运行态保护。
- `672e746`、`9038326`：删除 actor 内重复的 ReAct run 状态包装，收平只剩单一消息运行态的容器。
- `fbbb60f`、`426769c`、`48c8d1c`：分别移除 stream identity、run budget、token estimate 的 mailbox 往返，并把 LLM 请求策略快照固定在一次请求内（ADR 0219–0222）。
- `7ec0965`：usage 累计与持久化由 `UsageRuntime` 所有，actor 不再承载 usage mailbox（ADR 0223）。
- `5875b80`、`5abb9c3`：Agent 对工具目录和 observation 改用窄 port，保留授权/执行边界（ADR 0224–0225）。
- `00f5df5`：删除 AgentLayer 的 context limits 镜像，配置读取委托给 ReActEngine（ADR 0226）。
- `ce26c21`、`a9b03a4`、`6f86362`：Action board 输出、continue 截断和 usage 写路径分别完成 typed/store ownership 收口（ADR 0215、0217–0218）。
- `950e0fb`：UI 可见消息改为从 session reducer 纯派生，页面不再维护第二份消息数组，并补 selector 单测。

当前阶段判断：阶段 1 的 mailbox/运行态收口已基本完成，阶段 2–3 仍需继续验证恢复与 storage port 的全局边界；阶段 4 已完成目录/观察读取的切片，执行/授权必须作为一个安全边界继续审查；阶段 5–9 尚未完成。

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
- 统一 `persist_llm_call(session_id, LlmCallUsageInput)`，把 call purpose/cache accounting 等字符串收为 typed enum；
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
- 先校验和持久化 snapshot，再按模型/媒体/工具/MCP/日志/hotkey 依赖应用；
- 支持整体 runtime snapshot 替换、失败时恢复旧 snapshot 或显式报告 `restart_required`；
- 将配置副作用、通知和日志归属集中；命令保持 `Result<T, String>`；
- 增加应用顺序、半失败、回滚和敏感字段不泄漏测试。

主要文件：`crates/common/src/config/service.rs`、`crates/app-binary/src/commands/settings.rs`、`runtime.rs`、`bootstrap.rs`、MCP/tools wiring。

验收：settings command 不再逐个 bind router/tools/agent/MCP；热更新不会观察到混代 runtime；失败路径可诊断。

### 阶段 6：LLM Router 请求对象化（P2）

目的：减少 router 公共包装入口，不改变 provider adapter。

工作项：

- 将 `RequestKind` 中混合的 capability、call purpose、UI usage role 分成 `Capability`、`CallPurpose`、`RequestPolicy`；
- 将 router 对外收敛为 `complete`、`stream`、`embed`、`health` 四类 request object；
- 内部拆成 `ModelDirectory`、`CallExecutor`、`StreamExecutor`，保留已有 retry/timeout/health/semaphore/usage 管线；
- 删除只转发参数的 chat/chat_request/output_cap/stream wrapper；
- provider adapter 只做 wire mapping，保持 golden fixture。

主要文件：`crates/llm/src/router.rs`、`types.rs`、`request_pipeline.rs`、`streaming.rs`、Agent/Tools 调用点。

验收：四类能力各有 request object 和负向 capability 测试；provider wire 与 usage/stream 契约不变；无重复 retry/usage 入口。

### 阶段 7：统一 Job 生命周期与 MemoryRuntime（P2）

目的：减少后台/定时任务重复状态，并把记忆后台编排移出 Agent ReAct。

工作项：

- 在现有 `actions` 表和状态模型上把 trigger 与 execution 分开，后台任务是 `Immediate` trigger，定时任务是 `At/After` trigger；
- 统一 claim、cancel、timeout、retry、tail output、completion outbox 和 UI projection；
- messaging 不并入 Job，仍是独立 transport domain；
- `MemoryRuntime` 监听 committed session event，负责 fact extraction/maintenance/index catch-up；
- Agent 只提交 `SessionCommitted` 并通过 MemoryReader 获取 recall；LLM 调用通过小型 `InferencePort` 注入。

主要文件：`crates/tools/src/action_service.rs`、`action_lifecycle.rs`、`crates/agent/src/memory_worker.rs`、`memory_service.rs`、`memory_index.rs`、`crates/memory/src/`。

验收：两类 Job 共用一套生命周期和 UI 投影；记忆失败不改变 ReAct turn 结果；重启、重复 outbox、取消和限额有测试。

### 阶段 8：IPC 单源生成与 UI 编排收口（P2）

目的：减少 Rust wire DTO、TS contract、mapper、文档之间的重复维护，同时保留一个权威 session store。

工作项：

- Rust wire DTO 作为生成 TypeScript contract/字段映射的唯一来源；保留前端运行时校验；
- 命令数量不为减少复杂度而强行合并；事件按 `session/agent/action/recording/app` envelope 收敛；
- 将 `+page.svelte` 的事件注册、提交、resume、rollback、model 操作抽到 typed `ChatController`；
- 将 `sessionReducer.ts` 按 lifecycle/transcript/interaction/usage/stream 拆成内部 reducer module，对外仍是单一 store；
- reducer 以 session/selector 订阅，避免每个 stream batch 广播完整状态树；
- 删除手写镜像中的旧 shape、mapper 和兼容测试。

主要文件：`crates/app-binary/src/events.rs`、`ui/src/lib/contracts/`、`ui/src/lib/sessionReducer.ts`、`ui/src/routes/+page.svelte`、`+layout.svelte`、scripts。

验收：Rust/TS 生成检查在 CI 通过；单一事件登记点；UI session/stream/resume/rollback/optimistic 行为测试通过。

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
