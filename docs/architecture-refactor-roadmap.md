# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 与 §5.8 Agent capability surface 实现已完成；2026-10-17 第二轮逐文件审计新增兼容层清理与结构简化候选，当前顺序见[第二轮方案](refactor-plan-compat-design-audit-2026-10-17.md)。Slice 1–5 均已完成；当前实施 Slice 6 MCP protocol 收敛。Windows 发布及 HCS quota 验收仍是 §5.1 Open Gate。
> 更新日期：2026-10-17
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

**已完成。** `session_events` 是恢复权威；pending input 的持久路由、Ask 恢复、回滚截断和提交后 UI 发布各有明确边界。待投递输入在 durable marker 中保存 answer/steering/follow_up 路由，恢复不重算；显式投递及 turn/工作完成边界见 ADR 1156；见 ADR 0207、0209、0210、0385、0416、0440、0458。

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

状态按执行性质区分：**Gate** 是发布前签核条件；**Active** 是当前唯一实施切片；**Next** 是已有证据、等待前一切片完成后再进入的目标；**Candidate** 是已记录证据但不在当前顺序中的工作。结构性目标一次只推进一个 Active slice；行数下降、文件变少或 crate 变少不是完成指标。风险用于确定分片和验收，不单独作为暂缓理由。

| 状态 | 当前项 |
|---|---|
| **Active** | Slice 6：将 MCP protocol 固定到当前现代握手，按第二轮审计保留 stdio/HTTP 现代传输行为与安全边界。 |
| **Next** | Slice 6：将 MCP protocol 固定到当前现代握手，保留现代 stdio/HTTP、安全和取消行为。其后进行 owner 收敛及第一轮仍成立的模块职责切片，详见[第二轮方案](refactor-plan-compat-design-audit-2026-10-17.md)与[第一轮方案](refactor-plan-source-audit-2026-10-17.md)。 |
| **Gate** | Windows 发布验收 Open，见 §5.1。 |

### 5.1 Windows 发布验收（Gate / Open）

按 ADR 0395 的验收范围，在隔离的 Windows profile 或 VM 上使用当前构建完成并记录：

- 首启、Settings、会话、工具确认、媒体、后台/定时任务、恢复与回滚的真实 UI 流程；
- 当前安装包的安装、升级、卸载，以及卸载后的用户数据保留行为；
- 物理磁盘空间耗尽时的错误分类、用户提示和恢复操作。

用最新代码和安装产物复跑适用的 Rust/UI 门禁，并在 ADR 中记录实际构建版本、schema、环境、结果与限制。ADR 0395 中早期 profile 和门禁结果是当时的历史证据，不能当作当前工作树或当前安装包的通过结论。

可以提前准备隔离 profile、安装器和物理盘耗尽环境；最终验收必须使用交互生命周期等 IPC/UI 变更完成后的最新构建。此项是发布签核门，不阻塞不影响发布路径的独立模块整理。

### 5.2 交互生命周期所有权（行为已收口；恢复覆盖纳入新一轮）

Session、ScheduledToolRun 与 AppCommand 确认均由各自 owner 和稳定 request identity 路由；Session grant、resolve event/status 与 actor 唤醒顺序有明确的持久化和失败契约。交互 replay 失败保持 fail-closed，pending session 恢复有界并可取消地重试。当前行为契约与逐项验证见 ADR 0423/0424、0507–0522。2026-10-17 源码审计另发现 AppCommand pending 状态、入队/过期和 resolve continuation 分散于 `app_state.rs`、`commands/mod.rs`、`commands/session.rs`；下一轮将按新方案收拢内部 owner，不重定义既有交互契约。

源码复核修正了该边界：grant 写入后，Session confirmation resolution 与 `end_session` 由 `confirmation_resolution_gate` 串行；Actor 先 append `interaction_resolved`，成功后才移除 pending/唤醒 session，append 失败保留 paused pending。已有失败注入测试覆盖同一 actor 内重试且 grant upsert 不重复。进程崩溃会清除进程内 expiry task；启动恢复只自动载入 `status = pending` 的 session，因此该 paused session 要等显式 reopen 才会重建 actor、replay durable request/grant 并重新安排 expiry（过期时 delay 为零）。UI 打开历史会话前会调用 `reopen_session`；resolve IPC 对未加载 actor 返回 stale。新一轮验收明确覆盖此路径与 UI 行为，不将旧“未定义”结论当作事实，也不隐含承诺 unloaded paused session 在后台自动到期。session grant 的持久 owner 与 Agent/Memory 事务边界不变。

