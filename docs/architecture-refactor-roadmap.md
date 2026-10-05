# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；SessionUsage 累计上限契约、session-scoped KV 孤儿清理 owner、summary marker 单一原子生产路径、Tools 测试归属、Input→Tools 测试反向依赖、MCP/Skill 直调授权策略来源、MCP 管理操作网络策略来源、X12 例外消息写入口、ADR 编号索引完整性、actorless session action lifecycle 清理（ADR 0507）、Ask reducer state ownership 收口（ADR 0508）、终态 Ask 清理事件归属（ADR 0509）、ReAct phase 来源 session 身份（ADR 0510）、Windows 子进程先入 Job 再恢复（ADR 0513）、显式 end 失败重试契约（ADR 0514）、Skill venv 子进程 containment（ADR 0515）与 MCP prompt index 类型化（ADR 0516）已完成；当前无 Active 结构切片，下一轮从证据复核步骤 0 开始；Windows 发布验收为独立开放签核门
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
| **Next** | 当前没有经准入的 Next/Active。2026-10-05 步骤 0 复核了 SessionStore/Agent 持久化边界及 rollback 历史、ActionService、ProcessContainment spawn、venv 半成品判据和 App runtime config；未见稳定后的同一事务不变量重复回归、生产写入绕过持久化权威入口或新依赖漂移。另抽查近期高触碰的 `ui/src/routes/+page.svelte`、`agent/src/layer.rs`、`agent/src/react/mod.rs`、`app-binary/src/app_state.rs`、`tools/src/lib.rs`、`tools/src/security.rs`、`tools/src/builtin/mod.rs`、`agent/src/session/tool_runner.rs`、`agent/src/session/status.rs`、`agent/src/react/transcript.rs` 与 `agent/src/react/stream_step.rs`：变化对应 UI 功能演进、既有 owner 收口、ReAct 维护、组合根/模块门面、已修复的 MCP policy drift 与 session 生命周期垂直协作；未见稳定职责间重复返流。session grant 与 interaction resolve 的两次写窗口仍无状态冲突复现；venv 半成品问题仍无实际回归证据，按 §5.2、ADR 0515 的既定条件观察。新证据出现时重新分流；不以文件/crate 体量单独立项。 |
| **Deferred** | 已复核候选保留各自重开条件，见 §5.3；当前无其他结构候选。 |
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

### 5.3 内部模块与 crate 边界（按证据复核 / 当前无 Active）

本节只保留未解决项与已复核候选的重开条件；已完成切片的背景、决定和验证以对应 ADR 为准。私有模块整理与 crate 拆分都必须通过 §5.5 准入，不以文件或 crate 体量为目标。

#### 已复核候选与已知限制

| 状态 | 问题与当前证据 | 重开条件与边界 |
|---|---|---|
| **高风险 Candidate，暂缓** | `session_events.rs` 同时协调 event append、transcript projection、rollback、cache invalidation 与 commit 后 broadcast。按当前 HEAD `e81af24` 复算为 5,520 非空物理行（2,989 production / 2,531 tests），近 45 天 68 次提交触及。复核发现 `3bc807d`、`b0e47ba`、`a9b03a4` 是一次 SessionStore 边界收口中的不同缺口，`408564a` 将 confirmation CAS 与 event append 保持在同一事务 owner；未发现修复后同类事务不变量再次回归。`230a2e3` 已将只读历史测试移入 `session_history` 测试子模块。 | 仅在事务核心与无关 façade 反复耦合修改、相同原子性/rollback 缺陷修复后复发，或剩余测试无法按真实职责隔离且能证明收益时重开。event append、projection、rollback、cache invalidation 和 post-commit broadcast 继续由单一 SessionStore 协调，不暴露事务内部或引入第二恢复来源（[ADR 0466](adr/0466-session-history-read-facade-module.md)、[ADR 0479](adr/0479-session-history-test-ownership.md)）。 |

