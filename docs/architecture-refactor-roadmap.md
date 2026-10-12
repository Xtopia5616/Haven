# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；§5.7 全项目概念与实现规范覆盖持续推进。已完成 ADR 1031–1052：IPC runtime shape 与语义校验、API owner、rollback 水位、稳定 inbox ack、Messaging 单一持久来源，以及运行时身份、跨边界 ID 校验和 UI 静态词表命名的一致性。尚未完成全库覆盖；继续核对剩余跨层概念、实现命名与文档契约。此前审计没有发现值得合并/拆分的架构候选。桌面 `list_facts` 分页仍待 UI 设计；Windows 发布验收为独立 Open Gate（§5.1）。
> 更新日期：2026-10-12
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
| **Active / Next** | **Active：**继续 §5.7 全库概念与实现规范覆盖；rollback projection watermark 与稳定 `message_ids` inbox ack 已完成（ADR 1043–1044）。Messaging 已统一为单一 JSONL 生命周期来源：Actor 移除或重启不再切换消息来源，未确认消息按原 ID 可重领（ADR 1045）。provider `tool_call_id` 逐字保留，durable `call-*` fallback 与进程内 `preview-*` 已分开（ADR 1046）。Interaction identity 现由 append 与 replay 共用同一校验，避免写入后恢复失败（ADR 1047）；Rust、UI 与 Tools schema 对 canonical ID 的前缀和长度规则现已收敛（ADR 1048–1049）；清理器与 UI 恢复/续接也只接受 canonical entity ID 作为持久身份（ADR 1050）；Common、Agent 和 Memory 现共用媒体资产身份校验，消息 ingress 在写入前拒绝无效引用（ADR 1051）。UI 静态词表与固定选项现按 UPPER_SNAKE_CASE 命名；resume usage 的 Rust 数据字段保持 snake_case，测试 fixture 模块遵守 camelCase 文件名（ADR 1052）。继续核对剩余跨层概念、实现命名和文档契约；此前 crate 边界审计没有发现值得合并/拆分的新结构候选；桌面完整 `list_facts` 分页仍待 UI 设计。 |
| **Gate** | Windows 发布验收 Open，见 §5.1。 |

### 5.1 Windows 发布验收（Gate / Open）

按 ADR 0395 的验收范围，在隔离的 Windows profile 或 VM 上使用当前构建完成并记录：

- 首启、Settings、会话、工具确认、媒体、后台/定时任务、恢复与回滚的真实 UI 流程；
- 当前安装包的安装、升级、卸载，以及卸载后的用户数据保留行为；
- 物理磁盘空间耗尽时的错误分类、用户提示和恢复操作。

用最新代码和安装产物复跑适用的 Rust/UI 门禁，并在 ADR 中记录实际构建版本、schema、环境、结果与限制。ADR 0395 中早期 profile 和门禁结果是当时的历史证据，不能当作当前工作树或当前安装包的通过结论。

可以提前准备隔离 profile、安装器和物理盘耗尽环境；最终验收必须使用交互生命周期等 IPC/UI 变更完成后的最新构建。此项是发布签核门，不阻塞不影响发布路径的独立模块整理。

### 5.2 交互生命周期所有权（已完成；一项边界暂缓）

Session、ScheduledToolRun 与 AppCommand 确认均由各自 owner 和稳定 request identity 路由；Session grant、resolve event/status 与 actor 唤醒顺序有明确的持久化和失败契约。交互 replay 失败保持 fail-closed，pending session 恢复有界并可取消地重试。当前契约与逐项验证见 ADR 0423/0424、0507–0522；此域没有 Active 实现候选。

**暂缓边界：** Session grant durable write 成功、`interaction_resolved` append 失败后，如果原 actor 在两步之间退出，stale request 的重试/过期语义尚未定义。现有契约允许调用方对原 pending 请求重试，但没有规定 actor 退出时的跨 owner 补偿。只有出现可复现的卡死/重复授权问题或新增明确产品要求时，才重新定义该边界；不预先引入跨 owner 事务或第二授权状态源。

### 5.3 内部模块与 crate 边界（按证据复核）

本节只保留未解决项与已复核候选的重开条件；已完成切片的背景、决定和验证以对应 ADR 为准。私有模块整理与 crate 拆分都必须通过 §5.5 准入，不以文件或 crate 体量为目标。

#### 已复核候选与已知限制

| 状态                       | 问题与当前证据                                                                                                                                                                                                                                          | 重开条件与边界                                                                                                                                                                                 |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **高风险 Candidate，暂缓** | `session_events.rs` 的 append、transcript projection、rollback、cache invalidation 和 post-commit broadcast 同属事务敏感的 SessionStore owner。只读 history 测试与 SessionStore facade 已分别收口；当前没有同类事务/rollback 缺陷复发或独立消费者收益。 | 仅在同一事务不变量修复后复发、无关修改持续耦合进核心，或测试职责无法隔离且能证明收益时重开。拆分不得暴露事务内部、改变 append/projection/rollback 原子性或引入第二恢复来源（ADR 0466、0479）。 |

**已关闭候选 — Session end 失败契约：** actorless、驻留 Actor 与运行中 Actor 的取消/清理失败语义、ToolRun claim 竞争及重试边界已按 ADR 0507、0514 收口并联合验证；当前无 Active 工作。只有同类失败/终态不一致再次复发时才重开。

#### 已复核但不进入 Next

- **Tools 与安全边界：** `tool_contract.rs` 继续作为共享执行契约 owner；`builtin/admin.rs` 由 AdminServices 承接副作用，Admin 保留 operation/request/output contract；messaging adapter 继续复用 `haven_messaging`。只有稳定后再次出现 policy/schema drift、同边界回归或独立消费者，才重新评估私有模块（[ADR 0213](adr/0213-operation-spec-single-policy-source.md)、[0391](adr/0391-admin-services-typed-output-projections.md)、[0396](adr/0396-messaging-domain-crate.md)、[0506](adr/0506-mcp-admin-connection-network-policy.md)）。
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

长期结构治理按“有证据的问题队列”推进，不预设日期或全仓重写目标。候选只有在出现下列至少一项时才进入审查：同一业务状态被两个 owner 维护、调用链反复跨不稳定边界、重复分支已导致 bug/回归、某热点在多个变更中频繁发生跨职责修改、依赖图暴露反向/多余依赖，或同负载 profile 显示可复现的资源/延迟问题。文件超过约 800 行或多于两个独立职责只触发复核，不单独证明要拆。

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

