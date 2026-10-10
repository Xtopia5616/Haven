# Haven 架构降复杂度重构路线图

> 状态：阶段 0–8 已完成；唯一结构 Active 为 §5.7 全项目领域术语与架构角色命名收敛，当前从 Rust crate 内部符号与函数调用图开始；暂无 Next。Windows 发布验收为独立 Open Gate（§5.1）。
> 更新日期：2026-10-10
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
| **Active / Next** | **Active：全项目领域术语与架构角色命名收敛**，见 §5.7；当前先审查 Rust crate 内部符号与函数调用图。近期 Chat 输入 owner 与提交边界已收敛（ADR 0843–0846），全仓 Rust、Svelte/TypeScript、IPC、配置和文档审查仍未完成。暂无后续 Next。 |
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
- **工具运行时三个集合各自保留（已复核，暂不合并）：** `ToolRegistry` 是已安装工具的权威注册表，保留注册顺序、拒绝重复名并维护 global version；`DeferredToolCatalog` 保存尚未激活、供发现和按需加载使用的定义；`SessionToolOverlay` 是某个 session 当前可执行的附加工具集合，loader batch 负责幂等、预算准入与 session version。三者分别表达注册、延迟发现和 session 执行作用域，不能因 lookup/list/definition projection 外观相近而统一成一个通用 Catalog。Provider 定义、校验、manifest 与执行的共享 turn-level immutable view 已由 `ToolCatalogSnapshot` 承担；共同规则若真实重复并导致漂移，再评估窄 helper。ADR 0145/0148 的 provider visibility 与 session 隔离不变（本轮名称对齐见 [ADR 0533](adr/0533-tool-runtime-nomenclature-alignment.md)）。
- **MCP prompt index 边界保留：** `McpServerIndexEntry` 是固定的 Tools→App→Agent prompt 摘要；tool schema/result 保持动态 JSON。仅在摘要新增稳定字段或出现独立消费者时重开（ADR 0516）。
- **Admin 风险 metadata 保留单一来源：** 共用操作由 `OperationContract` 拥有风险级别，parity 门禁覆盖共享 operation 集合；MCP refresh/reconnect 等 native-only 操作保持独立测试。operation surface 或风险策略变化时重开（ADR 0512、0528）。
- **Windows 子进程 containment owner 保留：** 受管进程经 `ProcessContainment` 创建、分配 Job 并恢复初始线程；命令策略、管道和取消等待仍归各 adapter。新增入口绕过该 owner 或后代残留时重开（ADR 0513）。
- **Agent 与 Memory owner 保留：** SessionActor 独占会话运行态，MemoryRuntime/Worker/Outbox 按生命周期与数据责任协作；队列顺序、取消边界和公平调度未形成可证明的重复 owner。出现跨 session 状态泄漏、恢复/取消回归或同一策略漂移时重开（ADR 0214、0424、0468、0475–0476、0481）。
- **MemoryWorker / MemoryOutbox 边界保留：** durable store、scanner、retry/ack 与 outbox lifecycle 由 `MemoryOutbox` 拥有；MemoryWorker 负责组合、推理和 prefetch。只有生命周期职责再次耦合或出现重复容量/取消状态时，才按 §5.5 复核（ADR 0518）。
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

**当前执行位置：** 阶段 0–8 已完成；唯一结构 Active 为 §5.7 的全项目术语与架构角色审查，当前先审 Rust crate 内部符号与函数调用图。§5.3 记录仍满足重开条件的 Deferred 候选；Windows 发布验收是独立 Open Gate（§5.1）。

### 5.7 全项目领域术语与架构角色命名收敛（Active）

范围覆盖所有 Rust crate、Svelte/TypeScript、Tauri IPC/事件，以及对应架构、命名和契约文档。职责以 `docs/architecture.md` 为准，术语与角色动词以 `docs/naming.md` 为准；已完成决定和实现细节只保存在对应 ADR，不在路线图重复列实现日志。