**显式 end 的基线观察（实施前）：** actorless 路径清理失败会返回错误且不推进 session 状态，但多条 action 可能已部分取消；idle/running Actor 路径先永久取消 actor lifetime，随后 best-effort 清理失败只记日志，仍写 `Completed`，Tauri 成功事件因此发出。run-exit 会移除 Actor，但不重试 action 清理；失败的 scheduled action 可能仍保持 Waiting 并保留 timer。claim 已获胜的 action 按 ADR 0424 保持运行，不属于取消错误。上述契约与实现已由 [ADR 0514](adr/0514-explicit-session-end-failure-contract.md) 完成并通过联合门禁；background 仍保留 ADR 0507 定义的有限 best-effort 重试。

契约、主要协调决定与验收结果见 ADR 0514；end 的数据库状态、Actor/run、Tauri event 和 UI selection 已联合验证，覆盖 actorless、resident/idle、running/stuck run、单项失败、多项部分失败、重试、claim-wins/cancel-wins、confirmation 与 direct-run admission。此处不再保留 Active 项。

#### 已复核但不进入 Next

- **Tools 与安全边界：** `tool_contract.rs` 继续作为共享执行契约 owner；`builtin/admin.rs` 由 AdminServices 承接副作用，Admin 保留 operation/request/output contract；messaging adapter 继续复用 `haven_messaging`。只有稳定后再次出现 policy/schema drift、同边界回归或独立消费者，才重新评估私有模块（[ADR 0213](adr/0213-operation-spec-single-policy-source.md)、[0391](adr/0391-admin-services-typed-output-projections.md)、[0396](adr/0396-messaging-domain-crate.md)、[0506](adr/0506-mcp-admin-connection-network-policy.md)）。
- **MCP prompt index 类型化（已完成，ADR 0516）：** 固定的 `name/tool_names` 摘要由 `McpServerIndexEntry` 沿 Tools→App adapter→Agent prompt port 传递；工具数从 names 派生，capability resolver 不再解析拼接描述。该类型不序列化到 IPC/provider/MCP wire；工具 schema 和结果保留 dynamic JSON。只有该投影新增稳定字段或出现新的独立消费者时再复核。
- **Admin 风险等级 parity 回归门禁（已完成，ADR 0512）：** 2026-10-05 复核发现 Admin 的两份风险等级声明仍一致，但 ADR 0506 已证明同一 model/native 边界发生过真实网络策略漂移，而现有测试没有覆盖完整风险等级 parity，故将该 Candidate 升为 Next 并补齐测试。当前有 20 个 model/native 共用操作；另有 native-only reconnect/refresh 两项，不纳入共享操作比较。新增用例从五个 model tool 的 schema 枚举预期集合，并逐项比较 model 实际风险、native metadata 与 `OperationContract` 显式风险；没有改动风险值。仅在 parity 回归或操作集合增加时重新评估是否把重复声明收敛为单一来源。
- **Windows 子进程 containment 启动顺序（已完成，ADR 0513）：** MCP stdio、Shell、Skill 与后台 Action 过去都在进程已运行后才加入 kill-on-close Job，MCP 还在 spawn 后才创建 Job；因此子进程可能在加入前派生不受 Job 管理的后代。`haven-platform::ProcessContainment` 现在负责命令挂起标志、Job 分配、唯一初始线程核对与恢复，失败时终止进程；adapter 继续拥有命令策略、管道与取消/等待生命周期。Windows 测试覆盖挂起时不执行、运行后派生后代并由 Job 回收，以及线程发现失败时 fail closed。若新增受管进程入口绕过此 API或出现进程树残留回归，再重开审查。
- **Agent 与 Memory：** SessionActor 继续独占可变 session state；ReAct stream/checkpoint/retry 保持协同；MemoryRuntime、worker、maintenance store 与 fact inference 按既有 owner 分工。只有交互恢复/队列计数、buffer 顺序、事实 marker 原子性或 prompt prefetch 等同一边界问题再次回归，才重开对应模块审查（[ADR 0214](adr/0214-react-run-inside-session-actor.md)、[0424](adr/0424-interaction-lifecycle-ownership.md)、[0468](adr/0468-memory-worker-maintenance-pass-module.md)、[0475](adr/0475-single-source-fact-sensitivity-rules.md)、[0476](adr/0476-react-turn-owns-search-context-projection.md)、[0481](adr/0481-remove-summary-marker-only-enqueue.md)）。
- **App 与 UI：** AppState/runtime、Composer/InputRouter 与 Ask/reducer/event owners 近期未发现稳定后重复边界回归；Ask 响应结算、终态 Ask 清理和 execution phase 来源身份已收口（[ADR 0508](adr/0508-ask-response-reducer-ownership.md)、[0509](adr/0509-terminal-ask-cleanup-event-owner.md)、[0510](adr/0510-session-scoped-react-execution-phase.md)）。2026-10-05 复核发现启动组装（`app_state.rs`）与运行时配置更新（`config_runtime.rs`）都构造 Router/媒体客户端，但失败语义有意不同：启动允许可选客户端降级，运行时更新则先完整准备、成功后才发布。当前不升为 Next；若新增配置或能力规则需要两处分别修改，或出现两条路径行为漂移，再评估一个私有构造 owner，同时保留两种失败策略。只有出现旧响应覆盖新状态、跨 session 状态泄漏或同一 lifecycle 回归时才重开；不提取仅按页面/operation 分类的模块。
- **Tauri 输出 DTO：** 2026-10-05 复核发现 history/search 已将 Memory `Session` 映射为 App-owned `SessionRecordDto`；`SkillInfo` 在 Skills crate 中明确定义为 bridge/UI snapshot；`Fact` 仍由 `list_facts`/`add_fact` 直接用 Memory repository 类型序列化，但 ADR 0357 将 Rust `Fact` 明确规定为 wire authority，前端通过命名 contracts 消费现有字段。当前没有字段意外暴露、DTO 漂移或独立 wire 消费者的回归证据，因此维持现有边界，不进入 Next。只有需要不同于存储实体的 renderer 字段/命名、发生未审阅的字段外泄/破坏性变化，或出现独立消费者时，才评估 App-owned Fact DTO 与显式 mapper（[ADR 0357](adr/0357-memory-command-contract-boundary.md)；完整输出分类见 [跨层输出契约清单](architecture-output-contract-inventory.md)）。MCP refresh 把内部 `McpReconcile` 收窄为 `McpRefreshPlan`，不会把连接配置送过 IPC；`list_mcp_tools` 的 `McpServerSnapshot` 保留 settings editor 需要的 command/args/cwd/url 并遮蔽 env 值。当前按已存在的编辑契约保留；字段范围或 renderer 隐私要求变化时重新审查（完整边界见输出契约清单）。
- **Common 与 ActionService：** 依赖图仍是 11 个内部 crate、29 条单向边且无环；Common 作为广泛复用的基础类型 crate 保持现状。`ConfigService` 是 ADR 0068 规定的有限有状态例外，只拥有配置快照、串行 typed patch、原子持久化及不含密钥的变更通知；运行时应用仍由 App 装配。开发规范已与该边界对齐。ActionService 仍与 Tools 的执行策略、`haven_memory::ActionStore`、Agent 授权/完成投影及 App 生命周期形成纵向调用链。只有出现独立消费者、真实依赖方向问题或同环境可复核的维护/构建收益时，才重开 crate 评估（[ADR 0068](adr/0068-versioned-config-service.md)、[ADR 0359](adr/0359-common-boundary-and-profiling-baseline-audit.md)、[0507](adr/0507-session-owned-action-cleanup.md)）。

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
| 1 | **契约与生命周期收口（按证据推进）** | 显式 end 失败与重试（ADR 0514）及 Skills venv 子进程 containment（ADR 0515）已完成；下一轮须先经过步骤 0 证据复核，再决定是否进入本步骤。 | actorless、驻留 Actor、运行中 Actor、Action claim 先后竞争和持久化失败都有一致且可测试的可见结果；direct run 从 Paused 入场先持久化 Running；rollback/continue 不得在 end closing marker 后改写 durable state；end 对 stuck run 仍响应，ADR 0424 first-wins 不变。 |
| 2 | **权威来源与跨层不变量** | 检查持久状态、事件、运行态、投影和 UI 是否仍各有单一 owner；只有真实漂移、明确风险对应的失败注入缺口、重复回归或绕过权威入口时才切片。2026-10-05 的 ReAct Fatal 双终态 producer 已收口：dispatcher 专用入口过滤 AgentEvent 重复错误，SessionSupervisor 的 SessionEvent 经共同 TauriEmitter 投影，直接 run API 保留原行为（[ADR 0511](adr/0511-session-terminal-error-single-owner.md)）。后续交付仍是窄范围回归/故障测试、冲突入口删除和不变量文档更新。 | 回归固定不变量且不增加第二真源；跨 crate/跨端变更通过相应完整门禁。 |
| 3 | **稳定 owner 的职责收口** | owner 稳定后，若同一边界重复回归、跨职责共同修改或测试放错位置持续增加维护成本，迁移一条完整垂直链。交付物优先是私有模块/API 收窄、测试归属调整和旧入口清理，不预先按大文件切片。 | 调用和测试落到真实职责 owner，重复规则或跨边界修改减少，外部契约、依赖方向及运行语义保持不变；收益不能说明则关闭候选。 |
| 4 | **模块成熟后再评估 crate/API 边界** | 只有模块 owner 已稳定，且存在独立消费者、真实依赖方向问题或可复核构建/迭代成本时才评估 crate 拆分。交付物包括依赖图、API/消费者映射；若声称构建收益，须有同环境基准。 | 提取后依赖单向、API 稳定、消费者不用反向依赖或重复 adapter，并证明维护/构建收益；任一不满足就保留现边界。 |
| 5 | **性能与容量** | 仅在同负载 profile 复现有用户意义的成本时优化；交付物为固定场景的前后指标，并遵守 SQLite 容量、失败恢复与资源上限契约。Windows 发布验收独立保留在 §5.1，不作为结构重构阶段的退出依赖。 | 优化结果超过噪声且达到目标，否则关闭候选；没有当前测量就不以“降复杂度”为名做性能改动。 |

以上是循环复核的先后顺序，不是一次性瀑布项目：每轮从步骤 0 重新分流，完成一个切片、同类问题复现或准备发布时再审查证据。ADR 0514、ADR 0515 与 ADR 0516 已完成；当前没有 Active 或已准入的 Next，待新证据出现时从步骤 0 重新分流。ReAct Fatal 双终态 owner 已在步骤 2 收口（ADR 0511）；Admin 风险等级 parity 回归门禁已完成（ADR 0512）。发布验收 Gate 与结构重构并行，按 §5.1 独立关闭。

**当前执行位置：** 架构阶段 0–8 已完成；ADR 0514–0516 均已完成。滚动执行周期已回到 §5.6 步骤 0：下一轮须提供新的源码、重复回归、依赖/API 变化或可复核成本证据，才建立唯一 Next 并启动切片。不按 crate 数、行数或日历制造工作。ReAct Fatal 双终态发布 owner 已由 ADR 0511 收口；Admin 20 个共用操作的风险等级 parity 回归门禁已由 ADR 0512 收口，两项 native-only 操作仍单独测试。crate 拆分仍无证据，近期边界为 11 个内部 crate、29 条单向边。Windows 发布验收仍是独立 Open Gate。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