**当前执行位置：** 阶段 0–8 已完成；全库术语与实现规范审计仍在进行。此前已完成 API owner、持久身份、UI command/event contract、配置规则和文档引用的多轮切片，全部 command response 与 event channel 有结构级 runtime coverage，语义约束由各领域 owner 校验。本轮进一步发现并修复 branch projection watermark 只覆盖 messages 的持久化边界缺口（ADR 1043）；Interaction append 与 replay 已复用 identity validator（ADR 1047），继续覆盖进程内 ID 穿入 durable history 的边界。现有证据仍不支持新的 crate 合并/拆分。§5.3 的 Deferred 候选保留其各自重开条件；Windows 发布验收是独立 Open Gate（§5.1）。

### 5.7 全项目领域术语与架构角色命名收敛（持续覆盖）

范围覆盖所有 Rust crate、Svelte/TypeScript、Tauri IPC/事件，以及对应架构、命名和契约文档。职责以 `docs/architecture.md` 为准，术语与角色动词以 `docs/naming.md` 为准；已完成决定和实现细节只保存在对应 ADR，不在路线图复制实施日志。

本轮持续检查 Rust crate 可达 API 与调用图、跨层身份写入、配置 load/update owner、UI command/event response 和语义校验、架构与命名文档引用。此前 Messaging、MCP 和 Memory 普通生产路径已由领域 facade/store 承接，具体 transport、client、repository 和数据库装配归各自 owner；跨 crate 测试注入使用非默认 `test-support`（ADR 0887–0892）。近期 API 和 IPC 复核已分别记录在 ADR 1041–1042。继续检查持久写路径时发现 `last_msg_at` 只读取 messages，可能在 rollback 时误删更晚写入的分支内 step/usage；现通过跨三表单调水位、事务化写入与 schema v40 reset 要求收口（ADR 1043）。
继续核对运行时身份时发现显式 `agent.inbox` 的 `claim-*` 虽被文档定义为进程内，却随 ToolResult 进入 durable transcript；现改为稳定 `message_ids` ack，并为旧 transcript 参数返回迁移提示（ADR 1044）。对 Messaging 恢复路径的审计又发现同进程 Actor 的易失队列与 JSONL 竞争权威，并可能使 claim/ack 随 Actor 生命周期切换；现已删除 Actor inbox 和 `SessionMailbox` 分支，由 JSONL transport 独占投递、领取、确认、历史与回复查询（ADR 1045）。`call-*` 现明确为缺失/重复 provider ID 的持久 fallback，原始 provider ID 按不透明值保留；流式 preview 使用独立 `preview-*`（ADR 1046）。Interaction identity 由 append 和 replay 共用 `validate_interaction_ids`，避免 durable event 写入成功、恢复时才拒绝（ADR 1047）；Rust 与 UI `is_canonical_id`/`isCanonicalEntityId` 现对 prefix 字符及整体 ID 长度执行相同规则（ADR 1048）。Tools builtin 的 37 处持久实体 ID schema 现共用 `canonical_entity_id_schema`，并有 schema validator 对照 Common checker 覆盖边界（ADR 1049）。媒体文件清理、续接与 resume 持久身份分类现复用同一 ID 规则，不再仅按前缀判断（ADR 1050）。Common 的 `MediaInput::validate_asset_identities` 现由 Agent transcript 与 Memory message write 共用，防止带坏资产引用的 ingress 写入 messages 和媒体投影（ADR 1051）。UI 静态词表/固定选项统一使用 UPPER_SNAKE_CASE；resume usage 行保留 Rust wire 字段名称；测试 fixture 模块按 camelCase 命名（ADR 1052）。全库概念与实现规范覆盖仍未完成，继续按下表逐域审计并收敛文档和源码。

Agent 的跨 crate 生产启动只经 `AgentLayer::start_after_memory_ready`；`SessionSupervisor::start_dispatcher*` 与 `SessionRunHandler` 仅供 Agent crate 内部编排和测试使用（ADR 1005）。

Input、MCP、LLM、Tools 与 Common 已收回无跨 crate 生产消费者的辅助 API；`InMemoryCredentialStore` 仅由单测或显式 `test-support` 开启，生产依赖不包含该替身（ADR 1023）。`LlmRouter::config` 已移除；Agent、Tools 与 Admin 现经窄能力及安全诊断接口读取（ADR 1029）。

`ConfigLoader` 的生产写入入口（`save`、`config_mut`）现为 Common crate 内可见，`apply_settings` 与路径注入 `load_from` 仅限测试/test-support；Common 数值/名字校验 helper 不作为外部 API，Service 仍是唯一写入、版本通知与 runtime apply owner（ADR 1023–1024）。

Diagnostics command response parsers 与类型统一归于 `contracts/diagnostics.ts`；`LogInfo.level`、LLM connection status/reason 与 metrics phase 数量复用生成的 Rust contract，固定数组长度由 UI 边界严格校验（ADR 1025）。

Messaging JSONL envelope、Agent registry、Agent tool schema/result 与 UI 专用 renderer 已统一使用显式 message/session identity 字段；旧 inbox 字段不做 alias，升级重置要求见 ADR 1026 与 `release-and-reset.md`。

UI 执行阶段 store 的文件名现与 ReAct phase owner 一致；session 启动流程统一称 resume；ToolRun `ScheduleMode` 和 guard 直接由 `contracts/toolRun.ts` 提供，renderer 不重复定义 alias（ADR 1027）。

Durable transcript 的媒体清理辅助函数使用 `for_durable_transcript` / `canonical_for_compaction_event` 职责名，`continue_session` 按 `session_events` replay；BranchPoint append 无效 `force` 参数已删除（ADR 1006）。

Common `CanonicalMessage`、Memory `SessionMessageText` 与 Agent `SessionPromptMessage` 统一使用 `message_id`；CompactSummary replay 只为历史 event 的 `id` 提供反序列化兼容，新事件写明确字段。ToolRun completion 派生的 `msg-*` identity 作为 delivery 幂等规则写入 ID 规范（ADR 0998）。

SessionStore event replay 的固定页容量已收回 Memory typed store，Agent runtime 只传 session、cursor 与 cancellation，不再依赖 Memory 的页大小常量（ADR 0911）。

MemoryFactStore 不带抽取 cursor 的批量写入现仅在测试和 `test-support` 构建暴露；旧的无条件 summary marker ack 已删除，生产只保留 generation 条件确认（ADR 0912）。

SessionStore 的普通生产读取现统一经过 async/cancellable typed API 与 blocking pool；通知标题 cache miss 也不再同步查询。仅保留退出回调必须同步执行的会话暂停；pending recovery cancellation 可到达 SQLite 查询（ADR 0913）。

