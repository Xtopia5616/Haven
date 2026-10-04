# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；阶段 9 的 Windows 发布验收仍开放
> 更新日期：2026-10-04
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

**已完成。** Rust handler/Serde DTO 生成 TypeScript command contracts；事件 mapper、页面编排与 session reducer 各有明确 owner。见 ADR 0394。

### 阶段 9：Common 收缩、性能剖析和发布验收（最后）

**进行中，发布验收未完成。** 已有性能 profile、文件型 SQLite 容量观测及 `SQLITE_FULL` 注入测试；这些结果不能代替桌面发布验收。见 ADR 0359–0361、0395、0404。

## 5. 未完成事项

### 5.1 Windows 发布验收

按 ADR 0395 的验收范围，在隔离的 Windows profile 或 VM 上使用当前构建完成并记录：

- 首启、Settings、会话、工具确认、媒体、后台/定时任务、恢复与回滚的真实 UI 流程；
- 当前安装包的安装、升级、卸载，以及卸载后的用户数据保留行为；
- 物理磁盘空间耗尽时的错误分类、用户提示和恢复操作。

用最新代码和安装产物复跑适用的 Rust/UI 门禁，并在 ADR 中记录实际构建版本、schema、环境、结果与限制。ADR 0395 中早期 profile 和门禁结果是当时的历史证据，不能当作当前工作树或当前安装包的通过结论。

可以提前准备隔离 profile、安装器和物理盘耗尽环境；最终验收必须使用交互生命周期等 IPC/UI 变更完成后的最新构建。此项是发布签核门，不阻塞不影响发布路径的独立模块整理。

### 5.2 交互生命周期所有权

ADR 0424 当前为 **Proposed**，没有改变运行时 owner。它记录了 session、scheduled 和 UI 直调确认在 request 路由、过期处理、动作 continuation 上的剩余漂移。实施前先采纳具体切片与验收范围；不要把这项提案记为已完成的阶段 7 工作。

执行 owner 改造前，先让工作树中已开始的兼容清理按自己的目标完成适用门禁并独立提交；不要把删除旧契约与改变交互 owner 混成一个切片。当前 proposal 将 owner 定为运行时 typed envelope：Tauri event/resolve DTO 只携带路由标识，`InteractionRequest` 与 `session_events` 保持现有 durable 形状。Session owner 由 event 所属 session 确定；scheduled owner 用 `action_id`；UI owner 用 request ID。`session_id` 只表达真实上下文，绝不用于推断 owner；不持久化 owner，不增加 schema/reset。

按以下顺序推进，逐条迁移 owner，不做三路同时改写：

1. **先定路由和恢复契约。** 依 ADR 0424 proposal，以运行时 typed owner envelope 路由；明确 owner 操作结果（resolved、expired、stale、可重试失败）、Ask 与 permission confirm 的边界，以及 UI/scheduled 进程内请求在重启后的失效语义。owner 只决定路由，授权仍由后端 owner 校验 receipt、target 和 scope。保持 `InteractionRequest`/`session_events` durable 形状；未来若决定持久化 owner 或 continuation，另立决策并按届时 schema/reset 策略处理，不能靠内容或 `session_id` 占位值推断 owner。
2. **先迁 UI 直调确认。** 以 `AppState.ui_confirmations` 为唯一目标 owner，resolve/expire 直接调用它；校验或持久化失败时请求仍可重试，接受决定后先完成 pending 终态，再由 app-scoped task 执行动作。用短临界区保护同一请求的一次性消费，不让慢配置写入阻塞其他 owner 的确认。
3. **迁 scheduled confirmation。** 按 `action_id` 和请求 ID 定位 action owner；确认等待不伪装成 session actor 状态。点击和到期共用该 owner 的终态仲裁，并保持既有 ActionService completion/outbox 顺序。
4. **最后迁 ReAct confirmation。** resolve 明确定位 session/请求并交给对应 actor。保留先持久化 interaction event、后更新 actor 内存状态的顺序；同批确认未全部结束前不得唤醒 gated tool batch；session grant 必须在唤醒前提交。
5. **统一期限入口并删旧分发。** 每个 owner 使用同一绝对 deadline 和幂等 expire 结果；timer 只触发 owner 的 expire 操作，renderer 不决定后端状态。所有路径迁完后删除 actor/scheduled 扫描、executor-first fallback 和重复终态入口。生命周期日志与通知只从 owner 已接受的状态转移派生。

**退出条件：** request ID 不再跨 registry 扫描或 fallback；错误 owner 不能消费请求或触发副作用；并发点击与到期竞态最多接受一个终态；renderer 关闭、迟到点击、可重试持久化失败和 continuation 执行失败均有明确结果；session event append 失败不改变 actor 状态；session 重启只由 `session_events` 恢复，UI/scheduled 请求不自动重放。IPC、事件 mapper、通知安全字段、测试和必要的 schema/reset 文档与实现同批验收。细节以更新后的 ADR 0424 为准。

### 5.3 内部模块边界整理

这不是 crate 拆分目标，按职责和稳定 owner 选择可证明有益的内部边界。第一候选是 `SessionStore` 的只读历史 façade：把 `list_history`、`count_history`、`search_history*`、`conversation_window`、`session_resume_media`、`title_generation_context` 及只读 DTO 收入私有 `session_history` 模块；公开 `SessionStore` façade、SQL owner、查询过滤/排序/缓存和序列化均保持不变。不要把聚合 event stream 与多个投影的 `session_resume_projection` 一起移动。

事件存储与 transcript projection 的内部拆分属于高风险候选，仅在只读切片证明模块边界有实际维护收益后再评估。`SessionStore` 必须继续作为 append、物化投影和 rollback 的事务协调 owner：事件与 projection 原子提交，rollback 同时维护 `event_cursor` 和 `last_msg_at`，提交成功后才发布事件。若拆分要求上层分别写 event/projection、暴露事务细节或引入第二个恢复来源，应停止。

`haven-tools/builtin/admin.rs` 的五个管理 surface 契约有意集中，不按 operation 数量机械拆开。任何其他超过文件预算的热点先说明职责、接口和行为边界；只移动代码、没有清晰 owner 的拆分不进入计划。每个内部整理保持外部 API、wire、schema 与运行语义不变，并独立提交。

### 5.4 Common 拆分与性能优化

只有依赖图或可复现 profile 表明存在明确收益时，才另立拆分/优化任务；不要为减少文件行数或移动代码而拆 crate，也不预设性能阈值。

## 6. 更新规则

- 阶段状态只有在实现及适用验收完成后才更新；受限或未执行的验证要明确写出。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