### 5.3 内部模块与 crate 边界（按证据复核）

本节记录当前源码已确认的结构候选与保留边界；历史决定和验证以对应 ADR 为准。候选依据当前生产职责、调用路径和状态读写方，不沿用旧文档的“高风险暂缓”结论；当前切片顺序见[第二轮方案](refactor-plan-compat-design-audit-2026-10-17.md)，第一轮模块职责切片见[第一轮方案](refactor-plan-source-audit-2026-10-17.md)。

#### 已复核候选与已知限制

| 状态                       | 问题与当前证据                                                                                                                                                                                                                                          | 重开条件与边界                                                                                                                                                                                 |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Next / 高影响** | `session_events.rs` 除 append、projection、rollback 外，还包含 fork、ingress、pending-input、recovery、tool-step、replay/read 和 usage 等生产职责。 | 仍由唯一 `SessionStore`/Memory owner 协调；逐步拆私有模块，保持 append/projection 同事务、rollback/usage 补偿同事务、提交后才广播；不暴露 backing DB 或投影 intent（ADR 0466、0479、0206、0915、1123）。 |
| **已完成（2026-10-17）** | layout session/permission 通知读取 `notifyCfg`，ToolRun completion 读取独立 store；Settings 保存只更新后者，造成同进程配置版本分裂。 | layout 现在拥有全类通知唯一运行快照；Settings 成功保存后发布新快照，durable-first partial-apply 时重读已写配置，普通保存失败不更新；revision guard 阻止初始异步读取覆盖晚到的新快照。验收见 ADR 1294。 |
| **Next / 已发生 drift** | 命令 boundary/security metadata 在 Rust、TS、文档间人工复制，脚本只比命令名；`execute_skill` 已出现 AuthorizationPort/AuthorizationEngine 文案差异。Rust `COMMAND_CONTRACTS` 当前未被逐字段核验，不能称为已经生效的机器权威。事件 channel 与 DTO 是两个 exhaustive match，二者配对目前不受同一 projection 登记约束。 | 将 Rust metadata 设为权威并生成/逐字段验证派生值；AgentEvent 一次生成 channel+payload projection 并与 generated shape registry 配对校验。当前 generator 已提供命令响应和事件 payload shape，不再把它列为缺失。 |
| **Next / owner 收敛** | AppCommand pending map 在 `app_state.rs`；入队/超时在 `commands/mod.rs`；仲裁/授权/continuation 在 `commands/session.rs`。Session grant append failure 同进程重试已有故障注入覆盖，进程重启恢复路径需纳入下一轮验收。 | 新建明确 App confirmation owner；`resolve_confirmation` 保持 Session/ScheduledToolRun/AppCommand 薄路由。Admin UI authorization/finalization 留在 App command adapter，policy 继续归 Tools `AuthorizationPort`；覆盖 grant durable / resolved event fail / restart / expiry。 |
| **Next / 私有模块整理** | `tools/security.rs` 同时实现授权 engine、process-start permit、filesystem policy、路径安全和 prompt summary；`agent/session/tool_runner.rs` 同时实现 step persistence、执行模式和 confirmation/scheduled lifecycle。 | 各自按职责拆私有模块；唯一 `AuthorizationEngine`/`SafetyConfig` 与 `SessionSupervisor` 不变。Tools 先提供唯一策略复核，Agent 仍负责会话生命周期。 |

**已关闭候选 — Session end 失败契约：** actorless、驻留 Actor 与运行中 Actor 的取消/清理失败语义、ToolRun claim 竞争及重试边界已按 ADR 0507、0514 收口并联合验证；当前无 Active 工作。只有同类失败/终态不一致再次复发时才重开。

#### 保留边界与较低优先级候选