Memory 的 `Session`、`Message` 与 `Fact` 领域模型分别使用 `session_id`、`message_id`、`fact_id`；repository 负责映射现有 SQLite `id` 列。该变化不触及 schema、event 或 IPC shape，事实抽取提示词约定的 JSON `id` key 保持原样（ADR 1028）。

LLM 配置快照只由 LlmRouter 持有；跨 crate 消费者经窄方法读取已解析的上下文窗口、embedding route opaque identity、STT/provider search 能力和无凭据的 route diagnostics。embedding-v2 的 vector-space key 保持既有格式（ADR 1029）。

Session resume response 在 command contract 边界映射 interaction renderer view，reducer 只接收 typed interaction；optimistic/persisted message identity 与局部集合命名使用完整实体词（ADR 1030）。

ToolRun cancel 在 App command 边界从 wire DTO 映射至 Tools runtime enum，capability 不传字符串；WebSearch phase 复用生成闭合值集，UI mapper 拒绝未知 phase（ADR 1031）。

Memory usage 与 Agent live event 使用 `request_kind`；新 `usage_recorded` durable payload 写 `request_kind`，旧 `role` 只作为 replay 反序列化 alias，SQLite 物理列 `llm_usage.role` 保持不变（ADR 1032）。Committed event projection 使用 `event_sequence` / `eventSequence`，流式分片仍使用 `seq`（ADR 1033）。Input `RecordingReason` 仅描述实际产生 `RecordingResult` 的停止原因；取消仍由 App lifecycle DTO 表达（ADR 1034）。Common/LLM model-config API 与 Session、FactExtraction、Messaging、ToolRun 调用身份均已使用完整领域名称（ADR 1035）。UI command registry 仅作 owner metadata，bootstrap status 与 builtin manifest projection 在 renderer 边界校验闭合值与唯一身份（ADR 1036）。

#### 当前审计覆盖与剩余范围