最近完成的 Chat 输入审查已统一 submission payload 与生成的 `MessageAttachmentInput` 字段 owner；本地文件预览大小在 Composer 提交边界移除；`Composer.svelte` 成为单一输入组件 owner，route callback 直接连接 AskInteraction 与 ChatSessionController（ADR 0843–0846）。录音身份与会话身份已在 Rust、IPC、UI 和文档中分开命名（ADR 0847）。Provider 工具参数 wire 转换从 Common 归回 LLM adapter 私有边界，capability ancestry 由唯一 typed owner `CapabilityScope` 直接提供（ADR 0848–0849）；provider-neutral 结束原因解析移除 OpenAI 专属命名（ADR 0850）；Usage 原始字段与有效值 accessor 采用不同名称（ADR 0851）；缓存用量契约及其策略/结果/来源枚举归入 Common，并收口 Agent、SQLite、IPC 与 UI 的重复动态投影（ADR 0852）；工具媒体能力解析合并到唯一 resolver owner，区分后端可用性与最终工具能力视图（ADR 0853）；能力快照到 `RuntimeCapabilities` 的纯映射使用 `project_*` 动词，与 façade 的 fresh read 区分（ADR 0854）；LLM/MCP transport 共享 Common 中纯字节累积上限不变量，但分别保留取消、deadline 与协议错误 owner（ADR 0855）；Agent 的文本 tokenizer、request token estimate 与方向明确的前缀/后缀预算截断归入唯一 `token_budget` owner，React、prompt builder 和 compactor 不再以 compactor 模块作为 token accounting 入口（ADR 0856）；各消息边界复用 Common 的规范 `{prefix}-{uuid32}` 校验，删除 Tools 与 Messaging 的重复 `msg-` validator（ADR 0857）；App 与 Tools 的 Windows 文件元数据 symlink/reparse 检查由 Platform 共享，调用者继续控制路径遍历和根目录策略（ADR 0858）；Agent 与 Tools 的重复工具 root 派生归入 Common 的 `ToolDef::root_name`（ADR 0859）。前端闭合枚举成员判断和字符串、布尔、有限数、字符串数组谓词归入唯一 `valueGuards`，wire 字段读取归入 `wireGuards`；Agent/App/Session/ToolRun、Memory、录音、工具结果 JSON、manifest 与 reducer 共用原子判断，领域必填/缺省/null 策略仍由 mapper 拥有（ADR 0860）。OpenAI Chat/Responses 的 prompt cache key 状态机、拒绝分类与 LLM adapter 当前 Unix 秒读取归入共享 owner，key wire 生成仍分属协议 adapter（ADR 0861）；Tools 授权确认 hash 与 LLM schema/cache identity 共用 Common 的 canonical JSON bytes，业务哈希及 provider/cache key 语义保留在原 owner（ADR 0862）；App managed media 与 Tools asset registry 共用 Common 的 lexical path comparison，同时保留各自的文件遍历、metadata 安全检查及 root policy（ADR 0863）；App 与 Tools 直接使用 Common 的 XML entity unescape，不再经 App 同名转发 helper（ADR 0864）；Tools Skill 与 catalog 名称列表共用有序非空去重原语，各领域 trim/前缀/展平规则仍由自己的入口拥有（ADR 0865）；事实谓词规范化统一归 Memory facts repository，Agent 不再保留零行为转发入口（ADR 0866）；App 命令、通知、事件与 logging 直接使用 Common 错误文本 sanitizer，App logging 只保留命令日志职责（ADR 0867）；Common 错误摘要与 Agent prompt 前缀截取使用不同截断名称，事实 prompt 固定字符预算以领域命名（ADR 0868）；SessionStore 的 title-only 与 title-or-input 查询分别命名为 `get_session_title` / `get_session_title_or_input`，App 通知显示解析器继续独立管理 display fallback（ADR 0869）；Tools file read/summary/outline 共用 `file_line_reader::read_line_bounded`，统一受限缓冲与超长行尾部消费（ADR 0870）；后台 live completion、scheduled completion 和 crash reconciliation 共用 Common `ToolRunCompletionPayload`，Tools status UI projection 保留独立 owner（ADR 0871）。这些局部结果不代表 UI 或全仓审计完成。

#### 当前审计覆盖与剩余范围