- **Tools 其余边界保留：** `tool_contract.rs` 继续作为共享执行契约 owner；`builtin/admin.rs` 由 AdminServices 承接副作用，Admin 保留 operation/request/output contract；messaging adapter 继续复用 `haven_messaging`（[ADR 0213](adr/0213-operation-spec-single-policy-source.md)、[0391](adr/0391-admin-services-typed-output-projections.md)、[0396](adr/0396-messaging-domain-crate.md)、[0506](adr/0506-mcp-admin-connection-network-policy.md)）。本轮只拆 `security.rs` 内部职责，不挪 operation policy。
- **工具运行时三个集合各自保留（已复核，暂不合并）：** `ToolRegistry` 是已安装工具的权威注册表，保留注册顺序并拒绝重复名；`DeferredToolCatalog` 保存尚未激活、供发现和按需加载使用的定义；`SessionToolOverlay` 是某个 session 当前可执行的附加工具集合，loader batch 负责幂等、预算准入与 session version。published global/session catalog generation 由 `ToolCatalogVersion` 表达，不在 installed registry 复制一套无生产消费者的 version counter。三者分别表达注册、延迟发现和 session 执行作用域，不能因 lookup/list/definition projection 外观相近而统一成一个通用 Catalog。Provider 定义、校验、manifest 与执行的共享 turn-level immutable view 已由 `ToolCatalogSnapshot` 承担；共同规则若真实重复并导致漂移，再评估窄 helper。ADR 0145/0148 的 provider visibility 与 session 隔离不变（本轮名称对齐见 [ADR 0533](adr/0533-tool-runtime-nomenclature-alignment.md)）。
- **MCP prompt index 边界保留：** `McpServerIndexEntry` 是固定的 Tools→App→Agent prompt 摘要；tool schema/result 保持动态 JSON。仅在摘要新增稳定字段或出现独立消费者时重开（ADR 0516）。
- **Admin 风险 metadata 保留单一来源：** 共用操作由 `OperationContract` 拥有风险级别，parity 门禁覆盖共享 operation 集合；MCP refresh/reconnect 等 native-only 操作保持独立测试。operation surface 或风险策略变化时重开（ADR 0512、0528）。
- **Windows 子进程 containment owner 保留：** 受管进程经 `ProcessContainment` 创建、分配 Job 并恢复初始线程；命令策略、管道和取消等待仍归各 adapter。新增入口绕过该 owner 或后代残留时重开（ADR 0513）。
- **Agent 与 Memory owner 保留：** SessionActor 独占会话运行态，MemoryRuntime/Worker/Outbox 按生命周期与数据责任协作；队列顺序、取消边界和公平调度未形成可证明的重复 owner。出现跨 session 状态泄漏、恢复/取消回归或同一策略漂移时重开（ADR 0214、0424、0468、0475–0476、0481）。
- **MemoryWorker / MemoryOutbox 边界保留：** durable marker/episode IO 由 Memory `MemoryOutboxStore` 提供；Agent `MemoryOutbox` 拥有 scanner、调度、retry/ack 决策与生命周期；MemoryWorker 负责组合、推理和 prefetch。只有生命周期职责再次耦合或出现重复容量/取消状态时，才按 §5.5 复核（ADR 0518）。
- **App 与 UI owner 保留：** AppState、SessionReducer、Composer、Ask/Event/Session controllers 分属启动、状态、组件和流程生命周期；Router/media client 参数映射由 App builder 共享而错误降级与发布顺序留给 caller。仅在稳定后出现重复状态源、跨 session 泄漏或同类 lifecycle 回归时重开（ADR 0508–0510、0525）。
- **Tauri 输出 DTO 边界暂缓：** App-owned `SessionRecordDto`、Skills `SkillInfo` snapshot 与作为 Memory 命令 wire authority 的 `Fact` 各有明确 owner；MCP settings snapshot 暴露编辑所需字段并遮蔽环境变量，内部 refresh plan 不进入 IPC（ADR 0357、0720；详细字段见跨层输出契约清单）。当前没有输出字段漂移或额外消费者证据；只有 renderer 需要独立 shape、出现字段外泄或出现新消费者时再重开。
- **Common 与 ToolRunService 边界暂缓：** Common 继续作为无业务编排的共享基础 crate，`ConfigService` 仅保留 ADR 0068 定义的配置快照/串行 patch/原子持久化例外；ToolRunService 继续协调 Tools、Memory store、Agent 授权和 App lifecycle 的纵向流程。依赖方向、claim/cancel/cleanup/end 语义分别有 owner 与 ADR（0068、0359、0424、0507、0514）；没有收口后同类不变量复发。若出现重复权威状态、反向依赖、同一不变量回归或可证实的独立消费者收益，再按 §5.5 重开。