| 范围 | 已核对的当前基线 | 长期复核/重开条件 |
| --- | --- | --- |
| Rust 类型与架构角色 | 主要 owner 词汇、跨 crate ID 校验、Skills 测试构造入口、Messaging transport、MCP client、Memory 数据库/store、授权、资产、Skill execution 与 live-output ports，以及 Agent/App 的 ToolRun、MCP、Skills、Admin 消费端和 Tools owner capability ports 已有收敛（ADR 0533、0554、0615–0676、0880–0908）。`ToolRegistry`/`OperationRegistry` 已退回 Tools 内部；`SessionToolOverlay` 不再向 loader 暴露可变 map/version 锁，预算预览、原子批量注册与版本递增由 owner 管理。跨 crate catalog fixture 只能经 `ToolCatalogTestSupportPort` 注入；生产 `ToolsFacade` 不再公开任意 session-tool 注册。MCP server profile 只由 `ConfigService` 保存和读取，Tools 不再同步维护可变配置副本；`McpManager` 只拥有活动连接与发现状态。McpManager、SkillRegistry 和 AdminOperationSet 留在 Tools owner 内部；App/Agent 分别通过窄 MCP runtime、Skills catalog 与 Admin operation ports 调用（ADR 0903–0904）。Tools 的实现模块默认不可达，App 通过 facade 重置工具熔断器，跨 crate 测试只通过非默认 `test-support` 访问 builtin/SkillRunner；App 生产资产生命周期只消费 `ManagedAssetLifecyclePort`，具体 `ManagedAssetRegistry`/`ManagedAsset` 留在 Tools 内部并仅供 `test-support` 测试使用（ADR 0940）；`OperationSpec` 仅以 crate-root opaque 契约类型满足公开 `Tool` trait 签名（ADR 0905）。ToolRun lifecycle payload 由 Tools 在 crate 内构造，App 仅消费 payload contract 并投影到 IPC；跨 crate 投影 fixture 的构造入口仅在非默认 `test-support` 下可用（ADR 0952）。Memory 的 repositories/database/schema 模块均不可由下游寻址；生产只消费根导出的 typed stores/DTO，数据库装配和 store 构造由 `MemoryPersistence` 拥有。`EmbeddedText`、页容量、仅供测试的 partial row DTO 与 Memory 内部策略均不进入默认生产根 API；recall 使用 `MemoryHit`，分页上限由 Memory typed store 持有，跨层可见 fact policy 仅保留真实消费者所需项（ADR 0902、0906–0908）。SessionStore 普通生产 SQLite 操作经 async/cancellable store API 进入 blocking pool，仅保留同步退出暂停例外（ADR 0909、0913）；Memory outbox store 使用 `MemoryOutboxStore` 职责名，`MemoryStores` fields 私有并由具名 accessor 提供共享 clone，防止混拼不同 persistence 实例（ADR 0910）。Memory 的 `tool_runs.rs` 统一承载 background/scheduled ToolRun 的底层持久操作与行模型；`tool_run_store.rs` 是跨 crate typed async façade，completion delivery outbox 仍由独立模块负责，旧文件名 `scheduled_tool_runs.rs` 已不能准确概括其范围（ADR 0914）。`SessionCommitted` 保留 typed write methods，但其集合及投影意图类型收回 Memory 内部，Agent 不再能直接修改存储意图向量（ADR 0915）。Agent 的交互类型与 replay 函数通过 crate 根导出，`interaction` 实现模块不再对下游开放第二套等价路径（ADR 0916）。Agent crate 根保留 Agent-owned 跨 crate 契约，不再重导出 Memory 自有的 `SessionStore`、`SessionEvent`、`SessionEventInput`（ADR 0939）；ReAct、transcript projection、compactor、Memory worker、prompt renderer 与 session run engine 留在 Agent 内部；生产 run 唯一经 Supervisor dispatcher，direct-run lease/waiter/Actor command/barrier 与仅为该路径服务的 `SessionRunPaused` 事件退役，direct-run 生命周期回归仅在 `cfg(test)` 构建保留（ADR 0917）。Agent session 模块内部现在从 `haven_common::lifecycle` 导入 Common 所有的状态类型，Agent 根不再暴露 `SessionStatus` / `SessionWaitingReason` 别名（ADR 0933、1001）。 | 继续逐项检查各 crate 根导出的 DTO/store operation 与公开构造器；核实真实消费者、字段可见性、分页/事务/安全约束和命名，收回无消费者或泄漏实现参数的入口。 |
| Memory recall capability | MemoryTool 的 Recall 只经已绑定的 `MemoryRecallPort`；`MemoryFactStore` 不再提供并行 keyword recall，缺少 recall capability 时显式报错（ADR 0918）。内部 `MemoryRecallSlot`/`OnceLock` 留在 Tools，App 只注入 port（ADR 0955）。 | 持续核对其他跨 crate 调用是否绕过 owner port，以及 capability 注入时机是否由组合根保证。 |
| Memory fact-write policy | `MemoryFactWrite` 不再携带调用方计算的 single-valued 标记；FactGraph 在 Memory 内按规范谓词决策，`is_single_valued_predicate` 也不再是跨 crate 根 API（ADR 0919）。 | 继续核对跨层输入 DTO 是否携带了实际属于持久化 owner 的策略快照或重复判断结果。 |
| ToolRun history boundary | App 不再接收 Memory `ToolRunRow`；Tools 复用 `ToolRunView` 为 board/history 提供 typed safe projection，终态过滤和 history limit 下推到 SQLite query（ADR 0920）。Memory typed store 不再公开无消费者、无界返回原始 ToolRun rows 的列表方法；board hydration 与 history 分页使用各自具名查询（ADR 0977）。 | 继续核查其它管理能力是否把存储行或 owner 策略泄漏给 App/UI。 |
| Memory model-facing fact reads | MemoryTool 的 search/list 端口都要求显式 limit，由 Memory 在可见性过滤后截断；全局列表按有效置信度排序，删除会误称时间顺序的 `recent` API（ADR 0921）。`MemoryQuery` 只能经校验构造，事实抽取 store 对 prompt 结果再次封顶为 64；配置默认 40，TOML 超限只影响内存快照，Settings 超限拒绝。桌面 `list_facts` 仍完整返回，避免无分页 UI 静默丢失浏览/删除能力（ADR 1037）。 | 继续审查事实候选的大表扫描成本；若事实量增长成为可复现问题，再设计完整分页/虚拟列表与排序一致性。 |
| Tools capability handoff | App/Agent adapter 在 composition 阶段取得窄 capability 并缓存；运行期不再重取完整 `ToolServices` bundle，App bootstrap 复用已装配 Skills port（ADR 0922）。 | 继续核对其它 facade/bundle 是否进入运行期调用链，及 adapter 构造器是否只接收所需能力。 |
| Rust 调用与函数动词 | Input、Tools、Memory、Agent、LLM 和 App 的调用动词、副作用 owner 与共享纯函数已完成多轮审查（ADR 0570–0698、0861–0879）。 | 继续核对私有与 `pub(crate)` 函数、参数/返回值、trait 方法及调用链；只在职责或权威状态重复时合并，并按可观察行为统一动词。 |
| UI components、stores、controllers、handlers 与 contracts | 部分 controller 生命周期、生成类型复用、无消费者 alias、Tool result 输出校验和 Session 状态投影已审查（ADR 0595、0665–0670、0698、0745、0841–0846、0860）。MCP list/refresh response 已完整校验 profile identity 与快照字段，列表快照映射为 camelCase renderer view；动态 JSON Schema 保持开放数据（ADR 1002）。Skills list 校验完整 `SkillInfo` 和 name 唯一性并映射为 camelCase view，execute response 校验固定 envelope 且保留动态 output（ADR 1003）。ToolRun board/history command row 要求 status，并使用与稀疏 lifecycle payload 分离的 `ToolRunCommandRow`（ADR 1004）。session、Agent、ToolRun、recording 与 interaction event mapper 按 entity 前缀和 UUID32 校验身份（ADR 0945）；Session command adapters 校验 history、lineage、runtime list 与 resume identities，并验证 message/step/session 关联（ADR 0946）；Memory commands 校验 fact、source message 与 kind-specific recall identities（ADR 0947）；`process_transcript` response 在 Session command adapter 校验后才进入 submit state transition（ADR 0948、0968）；Settings command adapter 校验 Session permission grant 的所属 session identity（ADR 0949），credential staging result 与已持久化 Settings response 中的 credential references 均校验为 `cred-*`（ADR 0950–0951）。Session resume response 现由 command contract 统一映射 interaction renderer view，reducer 不再解析 wire DTO；accepted action 的乐观/持久消息身份字段及恢复集合名使用完整 message 术语（ADR 1030）。本轮同时按 store 角色统一 context-menu 模块名，并规范派生集合命名（ADR 0969）。 | 继续检查其它 command response、persistence/replay ID 写入和恢复路径，并区分 provider 外部 ID。 |
| Tauri IPC 与事件 | 命令静态 request/response 有 Rust DTO 生成来源，事件 channel 有 Rust 登记和 UI mapper；若干枚举、目标命名、字段和 App 命令 ID 入参已收敛（ADR 0613、0699、0730–0745、0883）。UI event mapper 校验 Haven identity；Session command response 校验 session、message、step、usage 与 interaction 身份，并将 resume interaction 映射为 renderer view；Memory command response 校验 fact/source/episode identity；MCP list response 校验完整 snapshot 并映射 camelCase renderer view，refresh response 校验 profile name 分类（ADR 1002）；Skills list command response 校验完整 DTO 并映射 camelCase view，execute command response 校验固定 envelope（ADR 1003）；ToolRun command row mapper 校验 status，同时保留 lifecycle payload 的稀疏投影（ADR 1004）；supplement event 的 `inject_source` 按 Rust `InjectSource` 生成值集校验（ADR 1014）；实际调用的 boolean command response 经过运行时校验（ADR 1015）；`process_transcript` 由 Session command adapter 校验 session/message result；Settings parser 校验 Session identity、Rust capability scope 语法、staging credential 与 persisted `cred-*` 引用；provider 外部 ID 保持原格式（ADR 0945–0951、0968、1041）。RequestKind 与 event sequence 字段分别使用 `request_kind`、`event_sequence`，bootstrap status 拒绝未知命令值（ADR 1032、1033、1036）；Rust generator 为全部 command success type 与 36 个事件 channel 生成 runtime shape，统一 invoke 和 event mapper 先验证结构字段，再由领域 mapper 校验 ID、通道不变量和跨字段语义。79 个 handler 均有 UI owner 或有理由的 non-UI disposition；invoke 字面量覆盖单双引号与模板字面量，Tauri core API 入口受限于 `tauri.ts`；app-binary emit site 由 checker 按 source/count 审查。Live 与恢复 usage 成本均拒绝负数（ADR 1041）。 | 继续审查其它 owner 的语义校验、producer/channel 约束及持久化 ID 写入；生成 shape 覆盖的是 JSON 结构，不替代领域语义验证。 |
| 架构、配置、持久字段与文档引用 | 部分配置 owner、schema 拒绝规则、Serde alias、连接名与供应商身份、数值约束方向及单位已审查（ADR 0632–0633、0971–0976）；模型与 STT/TTS/图像生成配置的连接引用使用 `provider_name`，OCR 能力与解析后的运行时 backend 保留各自 `provider` 语义；响应 token 下界叫 `response_token_floor`；文件工具读写分别设限、MCP binary cap 明确计量 Base64 表示字节，SKILL 行长上限明确按 UTF-8 bytes 计量，ToolRun 上限分别命名为等待中的定时任务数与运行中的后台任务数。Settings 数值 wire 类型/空值和模型完整性现对照生成 contract 校验（ADR 0999）；TOML/Settings 对 provider name、model config ID 与 RequestKind 重复键统一拒绝（ADR 1038）。Agent rollback 与 `continue_session` 现按 ADR 0502 描述 `session_events` replay，不再称为 ReAct snapshot 恢复；`ipc-contracts.md` 的全量命令表已修复被 Memory 说明段打断的 Markdown 结构。 | 继续核对当前规范文档和源码中的语义命名、配置字段以及 Memory/SQLite 写路径；旧 ADR 历史名称保留。 |
| Input crate API | `capture`、`vad`、`hotkey` 实现模块不再外露；真实跨 crate 类型只从 `haven_input` crate root 使用，按键 modifier 是强类型，组合只能经 parser 构造。移除 raw-bitmask 构造器与会伪造空 Manual 录音的 `Default`，保留有效 idle pipeline 的 `Default`（ADR 0926）；Common 所有的 `AudioConfig` 只经 `haven_common::config` 导入，Input root alias 已移除（ADR 0941）。 | 继续核查其他 crate 的 root contract、构造入口和实现模块可达性；再覆盖 UI、IPC/event、配置/持久字段及文档引用。 |
| Skills crate API | venv 实现模块私有；无消费者的 `VenvManager::root` 与 Tools 默认根导出收回。文件 fingerprint 与比较状态留在 `SkillRegistry`，Tools watcher 只调用 `refresh_if_changed` 并负责工具目录重建/通知；不再跨 crate 暴露 watcher DTO（ADR 0927）。Skill 执行在 Tools owner 查询当前 enabled/script 状态，不依赖 App 较早的快照（ADR 1039）。 | 继续逐个审查其它 crate 的跨边界操作是否泄漏实现 DTO/状态，及构造入口是否允许无效状态。 |
| Common shared contracts | `ToolRunCompletionPayload` 与 `ToolRunLease` 都从 Common 根导出；两个单类型实现模块私有，Memory/Tools 对 lease 统一使用根路径，不再用 `doc(hidden) + pub` 暴露平行入口（ADR 0928）。`usage` 是文档规定的领域命名子域，六个类型只从 `haven_common::usage` 使用，移除无消费者的根 re-export（ADR 0929）。配置 DTO 与 helper 改为只走 `haven_common::config`；Common 根不再重复导出 config aliases（ADR 0930）。15 个工具 contract 只从 `haven_common::tools` 使用，移除无消费者的 Common root re-export（ADR 0931）。media 与 media_detection contracts 统一从各自子域导入，Common root 不再重复导出 aliases（ADR 0932）。Session/step/ToolRun lifecycle 状态只经 `haven_common::lifecycle` 使用，删除 Common root 的重复导出（ADR 0933）。`discover_workspace_root` 保持 Common root 唯一入口，workspace 实现模块私有（ADR 0934）。MCP transport 更名为 `McpServerTransport` 并只从 `haven_common::mcp` 公开，Rust、IPC 与 UI 使用同一语义名称（ADR 0935）。`ConfirmId`/`RecordingId` 的 typed constructors 与反序列化现在校验固定前缀，并阻止外 crate 绕过校验直接 tuple 构造（ADR 0936）。 | 继续核对 Common 其余公开模块哪些表达稳定领域子语言、哪些只是可收回的实现路径；避免将必要的领域分组一律扁平化。 |

