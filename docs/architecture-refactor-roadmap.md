# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；当前无 Active 结构切片；Windows 发布验收为独立开放签核门
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

### 5.3 内部模块边界整理（持续按证据复核 / 当前无 Active 切片）

这不是 crate 拆分目标；只在职责与稳定 owner 边界能证明维护收益时做私有模块整理。已完成切片的实现范围、验证与回滚记录以 ADR 为准：SessionStore 只读历史 façade（[0466](adr/0466-session-history-read-facade-module.md)）、LLM provider schema projection（[0467](adr/0467-llm-tool-schema-projection-module.md)）、Memory maintenance pass（[0468](adr/0468-memory-worker-maintenance-pass-module.md)）、managed-media 生命周期与 producer/GC/Files 登记协调（[0469](adr/0469-app-managed-media-lifecycle-module.md)、[0470](adr/0470-generated-media-write-gc-gate.md)、[0473](adr/0473-files-rich-path-generated-media-gc-gate.md)）、录音 ID 交接和 Shell overlay controller（[0471](adr/0471-recording-session-id-handoff.md)、[0472](adr/0472-recording-overlay-controller.md)）、架构依赖清单门禁（[0474](adr/0474-architecture-dependency-inventory-gate.md)）、Memory fact sensitivity 规则单源化（[0475](adr/0475-single-source-fact-sensitivity-rules.md)）、ReAct 搜索响应投影归入 turn owner（[0476](adr/0476-react-turn-owns-search-context-projection.md)）。

**本轮完成 — [ADR 0476](adr/0476-react-turn-owns-search-context-projection.md)：**不依赖 stream state、仅由 turn response 处理调用的 server-side search context 投影与 outcome 已移入 `turn.rs`，identity 回归随实现迁移。StreamForwarder、队列、checkpoint、重试和 mixed tool+search 的既有时序留在原 owner。当前没有已批准的下一项结构切片；下一候选须按 §5.5 的证据门槛重新审查。

当前边界决定：Common 拆分维持 [ADR 0359](adr/0359-common-boundary-and-profiling-baseline-audit.md) 的暂缓结论；Tools crate 拆分没有独立依赖边界或消费者收益；SessionStore 继续独占 event append、投影和 rollback 事务协调，`event_cursor` 与 `last_msg_at` 双时钟、提交后发布均不得分散；`LOCAL_TOOL_SECURITY_MATRIX` 仍是生产权限提示的 operation 白名单，保留在 `security.rs`。管理 surface、LLM router、授权沙箱和 inbox 崩溃恢复边界按现有 owner 保留，具体依据见相关 ADR。

已复核热点包括 `+page.svelte`、`SettingsView.svelte`、`admin.rs`、`llm/router.rs`、`inbox.rs`、Tools/Agent 根模块、`react/mod.rs`、`layer.rs`、`session/mod.rs`、`resume.rs`、`session/tool_runner.rs`、`react/stream_step.rs`、`commands/session.rs`、`app_state.rs`、`MemoryView.svelte`、`ToolsView.svelte`、`ToolResultCard.svelte`、`InputRouter.svelte` 与 `+layout.svelte`。`tool_runner.rs` 的近期 churn 属于 ADR 0424 同一轮 owner 收口，确认与 ActionService 分持待决请求和执行/取消 claim，当前保留原边界；`stream_step.rs` 的流生命周期队列、checkpoint 与 retry 需保持协同，搜索响应投影则由本轮 ADR 0476 收回 turn owner。`resume.rs`、ToolsView 和 InputRouter 保留各自会话恢复、管理页与统一 composer 边界。InputRouter 的异步附件读取曾允许发送越过读取完成点且并行读取可能超限，现已阻止读取期间提交并预留附件名额，回归由 UI 测试覆盖。ToolResultCard 同时展示 ask 与 tool output，但 ask selection、pending interaction、dismissal 分属 controller/reducer/page owners；ADR 0430 的 reopen 路径近期只经历一次纵向改动，AskInteractionCard 提取留作条件候选，待跨职责重复 churn 或回归再启动。MemoryView 单次 resume 参数遗漏已在 `487ff9e` 修复；`+layout` phase store 订阅清理也已修复。

**观察项复核（2026-10-05；本轮均未批准实现切片）：**