**体量准入：** 路线图不保留过期行数或提交频率快照。若 §5.5 出现符合条件的候选，再按当前源码与同环境测量职责、依赖和维护/构建收益；体量只触发复核，不构成拆分依据。

### 5.4 Common 拆分与性能优化（Candidate）

只有依赖图、重复 owner 或可复现 profile 表明存在明确收益时，才另立拆分/优化任务。crate 拆分须证明独立稳定 API、单向依赖边界及实际消费者收益；不得只为减少文件行数、构建目录或 crate 大小而拆 crate。性能比较使用相同工作负载、数据规模和环境记录前后结果；没有明显改善则关闭候选，不继续微调。

### 5.5 长期候选准入与复核

长期结构治理按“有源码证据的问题队列”推进，不预设日期或全仓重写目标。进入审查的证据包括：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、热点在多次变更中跨职责修改、依赖图暴露反向/多余依赖、同负载 profile 显示可复现问题，或单一实现文件承载多个可独立命名的生产操作族且其测试/调用者/生命周期边界彼此不同。文件超过约 800 行仅触发复核，不单独证明要拆；候选也不必等到先发生回归才可拆。

每个候选的短 ADR/评估要记录：现有 owner 与不变量、生产代码和测试的职责分布、调用/依赖边界、预期收益及观察方式、破坏面和停止条件、适用门禁、回滚方式。实施顺序固定为：证据确认 → 接受目标与切片 → 迁一条垂直调用链 → 删除旧入口 → 跑影响面门禁 → 对比 owner/依赖/性能指标 → 独立提交并更新状态。若只移动代码、扩大公共 API、暴露事务内部、增加第二权威来源，或验证不能证明维护/运行收益，立即停止并把候选记为“不需拆分/暂缓”。

长期执行按触发信号驱动而不按日历制造工作，优先级为数据/安全/生命周期不变量故障、重复跨 owner 回归或修改耦合、依赖/API 边界问题、最后才是有同负载证据的性能优化；行数和 crate 大小不计为准入分。仓库较大时可并行委派只读审计（例如依赖图、热点职责、事务/安全不变量），可由 sub-agent 或用户授权的其他对话承担；委派范围优先只读、交付具体文件/函数与反例，主执行者必须回到源码和门禁独立核验，不把审计结论直接当成实现要求。任何时刻只实现一个 Active 切片；跨对话协作不改变路线图、契约 owner 或提交责任。审查没有合格候选时，保留“无 Active 切片”状态并等待新证据，再继续同一套复核流程。

### 5.6 长期滚动执行周期（跨迭代周期）

按问题证据推进，不按日历、crate 数或文件行数制造工作。每轮从步骤 0 复核；历史决策和实现细节以对应 ADR 为准。

| 步骤 | 工作流                       | 进入条件与交付物                                                                                                                                                | 退出条件                                                                 |
| ---- | ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| 0    | **证据复核与分流**           | 切片完成、同类回归出现、依赖/API 改变或准备发布时，记录问题、不变量、owner、源码证据、影响面、停止条件和适用门禁；按 §5.5 选择 Active、Deferred、关闭或无候选。 | 只保留一个 Active/Next，或明确无候选；旧快照与体量数字不得单独触发重构。 |
| 1    | **数据、安全与生命周期契约** | 仅在 owner 重复、失败/恢复结果不一致或同类故障复发时，形成失败注入与恢复场景。                                                                                  | 成功、拒绝、重启、重试和取消语义由唯一 owner 表达并经适用门禁验证。      |
| 2    | **跨层权威来源**             | 有证据显示持久数据、事件、运行态、投影或 UI 之间发生漂移，才核对并收敛具体调用链。                                                                              | 不产生双写或第二真源；保留的投影边界可从契约中解释。                     |
| 3    | **稳定模块 owner**           | 重复策略/状态已经导致回归，且存在稳定接口、独立消费者或可隔离测试时，才提取或合并模块。                                                                         | 调用与测试归属真实 owner，依赖方向和外部行为不变，收益可复核。           |
| 4    | **crate/API 边界**           | 模块 owner 已稳定且存在单向依赖边界、独立消费者或可复核构建/维护收益。                                                                                          | API 清晰、无反向依赖或重复 adapter；否则保留现 crate 边界。              |
| 5    | **性能与容量**               | 同负载 profile 复现用户相关延迟、资源或容量问题；固定场景记录优化前后指标。                                                                                     | 改善超过噪声并达到目标，否则关闭候选。                                   |