API owner 复核收回了无生产消费者的 `ConfigService`/`ConfigLoader` 入口、Skills 原始 registry getter、Tools facade 测试辅助/内部操作公开面，以及 `DetectedMediaKind` 重复路径。生产技能存在性检查改用 `SkillInfo`；raw `SkillRegistry::get_skill` 只在单测或非默认 `test-support` feature 可用。`Skill::instructions` 保留作者提供的 SKILL contract 内容读取能力（ADR 1042）。

ToolRun lifecycle、session-attachment 与 output-preview payload 均由 Tools 在 crate 内构造，App 仅消费 payload contract 并投影到 IPC；跨 crate projection fixture 仅通过非默认 `test-support` 暴露（ADR 0952–0953）。

completion 与 scheduled-fire DTO 的构造权限也收回 Tools，Agent/App 保留读取消费能力但不能通过 struct literal 伪造 producer-owned contract；当前没有跨 crate fixture 需求（ADR 0954）。

MCP refresh plan/target 由 Tools 从 manager reconciliation 生成，App 仅读取 projection 并在单测中通过非默认 `test-support` 构造 fixture（ADR 0956）。

Managed asset registry 的注册、解析、查询和 lease 边界统一校验 `asset-*` 与 `ses-*` 规范身份；非法输入在写入前拒绝，失败不改变资产或 lease 状态（ADR 0957）。

Memory 创建 `AgentSpawn` session 前校验父 `ses-*` 身份，读取持久化 provenance 时重复校验；畸形身份不写入，也不会作为有效 session 来源投影（ADR 0958）。

Committed UI bridge 遇到缺少旧 UI annotation 且没有 `message_id` 的 supplement 时，经 Common ID generator 产生符合 UI contract 的 `msg-*` fallback，避免事件 mapper 拒绝该投影（ADR 0959）。

`ScheduledToolRunSpec` 只在 Tools 内部生产调度和非默认 `test-support` fixture 中使用；Tools crate root 不再向默认生产依赖导出其可直接构造的字段（ADR 0960）。

Memory fact extraction 的 session、message cursor 与 summary episode 持久边界统一校验 canonical ID；畸形输入在写入前拒绝，畸形持久 cursor 在读取时失败关闭，既有合法 KV key 与 schema 保持不变（ADR 0961）。

ToolResult envelope 与 UI mapper 仅接受 canonical `asset-*` 引用；录音停止原因与 error/transcription 必需字段在 UI contract boundary 失败关闭，保留合法空转写、零时长及 ADR 0694 的 VAD 默认（ADR 0962）。

