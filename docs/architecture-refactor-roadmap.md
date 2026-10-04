# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；交互 owner 路由正在实施；Windows 发布验收为独立开放签核门；Common/性能工作按证据触发
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

### 5.2 交互生命周期所有权（Active / Accepted）

ADR 0424 已于 2026-10-04 采纳，确定 session、scheduled 和 UI 直调确认的运行时 owner、终态与期限契约。当前已建立非持久 `InteractionEnvelope`、显式 owner event/IPC 投影、可选真实 session 上下文以及 Session durable append/replay 校验；resolve IPC 已改用 `request_id` 并返回 typed outcome，AppCommand 直接由 `ui_confirmations` 仲裁。ScheduledAction 已改为按 `action_id` 定位 owner-local registry，并同时校验 `request_id`；批准/副作用启动前通过 ActionService 与 `scheduled_execution_claim.<action_id>` 的持久 CAS 认领执行权。认领与取消在共享数据库上 first-wins：取消先提交则批准不能授权或执行；执行认领先提交则取消返回 false，运行中的操作正常收尾。终态事务清理认领；重启时 running action 仍失败且不 replay，并清除残余 claim。Session capability/resolve 现按 `session_id` 直接定位 actor，再在该 actor 内匹配 `request_id`，不扫描其他 actor。durable resolve append 成功后才推进 actor，失败时 pending 保持可重试；session grant 持久化先于唤醒，但 grant 与 resolve event 仍是两次独立 durable write。下一项统一 owner deadline、删除 `timed_out` 和 renderer 本地 deadline fallback。前置兼容清理、SessionStore history façade 提取及其独立门禁已完成。此项是独立结构目标，不回写为已完成的阶段 7 工作。

运行时使用 typed owner envelope：producer 显式把 owner 附到非持久 supervisor/app event，再投影到 Tauri event/resume/resolve DTO；mapper 不从 kind 或 `session_id` 推断 owner。`InteractionRequest.session_id` 可选，但 SessionActor durable append/replay 必须拒绝缺失或不匹配的值；session-owned `Some(session_id)` 保持现有 JSON 字符串形状，持久 event 不增加 owner 字段。Session owner 使用 `session_id` + request ID；scheduled owner 用 `action_id` + request ID；AppCommand owner 用 request ID。真实 `session_id` 只表达 producer 提供的上下文，不改变 owner，也不持久化通用 owner；不增加 schema/reset。resolve IPC 将实际承载 `conf-*` request ID 的 `step_id` 改名为 `request_id`。已仲裁的 `Resolved`、`Expired`、`Stale` 使用明确的类型结果；可重试失败通过命令错误返回并保留 pending。

先迁移跨层 contract，再一次只迁一条 owner 路径：

1. **守住 durable session 契约。** 将运行时 `session_id` 改为可选上下文；append 与 replay 检查 Session owner 的内外 session ID 一致。`None` 不得进入 session event；session JSON 形状不变。
2. **显式传 owner 并更新 IPC。** Session、ScheduledAction 和 AppCommand producer 都在 runtime event 附 owner；Tauri event 与 resume projection 显式映射；同步 generated command types、事件 mapper、reducer 与 IPC 文档。未知 owner、缺失 route key 和非法 owner/context 组合 fail closed。
3. **直迁 AppCommand。** `AppState.ui_confirmations` 直接按 owner + request ID 接收 resolve/expire。先验证 receipt、target、scope 和当前策略；决定可重试失败保留 pending；被接受后先终结弹窗，再启动 app-scoped continuation。
4. **直迁 ScheduledAction（路由与执行权仲裁已完成）。** 按 `action_id` + request ID 路由；有关联 session context 时 owner 仍为 ScheduledAction。批准或已获准 operation 开始副作用前，ActionService 持有 owner execution claim；SQLite `kv_store` claim 与 action status CAS 在同一 writer 序列中仲裁取消和执行。取消先赢则不授权/不执行，执行 claim 先赢则拒绝后续取消，让 operation 收尾；终态提交清理 claim 并保留 ADR 0392 的 action/outbox 顺序和 running action 不自动 replay。cancelled action 的 pending confirmation 由 action owner 事件清除。此实现不增加 schema 或修改 durable event。
5. **直迁 Session（路由已完成）。** 按 session ID 定位 actor，不跨 actor 扫描；能力查询、普通 resolve、过期和 grant-aware resolve 均使用同一明确 owner。durable resolve append 成功后才推进 actor；append 失败仍可重试；gated tool batch 全部解决、session grant 持久化后才唤醒。grant 与 resolve event 暂非原子事务，保留当前可重试语义，不宣称二者原子提交。
6. **统一 expiry 并删旧路径（Next）。** 每个 pending permission confirm 登记时必须带有效绝对 deadline，由 owner timer 执行幂等 expire；UI 只展示最终 `expires_at`，删除本地期限回退和 resolve IPC 的 `timed_out`。三路完成后删除 executor-first fallback、跨 registry/actor 查询、`"ui"`/`"action"` sentinel 和字符串化 stale 分支。每个 owner 的生命周期日志/通知只从其已接受的状态转移派生。

**退出条件：** request ID 不跨 owner 扫描/fallback；错误 owner/context 不能消费请求或触发副作用；点击与到期竞争最多接受一个终态；所有 pending confirmation 有 owner 管理的有效绝对期限；renderer 关闭、迟到点击、可重试持久化失败和 continuation 执行失败均有明确结果；Session append 失败不改变 actor；session 重启只从 `session_events` 恢复，UI/scheduled 请求不自动重放。运行时 DTO 与 UI/IPC 类型保持一致；旧 route、期限入口和测试分支删除。此方案不改 durable session payload 或 schema，无需重置；验证与切片细节见 ADR 0424/0423。