**当前执行位置：** 阶段 0–8 已完成；历史覆盖记录（Rust 271/271、UI/App/IPC/文档 320/320）是当时审计快照，不代表此后源码与文档没有漂移。本轮 2026-10-17 源码复核确认 UI notification stale bug、命令 metadata 值检查缺口、Agent event channel/DTO 双映射、AppCommand confirmation 状态分散，以及 SessionStore/Tools security/Agent tool runner 内部职责可拆分。按新方案排队；目前不提出 crate 合并或拆分。Windows 发布验收仍是独立 Open Gate（§5.1）。

### 5.7 全项目领域术语与架构角色命名收敛（持续覆盖）

范围覆盖所有 Rust crate、Svelte/TypeScript、Tauri IPC/事件，以及架构、命名和契约文档。职责以 `docs/architecture.md` 为准，命名以 `docs/naming.md` 为准，历史决定与实现细节见对应 ADR。

本节此前的领域职责与历史完成事项保留为当时记录。第二轮源码审计确认的兼容清理与简化顺序见 §5.3 和[第二轮方案](refactor-plan-compat-design-audit-2026-10-17.md)；第一轮结构候选见[第一轮方案](refactor-plan-source-audit-2026-10-17.md)。历史覆盖表不作为当前源文件、命令 metadata 或 checker 状态的替代证据。

#### 当前审计覆盖与剩余范围