Memory messages、session_steps 与 FactGraph 的 transcript identity 在 owner 写入前及 row mapper 读取时统一验证 canonical ID；保留 thought/ask 的 `step-*` 共用身份和 supplement 的 `msg-*` 共用身份，schema 与 replay wire shape 不变（ADR 0963）。

Admin `sessions` / `errors` 工具结果将会话身份字段统一为 `session_id`；UI 专用 renderer 要求 canonical `ses-*`，畸形数据回退通用 JSON（ADR 0964）。

Memory usage batch 写入在投影前校验 `ses-*`；usage discard 在追加补偿事件前校验 `ses-*` 与 `usage-*`，拒绝路径不改变事件或用量投影（ADR 0965）。

Agent transcript 的 seed/append 与 resume/replay 使用同一身份校验：Thought/Reasoning、ToolResult、用户补充、compaction 和嵌套媒体引用按实体前缀校验；Committed UI 注释校验 step/message/asset ID 并关联所属 record。provider tool-call IDs 保持不透明；Memory 继续不解析 Agent payload（ADR 0966）。

Memory 的 SessionStore、FactGraph、facts、nodes、embeddings、usage、authorization 与 ToolRun 持久 owner 统一校验各自规范 ID；summary episode 必须属于对应 session，畸形行读取失败关闭（ADR 0967）。定时 ToolRun 执行权 claim 以来源身份统一命名和校验：确认使用 `conf-*`，无需确认时只能复用同一 ToolRun 的 `toolrun-*`（ADR 1016）。

ToolRunStore 的生命周期时间、completion 时间和计时触发 `due_at` 在写入时验证 RFC3339 并归一 UTC，保证 `due_at` 文本排序与时间一致；依赖触发仍以空字符串表示。typed read projection 统一输出 RFC3339，兼容并转换 SQLite 生成的旧 UTC `created_at` / interrupted `finished_at`；无效 waiting `due_at` 保留给 ToolRunService quarantine。schema 不变（ADR 0979）。Agent event UI mapper 的整数字段也按 Rust `u32` 上限或 JSON 可精确表示的 `u64` 安全整数范围校验，浮点用量继续执行有限数值检查（ADR 0980）。

Rust→TypeScript IPC 生成器拒绝把 `i128/u128` 映射为 JavaScript `number`；未来契约需选择十进制字符串或有明确值域的窄整数，不改变当前生成 DTO（ADR 0983）。

Settings 页面在完整快照的初始读取失败或响应无效时不允许提交 UI fallback；`context_limits` fallback 现在由生成输入类型强制覆盖全部字段（ADR 1017）。

`get_settings` 响应 shape 现由 Rust Serialize/Serde 图递归生成，Settings 页面校验必需字段、嵌套类型与闭枚举，同时允许新增未知字段；既有数值域与凭据引用检查继续由领域 parser 执行（ADR 1020）。

Agent、App、Session、ToolRun event mapper 现共用 `isValidTauriEventId`，统一要求 renderer-local envelope id 为非负安全整数；payload 继续按领域边界校验（ADR 0984）。

MCP server profile 名称在 Common 配置加载/写入边界统一校验，并由 UI 编辑器复用同一规则形状；精确大小写身份、无效配置备份行为和 `mcp_add` upsert 语义保持（ADR 0985）。

Settings 的 64 位整数统一限制为 JavaScript 安全整数；Common 校验配置载入、保存和 IPC 更新，UI 对读写 payload 做同界检查，浮点配置按各自声明范围拒绝非有限或越界值；默认上下文窗口的 K-token 控件上限匹配 Rust `u32`（ADR 0986）。

配置读取、保存、ConfigService 编辑与 Settings IPC 复用 Common 的语义校验；前端对整数 Rust 宽度、浮点范围、MCP 非零轮询/重连间隔和录音缓冲上限做同界拒绝。Settings 数值字段再逐项对照生成输入类型，拒绝错误运行时类型和不匹配的 `null`；模型草稿在 UI 与 Rust 边界都要求配置 ID 唯一、服务模型非空且 Provider 有效，避免后端静默过滤（ADR 0988、0999）。

日志复扫移除了 ingress transcript、stream rule 名称与 match、工具/provider 错误、事实谓词和 MCP warning 原文，并从 Skills/MCP 启动日志移除本机路径及可执行命令；日志只保留阶段、类别和字节计数（ADR 0987）。

LLM Provider 的重试、连接探测、adapter/stream 初始化以及 MCP 的连接、工具刷新、shutdown、子进程清理和 SSE 解析日志统一移除外部错误文本，改记稳定错误类别、固定事件描述或字节数；返回错误和状态事件保持原有语义（ADR 0989）。

录音/VAD、Agent、Tools、Messaging 与应用启动/收尾日志继续按来源区分错误处理：外部 Provider/MCP 和不可信内容只记稳定类别、描述、计数或长度；本地错误摘要先清洗。LLM endpoint 日志只记 host，长度单位统一为 UTF-8 bytes；`open_skills_dir` 成功响应不再包含本机路径（ADR 1000）。

Agent 内部步骤计数及 tracing 字段统一为 `step_number`，Rust/UI 的消息身份内部引用统一为 `message_id` / `messageId`；现有事件、数据库及 IPC wire shape 保持（ADR 0990）。

Session history、lineage 与 resume 响应中的 `SessionRecordDto` 统一以 `session_id` 表示会话身份；UI parser 和消费者拒绝旧泛名 `id`，数据库主键列 `sessions.id` 不变（ADR 0991）。

App `MemoryFactResponse` 以 `fact_id` 表示事实身份；UI command parser 不接收旧 `id`，数据库物理列 `facts.id` 与 Memory repository `Fact` 保持 owner 内部形状（ADR 0992）。

Memory resume projection `SessionStep` 以 `step_id` 表示执行行身份；UI parser 与 transcript identity join 拒绝旧 `id`，保留 supplement 行复用 `msg-*` 的既有规则，数据库物理列 `session_steps.id` 与 durable event payload 不变（ADR 0993）。

App-owned `SessionResumeMessageDto` 使用 `message_id` 显式投影 Memory `Message.id`；UI 只接受该字段并在 renderer 边界映射为气泡 `id`。消息行可使用 `msg-*` 或关联步骤的 `step-*`，数据库列 `messages.id` 保持不变（ADR 0997）。