### 5.3 内部模块边界整理（Candidate / 当前只完成一个低风险案例）

这不是 crate 拆分目标，按职责和稳定 owner 选择可证明有益的内部边界。首个边界已完成：`SessionStore` 的只读历史查询与 DTO 已收入私有 `session_history` 模块，公开 façade、SQL owner、查询过滤/排序/缓存和序列化保持不变。聚合 event stream 与多个投影的 `session_resume_projection` 继续留在事务协调 owner。实现约束和回滚见 [ADR 0466](adr/0466-session-history-read-facade-module.md)。

ADR 0424 收口后，按证据逐个评估以下候选；启动任一项前仍需写清 owner、边界和验收，并独立提交：

1. **LLM provider schema projection。** `llm/types.rs` 中约有 600 行 OpenAI、OpenAI Responses 与 Gemini 的 JSON Schema 方言投影，可移到 adapter 专属 helper/子模块；通用工具参数净化仍留在共享类型边界。验收要求 provider tool-schema wire 输出不变，内部 schema 执行校验保持不变。
2. **MemoryWorker 定期 maintenance。** 将 `run_memory_maintenance*`、predicate merge 和 contradiction arbitration 作为私有模块评估；普通 fact/summary 提取与 durable outbox 留在现有 owner。验收覆盖步骤顺序、失败聚合、逐步取消、LLM 未配置时跳过、返回计数，以及 outbox marker/ack 恢复语义不变。
3. **App managed-media 文件生命周期。** 评估从 `commands/recording.rs` 拆出上传落盘和媒体清理的私有模块，Tauri 命令和 IPC 保持不变。上传、两个 media root 的清理、同一 `UPLOAD_WRITE_LOCK`、SessionStore durable refs 与 ManagedAssetRegistry lease 必须作为一个完整边界移动；验收覆盖额度、部分失败、路径/重解析点拒绝、读取 refs 失败时 fail closed、活动 lease 保护和并发清理。
4. **Shell 录音 overlay 状态机。** 可评估从 `+layout.svelte` 提取录音 overlay timer/state/cancel 逻辑；shell 仍独占全局事件登记和系统通知。验收覆盖 start/stop/transcription 关联、快速 stop、cancel、voice transcript 提交，以及卸载时 timer/listener 清理。不要把全局事件订阅再搬进新 controller。

这些是候选排序，不是必须全部拆分的承诺；若实际代码已收敛、owner 更清楚或职责无法独立验收，就标记为不需要拆分。`+page.svelte` 已有多个 chat/session/model/ask/view controller；`SettingsView.svelte` 必须持有完整 Settings snapshot、dirty baseline 与 leave guard；`admin.rs` 五个 surface 共用一份能力/schema/request 桥接契约。这些文件不因行数单独拆分。

事件存储与 transcript projection 的内部拆分属于暂缓的高风险候选：只读历史 façade 已拆，但在写侧仍有可量化维护收益之前不启动。`SessionStore` 必须继续作为 append、物化投影和 rollback 的事务协调 owner：事件与 projection 原子提交，rollback 同时维护 `event_cursor` 和 `last_msg_at`，提交成功后才发布事件。若拆分要求上层分别写 event/projection、暴露事务细节或引入第二个恢复来源，应停止。

`haven-tools/builtin/admin.rs` 的五个管理 surface 契约有意集中，不按 operation 数量机械拆开。`llm/router.rs` 已有 request/stream executor；`security.rs` 是授权、receipt、禁用 operation 和路径沙箱 owner；`inbox.rs` 的 registry、mailbox、archive 与崩溃恢复共用文件锁，暂不拆。任一热点达到约 800 行时先区分生产职责与同文件测试，再说明保留理由或拆分边界；只搬行数、没有更清晰 owner 的工作不进入计划。每个内部整理保持外部 API、wire、schema 与运行语义不变，并独立提交。

### 5.4 Common 拆分与性能优化（Candidate）

只有依赖图、重复 owner 或可复现 profile 表明存在明确收益时，才另立拆分/优化任务。crate 拆分须证明独立稳定 API、单向依赖边界及实际消费者收益；不得只为减少文件行数、构建目录或 crate 大小而拆 crate。性能比较使用相同工作负载、数据规模和环境记录前后结果；没有明显改善则关闭候选，不继续微调。

### 5.5 长期候选准入与复核

长期结构治理按“有证据的问题队列”推进，不预设日期或全仓重写目标。候选只有在出现下列至少一项时才进入审查：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、某热点在多个变更中频繁发生跨职责修改、依赖图暴露反向/多余依赖，或同负载 profile 显示可复现的资源/延迟问题。文件超过约 800 行或多于两个独立职责只触发复核，不单独证明要拆。

每个候选的短 ADR/评估要记录：现有 owner 与不变量、生产代码和测试的职责分布、调用/依赖边界、预期收益及观察方式、破坏面和停止条件、适用门禁、回滚方式。实施顺序固定为：证据确认 → 接受目标与切片 → 迁一条垂直调用链 → 删除旧入口 → 跑影响面门禁 → 对比 owner/依赖/性能指标 → 独立提交并更新状态。若只移动代码、扩大公共 API、暴露事务内部、增加第二权威来源，或验证不能证明维护/运行收益，立即停止并把候选记为“不需拆分/暂缓”。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