| 领域 | 当前状态 | 本轮覆盖结论与后续复核条件 |
| --- | --- | --- |
| Rust 类型、API 与架构角色 | 多轮 crate root、实现可见性、构造入口和 capability owner 审查已完成；Memory 写入用 Common 闭合 `ScheduleMode`，而持久读入保留 raw text 供恢复 quarantine（ADR 1126）；LLM 流式工具调用 DTO 显式使用 `tool_call_id` / `tool_index`（ADR 1130）；Session 集合 API 按 `list_*` 命名并用 `session_id` 表达实体参数，ToolRun 删除 helper 私有化，文件摘要读取改为具名结果（ADR 1134–1135、1138）；Memory/EventBus 实体身份参数显式命名（ADR 1139）。 | 本轮 11 个 Rust crate 的 production inventory 271/271 完成核对；本轮新增的启动协调器、原生 splash 与启动 command 已纳入清单。后续新增或改变 API 时复核可见性、消费者与 owner。 |
| Rust 调用、状态和身份 | 持久实体 ID、event/replay 写路径和若干生命周期动词已有统一规则；恢复 error partial 的无序 Thought 发布被限定到专用 helper（ADR 1081）；Memory 对负数持久 `run_id`、transcript domain append 与非 assistant `step-*` 行 fail closed（ADR 1087）；MCP server/tool 名称组件采用单射编码，防止注册表身份碰撞（ADR 1088）；App 任务以 JoinSet 收割已完成结果，同时保留活跃任务的 shutdown join（ADR 1093）；ToolRun lease subject 明确命名为 `lease_subject_id`（ADR 1096）；VAD worker 失败沿用 recording lifecycle auto-stop，stream receiver 关闭会回收 provider body reader（ADR 1102）；自动标题任务由 AgentLayer 持有，删除前 cancel/join，标题写入/事件发布与持久删除按 session lifecycle gate 串行（ADR 1105）；Agent 消费者、ToolRun completion、restore 和 router prewarm 由 task group 持有（ADR 1106）；shutdown 在 actor registry 清空前停止并 join dispatcher wrappers 和最终 cleanup retry，Messaging inbox 注销由调用方 await（ADR 1107）；prompt cache sections 改为具名字段，Memory 集合读取和实体参数按语义角色命名（ADR 1109）；取消退出最终 cursor 核验不继承已取消的 run token（ADR 1110）；MCP notification consumer 与 HTTP SSE listener 有明确 join owner（ADR 1111）；Memory 缓存 generation tombstone 有界，facts subject/all key 身份分离且失效原子化（ADR 1112）；MCP 全部 manager connection entry point 共用并发预算，配置加载的未收割任务数同时受限（ADR 1113）；InputPipeline 启动准入、终态 shutdown、native worker join 和 auto-stop identity 校验见 ADR 1114；ToolRunService runner、output emitter、scheduled worker 和 terminal/quarantine retry 由 TaskTracker 纳管，shutdown 的 admission fence、并发等待与完整 drain 见 ADR 1115；CanonicalToolCall 内部/durable 身份字段统一为 `tool_call_id`，并保留旧事件 replay alias（ADR 1128）；MCP connection reservation 由 manager 按 server name/generation 独占并统一取消清理与 shutdown drain（ADR 1129）；流式元数据及 ToolRun 局部身份字段统一标出实体/位置（ADR 1130）；Session 存储操作参数按 `session_id` 命名；Memory/EventBus API 与局部身份参数按实体命名（ADR 1135、1139）。 | 本轮调用、状态与身份项完成全量清点；后续变更复核实体 ID、位置型值与文档例外。 |
| UI、stores、controllers 与 renderer | 主要 IPC/event 实体身份已校验并在 renderer 边界映射 camelCase；ToolRun、Settings grant、Resume Ask 双重关联及交互展示字段已纳入同一规则（ADR 1074、1078）；SessionSummary 与 pending message identity 见 ADR 1082；补充卡 presentationKey、模型配置 ID 与交互生命周期见 ADR 1083；runtime session list allowlist 见 ADR 1086；MemoryRecall 使用 entity ID、UI Automation 使用 element token，MediaPlan store 有全局 session LRU 与终态清理策略（ADR 1091–1092）；Settings LLM config event 经 app mapper 进入 renderer（ADR 1108）；resume/submit 选择和 continue/rollback 的 UI 后续遵循最新用户选择代次（ADR 1125）；Skills/MCP/内置工具开关按资源串行化并在失败后读取权威状态（ADR 1133）；测试 setup 与 profile script 模块名已统一为 camelCase；PendingInteractionsMenu 的请求身份和回调参数统一为 `requestId`；长期事实列表增加分页和加载更多（ADR 1153）。 | UI production inventory 247/247 完成分类，新增 `startupCommands.ts` 已纳入；35 个关键文件逐行复核；后续 mapper/组件变更复核身份、字段和事件顺序。 |
| Tauri IPC 与事件 | Command response 和已登记 event channel 有生成的结构级运行时校验，handler 已有 UI owner 或明确的非 UI 归属；live usage 的单次成本与 `has_cost` 一致（ADR 1078）；runtime session list 由 App DTO 明确 allowlist，不直接序列化 Agent `SessionInfo`（ADR 1086）；WebSearch id-less aggregate completion 以可选 `call_index` 映射到 UI 并保持 renderer identity（ADR 1127）；OCR 凭据命令使用 generated 闭合 `OcrCredentialFieldInput`（ADR 1131）。 | IPC scripts 3/3 与 generated command/event shape 通过复核；后续 contract 变化由 Rust source 生成并用漂移门禁核验。 |
| 配置、持久字段与恢复 | 配置值域、单位、资源上限及主要 rollback/replay 不变量已有规范和实现校验；ToolRun 持久 `kind` 与封闭枚举一致（ADR 1080）；Skill 配置仅在目录扫描成功后与 catalog 原子发布（ADR 1090）；ConfigService 变更通知每订阅者有界且不阻塞写入（ADR 1097）；Skill requirements、目录条目与配置文件读取具有硬上限和失败保留语义（ADR 1104）；Skills manifest/requirements/入口脚本采用打开句柄身份校验，requirements fingerprint 与 pip 使用同一快照，执行脚本快照上限 16 MiB（ADR 1151）；无事件流 fresh-run 不把待投递输入重复加入 prompt history，并拒绝不可由初始输入/pending marker 还原的其它消息投影（ADR 1119）；配置解析/语义校验失败后的默认值回退为只读，阻止 Settings、配置更新和凭据暂存覆盖损坏源文件（ADR 1121）；SQLite 持久 CacheAccounting 读取拒绝三值词汇之外的存储值（ADR 1124）；Credential Manager target 使用小写 `haven/`，依赖 Windows target 大小写不敏感保持既有凭据可读（ADR 1132）；配置写入与外部修改 fingerprint 复用 4 MiB 有界读写契约（ADR 1140）。 | Common/Memory/Skills 配置与恢复范围已逐域核对；本轮 partial schema v42 重置说明已同步。未来持久语义变化更新 ADR 和 reset guide。 |
| Messaging 生命周期与容量 | 新写入 envelope 字段与序列化大小共用 contract（ADR 1095）；parent child cap 按累计身份计数（ADR 1101、1116）；删除 tombstone 清理 mailbox、processing、archive 与临时正文（ADR 1116）；每 recipient 16 MiB 逻辑 pending budget，超过后拒绝新投递；不可变 backlog + 原子 cursor sidecar 以 100 行/1 MiB bounded batch 排空，整批 ack 才推进 cursor；service/tool history 上限 100，find/last-received/selective reply bounded，broadcast 显示部分失败（ADR 1117）。旧 canonical inbox 不需重置；单行超过 1 MiB fail closed 并保留源文件。删除谱系 map 读取/写入上限为 64 MiB，替换使用 `.bak` 恢复先前 committed tombstone；OS `.lock` 文件持久存在并由文件句柄持锁，重置前需关闭所有共享进程（ADR 1136）。单行超限的唯一支持恢复路径为备份后完整 inbox reset，会清除所有 peer 队列与 tombstone；没有针对单行的 quarantine 工具。 | Messaging 4/4 production files 已核对；Envelope recipient/type 注释漂移已修正。Windows 文件替换仍归 ADR 1136/0395 相关验证和发布 gate。 |
| Tools、网络与操作系统边界 | 不可信 HTTP URL 共享目标校验、DNS pin 和跳转规则，HTTP request 限额与取消、图片 URL redirect 校验见 ADR 1098；MCP Ask/Restricted 复用 Common destination resolver，stdio read 中断后 poison/discard 并令 liveness monitor 可触发 reconnect（ADR 1103）；Registry names-only 枚举、256 条/类上限、截断标志与阻塞线程取消见 ADR 1099；raw ToolHandle getter 不进入默认生产 facade，见 ADR 1100；文件搜索分别统计遍历/读取失败并把搜索不完整状态传给 renderer（ADR 1118）；MCP manager 的连接 reservation、策略重检、代次安全清理和 shutdown drain 见 ADR 1129；Credential Manager target 使用小写 `haven/` 且无需迁移（ADR 1132）。 | Tools/Platform/Input production inventory 已分域核对；本轮确认并修复 VAD shutdown、MCP list refresh/SSE 等运行边界。真实 Registry 与进程树行为仍由 Windows 发布 gate 验收。 |
| 日志与诊断 | 动态敏感内容、长度单位和事件边界指标命名已收敛；Common 错误清洗器覆盖 JSON 敏感字段和 URL 密码 userinfo（ADR 1085）；Skill setup 不向模型观察传递原始安装输出或路径（ADR 1089）；Input capture 启动日志只保留采样率、声道数和格式，不记录用户自定义设备名称；静音/callback 诊断不输出样本幅值，本机路径错误不回显路径，配置 Debug 对命令、目录、URL path/query、代理例外和认证前缀使用占位符（ADR 1137）。 | 日志/错误与诊断生产路径本轮完成扫描分类，稳定失败文本与本机路径脱敏已同步；以后新增动态字段按 conventions 校验。 |
| 文档与 ADR | 命名、架构和 IPC 契约有明确权威文档；本轮修正 Rust/UI 错误日志边界、ApplicationRuntime 名称、真实 crate 依赖、Settings snake_case 例外、UI listener/TypeScript 示例及 `load_skill` 安全矩阵，并更新输出清单日期。 | 核心架构/命名/输出契约与历史 ADR 18 项完成交叉核对；按覆盖记录保存本轮结果，历史 ADR 保留历史表述。 |
| 架构合并/拆分 | 当前无 crate 级拆分/合并候选；已确认多个私有模块职责/状态 owner 候选，见 §5.3 与两轮重构方案。 | 完成排队切片后按源码与生产调用重新评估；不得用旧审计覆盖数或“高风险”标签替代当前证据。 |