- **Tools 根模块 helper：** 文件约 402 行，近 45 天热点没有形成 helper 的重复跨职责修改；保留 crate 级共享规则与现有 feature owners。只有相关 churn 或回归复现后才考虑搬移。
- **ReAct media projection：** `types` / `react` 存在双向模块调用；Text fallback 会对同一输入确定性地重算一次 media plan。共同的 append helper 已避免 live/replay 分叉，目前没有性能 profile 或功能回归证据支持拆投影 owner或优化重算。若证据出现，优先评估将 event→canonical/round projector 与媒体 helper 一并收归 ReAct projection owner；单独优化时让一次投影同时返回 `ContentPart` 与表示元数据，并保留 snapshot-safe `MediaInput`、稳定 `asset_id`、路径脱敏及 durable MediaPlan 边界。
- **启动编排：** 9 月的 readiness 调整曾达到历史复核门槛；当前由 AppState 决定启动顺序、ApplicationRuntime 管任务生命周期、AgentLayer 管 dispatcher、bootstrap 触发并提供 Tauri emitter，边界已由架构文档和 ADR 对齐，之后未见同一路径重复回归。
- **`getTools()`：** `+layout` 与 ToolsView 双读服务不同生命周期，Rust manifest 源和前端 mapper/snapshot 各只有一个 owner；未发现旧响应覆盖新值或 UI 漂移。

以上候选只有在 §5.5 所列回归、重复 owner、调用边扩张或可复现成本出现后重开；纯行数减少不构成准入理由。

事件存储与 transcript projection 的写侧拆分仍是暂缓的高风险 Candidate。2026-10-05 复核时，`session_events.rs` 为 6,078 行（3,075 production / 3,003 tests），近 45 天有 66 次提交触及（约 +7,003/-925）；改动主要是 9/22–25 一轮 SessionStore owner 收口，以及 9/30–10/5 的 durable session facts 纵向演进，未发现边界稳定后事务不变量反复回归。体量和 churn 足以触发复核，但不证明搬进 child module 会降低维护成本。只读历史 façade 已由 [ADR 0466](adr/0466-session-history-read-facade-module.md) 拆出；后续只有观察到事务核心与无关 session façade 反复耦合修改、同一原子性/rollback bug 重复修复，或测试无法按真实职责隔离并能证明模块收益时，才重开实施评估。若准入，优先评估不参与 canonical event/projection transaction 的 lifecycle façade 和对应测试；event append、projection、rollback、cache invalidation 与提交后 broadcast 继续由同一 SessionStore owner 协调。若候选要求上层分别写 event/projection、暴露事务内部、引入第二恢复来源，或只有行数下降，立即停止。

`haven-tools/builtin/admin.rs` 的五个管理 surface 契约不按 operation 数量拆分；`llm/router.rs` 已有 request/stream executor；`security.rs` 拥有授权、receipt、禁用 operation 和路径沙箱；`inbox.rs` 的 registry、mailbox、archive 与崩溃恢复共用文件锁，暂不拆。热点约 800 行时先区分生产与测试代码，再记录保留理由或明确拆分边界；只搬行数不立项。内部整理保持外部 API、wire、schema 和运行语义不变，并独立提交。

### 5.4 Common 拆分与性能优化（Candidate）

只有依赖图、重复 owner 或可复现 profile 表明存在明确收益时，才另立拆分/优化任务。crate 拆分须证明独立稳定 API、单向依赖边界及实际消费者收益；不得只为减少文件行数、构建目录或 crate 大小而拆 crate。性能比较使用相同工作负载、数据规模和环境记录前后结果；没有明显改善则关闭候选，不继续微调。

### 5.5 长期候选准入与复核

长期结构治理按“有证据的问题队列”推进，不预设日期或全仓重写目标。候选只有在出现下列至少一项时才进入审查：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、某热点在多个变更中频繁发生跨职责修改、依赖图暴露反向/多余依赖，或同负载 profile 显示可复现的资源/延迟问题。文件超过约 800 行或多于两个独立职责只触发复核，不单独证明要拆。

每个候选的短 ADR/评估要记录：现有 owner 与不变量、生产代码和测试的职责分布、调用/依赖边界、预期收益及观察方式、破坏面和停止条件、适用门禁、回滚方式。实施顺序固定为：证据确认 → 接受目标与切片 → 迁一条垂直调用链 → 删除旧入口 → 跑影响面门禁 → 对比 owner/依赖/性能指标 → 独立提交并更新状态。若只移动代码、扩大公共 API、暴露事务内部、增加第二权威来源，或验证不能证明维护/运行收益，立即停止并把候选记为“不需拆分/暂缓”。

长期执行按触发信号驱动而不按日历制造工作，优先级为数据/安全/生命周期不变量故障、重复跨 owner 回归或修改耦合、依赖/API 边界问题、最后才是有同负载证据的性能优化；行数和 crate 大小不计为准入分。仓库较大时可并行委派只读审计（例如依赖图、热点职责、事务/安全不变量），但审计结论须由主执行者回到源码与门禁核验，任何时刻只实现一个 Active 切片。审查没有合格候选时，保留“无 Active 切片”状态并等待新证据，再继续同一套复核流程。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