Agent runtime list 使用 `SessionInfo.session_id` / `StepInfo.step_id`，UI `SessionSummary` 使用 `sessionId`；parser 与消费者拒绝旧泛名 `id`，App lifecycle event projection 保持 `session_id`（ADR 0994）。

持久用量记录及 resume DTO 使用 `LlmUsageRecord.usage_id`；UI parser 拒绝旧泛名，Memory 只为历史 durable event payload 保留反序列化 alias，新事件只写 `usage_id`，数据库列 `llm_usage.id` 不变（ADR 0995）。交互请求统一使用 `request_id` / `requestId`，Ask 仍复用 `step-*`，确认仍使用 `conf-*`；清除事件改写 `request_ids`，durable replay 兼容历史 `id` / `ids`，UI 新契约拒绝旧 `id`（ADR 0996）。后续实体身份审计转向 CompactSummary durable payload、跨 crate prompt DTO、其余 command DTO、配置字段、Rust crate 可达 API 与当前文档引用。

Compaction、memory embedding maintenance、App 启动/恢复、clipboard history 和 managed-media cleanup 的错误日志现统一清洗；上传批次与生成媒体目录项日志移除 basename，避免暴露用户文件名（ADR 0981）。

Agent compaction 与 media-plan toast 现由 `+layout` 统一持有；chat controller 保留 media-plan session 状态投影，chat usage handler 不再写通知 store（ADR 0982）。

`process_transcript` 的 command invocation 与 response parser 由 `sessionCommands` 持有，submit coordinator 只消费已验证结果；Rust 和 UI 派生集合按实体复数命名，context-menu store 文件明确标注 `Store` 角色（ADR 0968–0969）。

Session、ToolRun、transcript supplement 与 lineage command response 现在将响应实体绑定到请求 owner；lineage DTO 显式携带父子关系锚点，resume 校验 `msg-*` supplement step 与全部嵌套 managed-asset 引用（ADR 0970）。

#### 执行顺序与完成条件

Active 子域继续 Rust crate 边界审计：Messaging 与 MCP 的跨 crate 实现边界已收口；Memory 默认生产构建隐藏 `Database` 和 store 数据库构造器，App 通过 `MemoryPersistence` 获取 typed stores。Agent 的 `AgentToolRunPort` 映射 Tools 的 session capability；App 的 `AppToolRunPort` 映射 Tools 的管理 capability；completion receiver 与 outbox/scheduled recovery 留在 Tools owner 内，Agent 跨 crate 测试注入只使用 `test-support` port（ADR 0899）。Tools 的 registry getter 和任意 session-tool 注册入口已关闭；Agent/App 测试通过独立 `ToolCatalogTestSupportPort`，生产调用方只取得已发布的 catalog snapshot（ADR 0900）。MCP 配置改为只读 `ConfigService` 的当前快照，工具加载、目录、管理和 App 状态查询不再维护第二份可变 profile map（ADR 0901）；MCP/Skills owner 与 AdminOperationSet 保持 Tools 内部，App 分别通过窄 runtime/catalog/operation ports 调用（ADR 0903–0904）。Tools 的实现模块默认不可达，熔断器重置经 `ToolsFacade`，测试 fixture 入口由 `test-support` 控制（ADR 0905）；App 生产仅消费资产 lifecycle port，managed asset registry 具体类型不进入默认 crate-root API（ADR 0940）；shell runtime helpers 也只在 Tools crate 内可见（ADR 0942）；Skills catalog port 不再把完整 `Skill` runtime 对象交给 App；执行 capability 接收名称并在 Tools owner 内查找和执行，测试 fixture lookup 留在 `test-support`；Tools 移除无消费者 `Language` alias，MCP adapter 的 `McpToolInfo` 也只在 `test-support` 暴露，`SkillInfo` 保留为 catalog port contract（ADR 0943）。Memory repository 跨 crate 路径已核对：外部只使用 Memory 根导出的 domain stores/DTO，没有直接进入 repository、数据库或 schema 模块；raw `EmbeddedText` 已从默认生产 API 收回（ADR 0906）；outbox/session recovery 的页容量已回收到 typed store，Agent scanner 只传 cursor 与 high-water；无外部消费者的 policy helper/阈值及 `ProjectionCutoff` 已收回，raw `PartialMessageCheckpoint` 限制在 `test-support`（ADR 0907–0908）；SessionStore 普通生产 SQLite 操作通过 async/cancellable typed 边界进入 blocking pool，仅保留同步退出暂停例外（ADR 0909、0913）；outbox store 使用职责名，且 `MemoryStores` 改用私有字段和具名 clone accessor（ADR 0910）。Memory 的 ToolRun 底层持久化文件现命名为 `tool_runs.rs`，覆盖 background/scheduled lifecycle 与行读取；typed async façade 和 completion delivery outbox 保持独立（ADR 0914）。`SessionCommitted` 对 Agent 仅提供 typed commit builder 和批量合并入口，投影向量与实现 DTO 保持 Memory 私有（ADR 0915）。Agent interaction 的 crate 根导出为唯一对外路径，Agent/App 内部统一经根导出引用，具体实现模块留在 crate 内（ADR 0916）。Agent 根 API 与生产 run 入口已完成本轮收口，Memory 类型使用其唯一 owner 路径（ADR 0917、0939）。ToolRun history 的 Memory 行模型现只在 Tools 内投影，App management capability 只暴露 safe `ToolRunView`；history status filter 与 limit 在 Memory query 执行，App 不再做全量读取后分页（ADR 0920）。Skills registry 现在也在 owner 内比较 watcher fingerprint 并执行按变化刷新；Tools watcher 仅控制轮询、catalog rebuild 与通知，路径/时间戳 DTO 不再穿过 crate 边界（ADR 0927）。Common 的 lease 与终态交付 contracts 改为唯一根路径，单类型模块私有，Memory/Tools 不再直接依赖 `doc(hidden)` 模块（ADR 0928）；用量 contract 则统一留在 Common 的 `usage` 命名子域，移除未使用根 re-export（ADR 0929）；Common 配置 API 统一经 `config` 命名子域（ADR 0930）；工具 contracts 统一经 `tools` 子域，Common root 不再保留未消费 aliases（ADR 0931）；media 与 media_detection contracts 则统一通过对应领域子域访问，不再由 Common root 提供第二入口（ADR 0932）；session、step 与 ToolRun lifecycle status 统一由 `lifecycle` 子域提供唯一入口（ADR 0933）；workspace root 探测继续只从 Common root 暴露，源实现模块收为私有（ADR 0934）；MCP server transport 的类型和命名统一为 `mcp::McpServerTransport`，并同步 IPC/UI contract（ADR 0935）；运行时 confirmation/recording ID 通过 type-specific generator 生成，TryFrom 与 serde 校验固定前缀，外部不能直接调用 tuple 构造器（ADR 0936）。Rust 根 API 审查继续逐 crate 核对未覆盖的构造入口与直接调用；UI event、Session/Memory command response 与 transcript submit result 的 entity identity 边界已有 ADR 0945–0948，后续仍需覆盖其他 command result、持久化/replay 写路径、配置字段和当前文档引用。只有证据显示职责重叠或 owner 不清才实现合并/改名/封装切片；不以行数、文件数或 crate 数为完成标准。