#### 执行顺序与完成条件

每轮按“源码与调用点搜索 → 核对权威 owner 和边界语义 → 修正规范文档 → 实现最小一致性切片 → 运行适用门禁 → 记录 ADR 与当前状态”推进；一次只实现一个逻辑切片。跨 Rust/UI/IPC 或持久化边界的变更按项目要求运行完整门禁。审计结论须包含具体文件/符号、正反例和保留例外，不能仅以全库搜索无结果作为覆盖证明。

本节完成时，每个主要生产概念和跨层字段都有唯一规范词、唯一权威 owner 和可定位校验；剩余例外、provider 外部 ID、动态数据边界和历史兼容行为均明确登记；规范文档不再用已退役术语描述当前实现。新发现继续按 §5.5 证据准入，不以文件数量或重命名比例作为完成标准。

### 5.8 Agent capability surface 与执行边界（实现完成）

目标契约见 [ADR 1163](adr/1163-tool-exposure-codemode-workflows-and-execution-boundaries.md)。本节代码实现已完成；其后新增的内部模块候选按 §5.3 与第一、二轮方案跟踪。

- **工具暴露与执行模式：**`direct`、`model_only`、`deferred`、`codemode`、`hidden` 由同一确定性 resolver 驱动目录、加载、provider surface 与执行校验；MCP server 可切换 Legacy/CodeMode，并配置 per-tool overrides。CodeMode 使用无宿主 API 的 QuickJS 与有界子调用；每个子调用仍经 Tools 的实时授权、target、sandbox/network 和结果上限校验。隐藏或已撤权的旧调用 fail closed。见 [ADR 1183](adr/1183-add-mcp-model-only-exposure.md) 与 [ADR 1163](adr/1163-tool-exposure-codemode-workflows-and-execution-boundaries.md)。
- **MCP resources/prompts 与 Workflow：**资源和 prompt 由用户浏览、预览和选择，读取有类型与预算限制，不进入模型可调用工具面；Workflow 是有界静态 DAG，在当前 turn 内串行执行，每步经标准 Tools gateway 独立授权。需要确认的子步骤在 handler 前 fail closed；Workflow 不持久化、不后台运行且部分完成不自动重放。见 [ADR 1164](adr/1164-mcp-resources-and-user-selected-prompts.md)、[ADR 1165](adr/1165-bounded-declarative-workflow-validation.md) 与 [ADR 1166](adr/1166-execute-workflows-through-standard-tool-gateway.md)。
- **权限、workspace 与隔离：**`ExecutionWorkspace`、目录 capability、授权 proof、policy revision/workspace generation 和实时撤销由各自 owner 管理；文件副作用仍通过 Tools policy。隔离输出先进入有界 review owner，再经现有授权逐项晋升。Shell、模型 Skill 与受管 MCP 的 `WorkspaceWrite` 路径使用固定 HCS profile/broker 与配额证明；MCP 每次调用只读挂载有界 workspace snapshot。见 [ADR 1195](adr/1195-separate-execution-and-sandbox-workspaces.md)、[ADR 1209](adr/1209-require-os-enforced-sandbox-storage-quota.md)、[ADR 1268](adr/1268-wire-brokered-skills-to-live-authorization.md)、[ADR 1286](adr/1286-connect-managed-mcp-to-container-broker.md) 与 [ADR 1287](adr/1287-isolate-workspace-bound-mcp-invocations.md)。
- **受管 MCP package 证明：**catalog v6 的 `tree_sha256` 覆盖 package 普通文件树；扫描拒绝 reparse point、未证明的模块搜索目录和 NTFS 命名 alternate data streams。管理员摘要工具与启动 probe 共用扫描器。见 [ADR 1288](adr/1288-verify-managed-mcp-package-trees.md) 与 [ADR 1289](adr/1289-reject-mcp-package-alternate-data-streams.md)。

**保留的安全边界与发布验收：**`FullAccess` 下获准的宿主进程仍继承当前 Windows 用户的 OS 文件/网络权限；Haven 的工具授权不等于系统级进程隔离。没有可用且通过配额 attestation 的 HCS backend 时，受限执行 fail closed。管理页直接运行 Skill 在 `WorkspaceWrite` 下仍拒绝，因为该入口没有会话步骤身份与输出审阅生命周期。真实 HCS guest 超额写入、支持的 Windows build、broker token、固定镜像与安装包验收仍属于 §5.1 Open Gate；本仓库已实现的启动 probe 不能替代目标环境验收。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