| 范围                                                      | 已核对的当前基线                                                                                                                                                                                                                                                                 | 尚待核对                                                                                                                                                            |
| --------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust 类型与架构角色                                       | crate 公共类型做过跨 crate 重名初扫；主要 `Engine`、`Runtime`、`Store`、`Registry`、`Manager`、`Service`、`Facade` owner 词汇及一批领域类型、公开 API 已按 ADR 收敛（ADR 0533、0554、0615–0676）。条件编译下同名平台 handle 表示同一抽象，保留。                                 | 为 `pub(crate)`、私有类型、字段、模块、trait 方法建立逐 crate 清单；复核每个实例的状态 owner、生命周期和调用者。                                                    |
| Rust 调用与函数动词                                       | Input、Tools、Memory、Agent、LLM 和 App 已有多轮按返回形状、副作用、资源 owner 与错误语义收敛的切片（ADR 0570–0676）；OpenAI Chat 与 Responses adapter 的 prompt-cache-key 支持状态与错误分类现归共享私有 owner（ADR 0861）；Tools 授权 hash 与 LLM schema/cache identity 共用 Common canonical JSON bytes（ADR 0862）；App media cleanup 与 Tools asset registry 共用 lexical path predicate（ADR 0863）；App/Tools XML entity decoding 共用 Common helper（ADR 0864）；Tools builtin 的相同有序列表去重循环归入私有 helper（ADR 0865）；事实谓词规则由 Memory 独占，Agent extraction 和 maintenance 删除转发 alias（ADR 0866）；App 错误文本清洗只由 Common 实现，logging.rs 保留命令日志 owner（ADR 0867）；Common append-ellipsis 与 Agent prefix-only 字符截断采用不同名字（ADR 0868）；SessionStore 的 title-only 与 title-or-input 查询分别命名为 `get_session_title` / `get_session_title_or_input`，App 通知显示解析器继续独立管理 display fallback（ADR 0869）；Tools file read/summary/outline 共用 `file_line_reader::read_line_bounded`，统一受限缓冲与超长行尾部消费（ADR 0870）；具体实现历史见 ADR。                                                   | 逐 crate 审核私有/public(crate) 函数、参数/返回值、trait 方法及调用链；只在职责或权威状态重复时合并，按实际可观察行为统一动词。                                     |
| UI components、stores、controllers、handlers 与 contracts | 已完成部分 controller 生命周期分界、生成类型复用、无消费者 alias 清理、Tool result builtin nested-shape 校验和 Session 状态投影审查（ADR 0595、0665–0670、0698、0745、0841–0846）。Tool result JSON 的 builtin 输出按 family 校验；MCP/Skill renderer 保持明确的扩展型动态边界。UI 原子值与 wire-field guard 已归并（ADR 0860），不取代各领域 mapper 的字段策略。 | 逐组件、store、controller、handler 检查数据/状态 owner 与调用图；完成 props、事件及 contracts 中非 alias 类型的 owner/命名审计，并检查 route 局部副本和无行为转发。另需核对 UI runtime mapper 对持久实体 ID 是否只做非空检查、是否需要执行规范前缀/UUID32 校验；provider 外部 ID 与进程内 ID 按各自规范区分。 |
| Tauri IPC 与事件                                          | 命令静态 request/response 有 Rust DTO 生成来源，事件 channel 有 Rust 登记和 UI mapper；已按领域收敛若干闭合枚举、目标命名和事件字段（ADR 0613、0699、0730–0745）。                                                                                                               | 对每项命令与事件核对业务名词、snake_case/camelCase 边界、payload owner、顺序/幂等、失败语义、敏感字段和运行时校验；结构脚本通过不能替代语义审查。                   |
| 架构、配置、持久字段与文档引用                            | 已核对部分配置 owner、schema 拒绝规则、Serde alias、Provider 标识及多项架构文档交叉引用（ADR 0632–0633）；外部 provider 协议拼法与 Haven 自有历史数据名已区分。                                                                                                                  | 逐项对照代码、生成 contract、配置/SQLite 字段、用户文案与文档；清理当前文档里的退役符号，确认只在 ADR 历史或外部协议边界保留旧拼法。                                |

#### 执行顺序与完成条件

当前 Active 子域：Rust crate 内部符号与函数调用图，先从类型/字段/module/trait method/参数和返回值的实际消费者与生命周期建立清单。之后依次复核 UI owner 与本地契约、IPC/event 语义、配置/持久字段/文档引用；发现独立可回退的重复职责就按逻辑切片实现，不按行数或名称后缀机械拆分。

每个候选都要能说明真实消费者、权威状态 owner、作用域与生命周期、失败/恢复语义，以及是否改变 IPC、持久化或安全契约。结论必须是合并、改名、拆分、保留并解释，或有证据的暂缓。全仓通过条件：主要生产概念有唯一规范词和可定位 owner；重复职责已合并，或相邻 owner 的区别能从代码和文档解释；当前源码、契约与路线图不再使用已退役名义描述当前状态。历史决定仍可在 ADR 中按原名检索。

## 6. 更新规则

- 阶段/候选状态在一个逻辑切片独立提交且适用验收完成后更新；受限或未执行的验证要明确写出。发布 Gate 只在当前构建与安装环境的实际验收记录齐全后关闭。
- 设计背景、替代方案、实现范围和详细测量结果写入对应 ADR；本路线图保留链接与当前结论，不复制变更日记。
- 影响数据库、配置、IPC 或用户可见安全行为时，同步更新相应契约文档与发布/重置说明。
- 每次完成一个结构切片、准备进入下一个 Candidate、或准备发版签核时，复核候选触发证据；证据已消失就关闭为“无需拆分/优化”，不让 backlog 无限累积。
- 路线图只保留一个 Next/Active 结构目标。独立发布 Gate 可并行准备；不影响发布路径的候选整理不因 Gate 开放而被阻塞。