本轮 Memory 调用图确认 MemoryTool recall slot 在生产目录重建前由 Tools startup 绑定；原 keyword fallback 只会在未绑定时绕过该 capability，现删除重复 `MemoryFactStore::recall_keyword` 与 fallback 路径（ADR 0918）。事实批写 DTO 曾将 Memory 单值谓词判断作为 Agent 布尔值传回 Memory；现由 FactGraph 内按规范谓词计算，保持当前行为并关闭调用方覆盖策略的入口（ADR 0919）。

ToolRun history 调用图确认 App management capability 原先把 Memory `ToolRunRow` 原样暴露给 App，history 在装载全部记录后才由 App 过滤和分页。现由 Tools owner 用同一 `ToolRunView` 投影 live 与 persisted board/history 数据；Memory 终态查询可在 SQLite 执行过滤与限页，App 仅组装 IPC 结果（ADR 0920）。

MemoryTool fact read 调用图确认 search/list 原由 Tools 获取宽结果后截断，其中 search 可将未限页 FTS 候选跨 crate 返回；全局 list 的 `recent` 名称也与有效置信度排序不符。现由 `MemoryFactStore` 的显式限量 search/list capability 在敏感过滤后截断，Tools 不再重复归一查询或接收宽结果；数据库读排序和结果保持不变（ADR 0921）。

Tools capability 调用图确认 AppRuntime 已取得 `ToolServices` 后又把 `ToolsFacade` 传给三个 adapter factory 重新读取整包；Agent prompt adapter 还在每次 catalog 查询时读取整包。现 adapter 构造器只接收自身所需的窄 port，App background bootstrap 复用 `AppServices.skills`，`share_services()` 保留为 composition handoff（ADR 0922）。

Messaging transport 复核确认 `InboxBus` 已是 `haven-messaging` 的私有实现，生产消费者只使用 `MessagingService`；`impl MessageTransport for InboxBus` 中同名方法转发是私有 transport adapter，不是跨层绕过 facade。该 trait 同时承载隔离测试 transport 注入，因此当前保留；继续按此区分审查其他模块的真实 consumer 直连、组合根 wiring 与必要 adapter 转发。App composition 另有明确的模块归属问题：录音生命周期锁、`rec-*` identity owner 和并发测试从 `app_state.rs` 移到专属 `recording_lifecycle.rs`，AppState 仍负责唯一实例的装配与共享（ADR 0923）。

Common shared contract 路径复核发现 Tools 根额外 re-export 了 Common 所有的 `CapabilityScope`，仓内只有测试使用该第二入口。现移除 Tools alias，统一通过 `haven_common::types::CapabilityScope` 命名；Tools 公开策略 DTO 仍引用同一 Common 类型（ADR 0937）。

进一步核对 Common transcript API 时发现九个类型/常量同时由 `types` 模块和 Common 根导出，而所有仓内消费者均使用定义模块。现删除根 aliases，统一通过 `haven_common::types` 引用；transcript 语义与 wire contract 未改变（ADR 0938）。

`CapabilityScope` 构造边界现只公开 fallible `try_new`；移除 `new` 与绕过校验的 `From`，层级候选只从已验证 scope 内部派生。授权 grant 与外部 Tool policy builder 只接收 typed capability；永久 grant 撤销也先解析并在无效输入时保持规则不变。运行时策略 resolver 与授权端口表达策略构造错误，Agent 在调用授权引擎和执行工具前失败关闭；定时调用与确认恢复同样适用。模型工具参数先经过 Agent catalog schema admission，但 resolver 仍独立验证 capability 边界（ADR 0944、0978）。

进一步的 composition 调用图发现 AppState、Agent adapter factory 与 ApplicationRuntime 曾分别调用 `share_services()`。现由 AppState 在唯一 App composition root 取得一次 `ToolServices`，将 Agent 所需项作为显式构造参数传给 adapter，并把同一 bundle 交给 ApplicationRuntime 适配 App services；运行时和 adapter factory 不再另行读取 facade bundle（ADR 0924）。

LLM 导入路径审查发现 router、client、types、media、registry、STT 与 adapter 查询同时经 crate root re-export 和 `pub mod` 实现路径可达，Agent/Tools/App 生产代码与测试混用两套拼法。现将稳定 consumer DTO/操作集中从 LLM crate root 导出，隐藏实现模块并统一仓内调用路径；provider adapter 细节只由 LLM 内部寻址（ADR 0925）。

Input API 审查发现 capture engine、VAD detector 与 hotkey parser 实现模块对下游公开，消费者同时通过 crate root 和实现模块路径依赖类型；modifier 以无类型 `u8` bitmask 暴露，且 `KeyCombo::new` 可构造无效组合。现将 capture/VAD/hotkey 模块设为私有，仅根导出真实跨 crate contract；modifier 改为 `KeyModifier`，按键组合只由解析器构造。保留有效 idle pipeline 的 `InputPipeline::default`，删除会产生空 Manual 结果的 `RecordingResult::default`（ADR 0926）。

UI 事件与 ToolResult contract 复核发现动态工具输出里的资产身份校验不一致：envelope collector、observation mapper 与 Window/Media 专用 renderer 曾只要求字符串。现三处统一要求 canonical `asset-*`；专用 renderer 遇到畸形字段退回 JSON，开放的 MCP/Skill 输出 contract 不变。录音 lifecycle mapper 同步按 generated reason enum 和必需字段失败关闭（ADR 0962）。

每个候选都要说明真实消费者、权威状态 owner、作用域与生命周期、失败/恢复语义，以及是否改变 IPC、持久化或安全契约。结论必须是合并、改名、拆分、保留并解释，或有证据的暂缓。全仓通过条件是主要生产概念有唯一规范词和可定位 owner；重复职责已合并，或相邻 owner 的区别能从代码和文档解释；当前源码、契约与路线图不再使用退役名称描述现状。历史决定仍可在 ADR 中按原名检索。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
