# 第二轮逐文件审计：兼容层与不必要设计

> 日期：2026-10-17
> 类型：只读源码审计与下一轮重构方案；本轮没有修改运行时代码，也没有运行测试。
> 目标：在接受破坏性调整、无需兼容 Haven 旧版本的前提下，移除无消费者的兼容路径与空转抽象，收敛当前实现的真实 owner。

## 1. 审计口径

本方案以生产 writer、reader、调用点和当前契约为证据，不以旧 ADR、注释或上一轮审计结论代替源码。把下列情况分开判断：

1. **Haven 自有旧格式兼容**：只为旧 Haven 数据、IPC 名称或内部序列化形状保留的 alias、fallback、normalize 和迁移代码。在本项目当前测试阶段不再支持，按计划删除；持久化格式改变时集中提升 schema 版本并按 `docs/release-and-reset.md` 重建数据库。
2. **外部协议互通**：LLM provider wire、MCP 字段、用户文件编码等差异。只有确定是过时版本 fallback 才列入删除；当前 provider 或用户输入实际需要的多形状处理仍保留。
3. **当前语义与数据保护**：可选字段、部分 Settings patch、安全校验、损坏配置备份只读恢复、事务/发布顺序等不能因为带有 `default`、`fallback` 或 `legacy` 字样就删除。要从生产调用证明它们多余。

不以改动风险作为暂缓条件。风险大的工作拆成有明确边界的切片，并一次性说明数据库/收件箱重置影响；计划中的测试是未来实施门禁，本轮不执行。

覆盖范围包括 Rust workspace 的 Serde alias/default、legacy/fallback/version 命中点及关键生产读写；App IPC 注册、renderer caller 与 checker；UI tool result parsing/presentation、resume 和 store 写入方。结论只把仓内可见生产消费者作为兼容证据；本项目明确不要求兼容外部旧 binary 或 workspace 之外未登记的私有调用方。

## 2. 当前组件职责基线

| 组件 | 当前权威职责 | 本轮重构边界 |
|---|---|---|
| Common | ID、配置 schema 与验证、跨 crate typed contracts | 删除 Haven 内部旧字段读取；保留当前配置默认策略和真正的共享 contract。 |
| Memory | SQLite schema、SessionEvent append/replay、transcript projection 与 rollback | 删除单版本但无解码器的 event version；统一当前 writer 时间格式；不把 DB/projector 暴露给 Agent。 |
| Agent | ReAct/session 生命周期、durable event 生产、ToolRun 结果交付 | 结果补充由当前 producer 格式解析；不把格式失败转成伪成功。 |
| App | Tauri composition root、command/event DTO 与 IPC 绑定 | 删除无 renderer owner 的 command；去掉无转换能力的转发 adapter；消除手工 metadata 副本漂移。 |
| MCP | 连接、外部协议 transport 与 server/tool 注册 | 本方案按当前现代握手收敛；旧协议握手/transport fallback 删除，但不动当前 `Legacy` tool exposure 策略。 |
| LLM | provider 请求/响应映射 | 继续适配当前 provider wire 变化；不把供应商互通误判为 Haven 数据兼容。 |
| UI | IPC 校验、业务身份/状态校验、session 展示投影 | 删除已无生产写入端的专用 alias；保留每层职责不同的 shape/semantic/renderer validation。 |

没有证据支持合并 workspace crate。收益集中在删除多余兼容分支、统一当前权威和少数私有状态/模块，不新增 service container 或跨层 facade。

## 3. 逐文件审计结果

### 3.1 当前生产缺陷

| 文件 / 符号 | 当前路径证据 | 结论与目标 |
|---|---|---|
| `crates/agent/src/layer/tool_run_result_delivery.rs::format_tool_run_result_message`、`crates/agent/src/layer/context.rs`、`crates/agent/src/react/transcript.rs` | Agent 当前写出带 `<untrusted_tool_run_result>` 包裹的 JSON supplement，并在提交后按 supplement 发布。`transcript.rs` 与 Agent lifecycle 测试仍构造早期 `[Background tool run result]` 文本。 | producer 的当前格式是唯一格式；移除旧文本夹具和旧格式读取。确保 durable transcript 保留该 untrusted envelope。 |
| `ui/src/lib/streaming.ts::parseToolRunResultInject`、`ui/src/lib/sessionReducer/agent.ts::applySupplement` | UI parser 只识别旧文本头；当前 producer 输入解析为 `null`。reducer 随后合成 `tool_run_id = "unknown"`、`status = "completed"` 的结果卡。 | 直接解析 producer 当前 JSON envelope；校验规范 `toolrun-*`、`kind`、状态和所需字段。解析失败时保留普通 supplement，不得生成 unknown/completed 卡；按 supplement ID 保持重复事件幂等。 |

这是 P0 行为修复：现有 UI 对当前后台任务结果可能显示错误身份和假成功状态，应先于纯结构清理实施。

### 3.2 可直接删除的 Haven 旧格式兼容与死分支

| 文件 / 符号 | 源码证据 | 处置 |
|---|---|---|
| `crates/agent/src/interaction.rs` | writer 使用 `request_id` / `request_ids`；Serde alias `id` 与 replay 的 `ids` fallback 只由旧格式测试覆盖。 | 删除旧 key 读取和 legacy-only 测试；保留当前 `Option` / `skip_serializing_if` 组合，因为 writer 会省略当前可选字段。 |
| `crates/common/src/types.rs`、`crates/agent/src/types.rs` | `CanonicalMessage.message_id`、`CanonicalToolCall.tool_call_id` 的 `id` alias 只服务 durable payload 旧形状；现有 writer 已写完整字段名。 | 删除 alias 与旧 JSON fixture。不要删除有当前省略语义的 `default`。 |
| `crates/memory/src/repositories/usage.rs`、`crates/memory/src/repositories/session_events.rs` | `LlmUsageRecord` 的 `id` / `role` aliases 只服务旧 durable event；当前 writer 写 `usage_id` / `request_kind`。`llm_usage.role` 是仍在用的 SQLite 物理列名，不属于 JSON alias。 | 删除 JSON aliases 与 legacy fixture；不顺带重命名 SQL 列。 |
| `crates/memory/src/repositories/usage.rs::SessionUsage` | 由 SQLite 查询构造后直接作为 IPC response 序列化给 UI；无 JSON 反序列化消费者，TS response 字段均 required。Serde `Deserialize` / `Default` 和新计数列 defaults 只放宽 response DTO 的旧 shape。 | 保留 `Serialize`，删除 `Deserialize`、`Default` 和旧 response defaults。不要改 `LlmUsageRecord` 的持久 event owner。 |
| `crates/common/src/tools.rs::deserialize_legacy_operation_idempotency` | 宽松 parser 把未知 enum 字符串吞成 `Unknown`；UI 当前 mapper 对 enum 严格校验。 | 删除自定义降级读取，直接使用严格 enum Serde；未知值在边界报错。 |
| `crates/common/src/tools.rs::ToolResultEnvelope` | `Default` 仅被测试使用且生成假的 `Failed` outcome；`assets` writer 总会序列化（含空数组），其 `serde(default)` 无当前省略形态。`sandbox_output_review_step_ids` 在空时会省略，需保留其 default。`retry_safety` 的字段值实际是 `OperationIdempotency`。 | 删除误导性 `Default`，测试显式构造。把 Rust/UI/IPC 字段统一改为 `idempotency`；删除 `assets` 的无用 default，保留空值可省略字段的 default。Envelope 本身承载 transcript UI 需要、不可进入模型 observation 的资产和 sandbox review 信息，保留。 |
| `crates/agent/src/react/tool_batch.rs` | 主批处理路径先传 `Unknown`，之后通过 `with_default_retry_safety` 改成已知 policy；helper 没有必要作为两阶段构造 API。workflow/codemode 当前没有同一具体 policy 来源，`Unknown` 仍是其语义。 | 主批处理直接传真实 idempotency 并删掉 setter；workflow/codemode 保留 Unknown 的显式语义，不用兼容性名义误删。 |
| `crates/common/src/tool_run_completion.rs`、`crates/tools/src/tool_run_service/views.rs` | `status_projection_kind` 被序列化为 legacy JSON key `kind`，生产只从 view 写入 `background`；仓内没有 payload reader。完成类型已由 outbox row 的 `kind` 和 `ToolRunCompletion` enum 表达。 | 删除冗余字段、旧 JSON rename 和赋值/struct literals；相同完成记录不再保存重复的 UI 投影标签。新增 superseding ADR 记录删除原因；ADR 0871 保持历史原文。 |
| `ui/src/lib/toolResultPresentation.ts`、`ToolRunsResult.svelte`、`ToolScheduleResult.svelte` | 当前 Rust `tool_runs` 只发 `list/inspect/cancel`，scheduled tool 只发 `set/list/cancel`；`tool_runs_*` 和 `schedule_*` alias 没有当前 producer。`tool_runs_result_injected` 是 UI reducer 的合成名，不是 Rust wire operation。 | operation union 收敛到 canonical 值，删除 normalize 函数；reducer 合成 `result_injected`，组件按联合类型直接分支。 |
| `ui/src/lib/ToolResultCard.svelte` | Skill/MCP 注册名经 `llm_tool_name` 规范化为 `skill__…` / `mcp__…`；UI ToolCall/Observation 使用该注册名。旧 `skill::` / `mcp::` renderer 分支没有当前生产 writer。 | 删除旧前缀 renderer 分支和旧名测试；内部授权/注册前的 `skill::` / `mcp::` 仍是 Tools 私有 canonical key，保留。 |
| `ui/src/lib/toolResultValidation.ts` | 内建文件 renderer 的当前 tool name 是 `files.search`；`file_search` alias 无生产传入。 | 删除 `file_search` case；错误或旧名称不能进入 Files 专属 renderer。 |
| `ui/src/lib/resumeMessages.ts` | Ask tool 当前 schema 与 producer 都写 `string[]`；恢复路径把旧 `{answer: ...}` 项转成字符串。 | 只接收字符串数组；非字符串项不显示，不转成 `[object Object]`。 |
| `crates/agent/src/fact_extraction.rs`、`fact_inference.rs` | `coerce_to_string` / `coerce_string_array` 会把模型错型的 number/bool/object/array/null 转成事实文本或空字符串，导致坏模型输出被当成有效数据。该逻辑是 model-output 宽容，不是旧数据兼容。 | 独立切片改为严格结构输出；拒绝或跳过无效项并给出受清洗诊断，保留 prompt-injection 清洗和实体校验。不要把错误类型静默转换成事实。 |
| `crates/memory/src/schema.rs::validate_current_schema` | 旧 `memory_episodes` 表拒绝分支位于 `user_version == SCHEMA_VERSION` 校验之后；旧 schema 在此前已拒绝。该特殊检查只对被错误标记为当前版的残留表触发。 | 删除 `memory_episodes` 特例和只测试此特例的用例；保留当前表/列完整性检查。 |
| `crates/common/src/config/endpoint.rs::RoutedModel`、`RouterConfig` | 是 `AppConfig` materialize 后供 `LlmRouter` 使用的 runtime snapshot；仓内生产代码只构造/读取，没有 TOML/IPC/wire Serde 消费者。`RoutedModel.model_config_id -> id` rename 只有测试固定。`RouterConfig::default()` 则是现行无模型 runtime baseline，多个 production/test caller 正在使用。 | 删除 runtime-only DTO 的无消费者 Serde derives、rename 与虚构 wire contract 测试，恢复内部 `model_config_id` 命名；保留 `RouterConfig::default()` 和实际使用的运行时 `Default`，不要改持久化及 IPC 使用的 `ModelConfig.id`。 |
| `crates/common/src/tools.rs::ToolManifest`、`ToolPresentation` | Tools 单向生成 manifest 给 App/UI；TS 生成类型字段 required，仓内无 Rust deserialize caller。`represented_source` 与 `root_presentation` 默认值只接受旧 manifest shape。 | 保留 `Serialize`，删除 `Deserialize` 与旧 shape defaults。 |
| `crates/common/src/tools.rs::ToolDef` | runtime catalog 定义供 Router 使用；provider contract 由 `ToolDef::json()` 手工生成，仓内无 `ToolDef` deserialize/serialize wire 消费者。`retry_safety` default 没有可接收的旧数据来源。 | 删除无消费者的 Serde derives 和旧字段 default；验证 crate 外没有实际公开序列化使用点后一起改。 |
| `crates/tools/src/workflow.rs`、`workflow_tool.rs` | 模型 inline workflow DSL 固定要求 `version: 1`，常量/校验/schema 只有一版且无持久化或版本解码；它不是历史恢复需要。 | 删除 DSL `version` 字段、常量、unsupported-version 分支及 prompt/schema 文字；保留 DAG 闭合校验、规模/深度限制、每步授权和部分失败语义。 |
| `crates/tools/src/builtin/messaging.rs::AgentParams` | `claim_token` 是旧进程内 claim receipt；当前 inbox 输出稳定 `message_ids`，ack schema 和生产路径已用 `message_ids`。旧 token 只返回提示改用新字段。 | 删除 claim token 字段、旧提示/验证和仅覆盖它的测试。确认 schema validation 对未知参数拒绝；必要时在该工具输入 DTO 上明确拒绝 unknown fields，不能让旧字段被静默吞掉。 |
| `crates/app-binary/src/commands/session_history.rs`、录音/权限/ToolRun commands、`bootstrap.rs` | `search_session_history` 与 paginated 旧入口由 filtered 查询替代；`get_recording_state`、`list_permissions` 无 renderer caller；`delete_tool_run` 仅 IPC handler 无 caller，但 Tools 底层删除能力仍有内部消费者。`export_session_history` 无 renderer caller且不是 alias。 | 删除无 caller 的 IPC handler、注册、生成 DTO 和契约条目；`delete_tool_run` 只删 command，不删底层能力。无 UI owner 的 export 作为无消费者 capability 删除。先以生产引用做最后复核，不按 allowlist 名字盲删。 |
| `crates/app-binary/src/commands/contracts.rs`、`bootstrap.rs`、`scripts/check-ipc-contracts.ps1` | `IPC_CONTRACT_VERSION` / `EXPECTED_COMMAND_COUNT` 只供 debug assert；checker 只比较 Rust 与 TS command name 集合。 | 删除固定计数和无协商能力的版本门面；保留注册命令集合、生成 shape 与 handler coverage 检查。 |
| `crates/messaging/src/inbox.rs::BacklogState`、`Envelope`、`AgentEntry` | Backlog sidecar 只有 version 1、无 decoder 分支；writer 总写 `version` / `active_attempts`。`Envelope.type` 与 `AgentEntry.capabilities` 的 writers 也总写字段，当前 defaults 只接受缺字段旧 JSON 行。 | 删除单版本 marker 与仅放宽旧 shape 的 defaults；让新 sidecar/envelope/registry 按当前格式重建。保留当前 inbox 原子重写、cursor ack、崩溃恢复和容量边界。 |
| `crates/messaging/src/inbox.rs::MAX_LEGACY_LINE_BYTES` 及读/修复入口 | 读端对旧超限 JSONL 行使用独立 1 MiB 上限，而新 outbound envelope 有更低的当前 contract 上限。 | 收敛 reader/repair/archive 的单行上限到当前 envelope contract；超限旧行不迁入当前队列，重置/备份策略明示。保留有界读取与 source-preservation 的故障保护。 |

### 3.3 需要一次持久契约收敛与数据库重置的候选

| 文件 / 符号 | 当前行为 | 目标 |
|---|---|---|
| `crates/memory/src/repositories/session_events.rs`、`crates/memory/src/schema.rs`、`crates/agent/src/{interaction.rs,memory_runtime.rs,react/transcript.rs,react/tool_batch_plan.rs,react/committed_ui.rs}` | 当前所有 event 写入恒为 `event_version = 1`；Memory/Agent reader 只有相等检查，没有版本分发/解码；打开数据库时另有严格 `user_version == SCHEMA_VERSION` 门禁。 | 删除 `CURRENT_EVENT_VERSION`、`SessionEvent.event_version`、SQL 列、所有读写和 DTO plumbing。若将来确实需要 event evolution，届时实现版本化 decoder，而不是继续保留只拒绝的固定版本字段。 |
| `crates/memory/src/repositories/tool_runs.rs`、`tool_run_store.rs`、`schema.rs` | `created_at` 当前 writer 明确写 SQLite `datetime('now')`；reader同时接受 SQLite 文本和 RFC3339。started/finished 的 SQLite fallback 主要由旧 fixture 模拟；当前 Tools writer使用 RFC3339。 | 所有 ToolRun 时间 writer 与表约束统一写 canonical UTC RFC3339 毫秒值；删除 SQLite 时间字符串 fallback 和旧 fixture。同步检查所有 INSERT，避免 schema 默认值继续生成非 canonical 格式。 |
| `crates/memory/src/repositories/messages.rs`、`session_events.rs` | projection watermark 使用 `strftime` 归一化；生产共享投影时钟写规范毫秒 UTC。9 位小数/offset 精度 rounding 由旧测试输入触发。`update_message_created_at` 无生产调用点。 | 当前投影读取严格校验规范格式；删除超精度旧兼容分支与无生产 consumer 的更新时间写口，或将其变为仅测试构造器。保留三表共享投影时钟、严格递增和 rollback cutoff 语义。 |

以上 SQL 列/存储格式切换应合并成一次 schema 版本升级，而不是每个文件分别 bump。当前源码 schema 是 v44；在 v45 统一移除 event version 并定 canonical 时间格式。项目不运行迁移：发布/切换时需备份后删除 `haven.db`、`haven.db-wal`、`haven.db-shm`，新版本建空库；这会清除会话、消息、记忆、权限、ToolRuns、用量和事件。不要执行隐式导入或恢复旧事件数据。

### 3.4 当前协议/配置与结构复杂度

| 文件 / 文件组 | 当前结构问题 | 新结构与保留边界 |
|---|---|---|
| `crates/mcp/src/{protocol.rs,client.rs,transport.rs,features.rs,manager.rs}` | 同时维护 `server/discover` 现代协议与旧 `initialize` 协商、`notifications/initialized`、Mcp-Session-Id、legacy HTTP GET SSE、旧 capability parsing 和 fallback version list。该整组是对更旧 MCP peer 的协议兼容。 | 按当前现代握手和 request-scoped transport 收敛，删 Legacy protocol mode/handshake/session/SSE 分支与对应测试；不支持旧协议的 server 明确连接失败。同步更新支持范围文档。保留 MCP 当前 wire 的 `inputSchema` camelCase alias。`McpToolExposure::Legacy` 是当前有效的工具暴露策略，不是旧协议 fallback，不删除。 |
| `crates/common/src/config/endpoint.rs`、`validation.rs`、`ui/src/lib/apiStyleCapabilities.ts`、`providerPresets.ts`、`settingsModelTypes.ts`、`ModelSettings.svelte`、`ProviderDialog.svelte` 与 generated IPC types | `ProviderConfig.media_capabilities: Option<_>` 在缺省时由 Rust 和 UI 两套 provider/host heuristic 推导；源码注释明确面向 existing/older config。设置 UI 已能编辑并保存完整三布尔值。 | 改成必需的显式 `ProviderMediaCapabilities`；所有 preset/default/编辑/IPC fixture 都写 `stt/tts/image_gen`，删除两端推断。保留 API style 上限 validator；缺失字段的 config 进入现有备份、只读恢复流程，用户通过新模板重建配置。同步更新 `architecture.md`、输出契约 inventory 与 `release-and-reset.md`。 |
| `crates/common/src/config/loader.rs`、`crates/app-binary/src/commands/settings.rs`、`ipc_codegen_types.rs`、`ui/src/lib/views/SettingsView.svelte` | `settings_pair!` 生成同字段 `AppConfig`/`Settings`；Settings 同时承担全量响应和局部输入。`apply_settings` 手工保留 UI 不可编辑的凭据、权限、MCP、skills、tool settings 等。 | 拆为全量 `SettingsSnapshot` 和按域的 `SettingsPatch`（必要时 section patch）；patch 字段明确“未提交/设值/清空”三态，AppConfig 只 apply 实际变更。移除宏生成的全量同构 Settings，不盲删所有 TOML/default，因为那也定义当前可省略字段的默认语义。原子配置写、external edit fingerprint 和坏配置备份只读恢复保留。 |
| `crates/app-binary/src/agent_tool_adapters.rs`、`runtime.rs`、`app_state.rs` | `ToolsFacadeAppToolRunAdapter` 与 `ToolsFacadeLiveOutputAdapter` 对现成窄 capability 逐方法同名透传，无转换/策略/校验；`ApplicationRuntime::new` 主要重包已有 capability。 | AppServices 直接持有窄 capability 并简化装配；测试专用 `schedule` 从生产 wrapper 移到 test support。不要扩大 capability，也不新增容器层。 |
| `crates/app-binary/src/commands/contracts.rs`、`ui/src/lib/contracts/commands.ts`、`docs/ipc-contracts.md`、`scripts/check-ipc-contracts.ps1`、`generate_ipc_contracts.rs` | Rust `COMMAND_CONTRACTS` 的 boundary/security 值目前不被 checker 逐字段比较；bootstrap 只检查数目/版本；TS 表只被测试消费。此前“Rust metadata 已是机器校验权威”的文档结论不成立。 | 保留一份安全分类权威并由现有 generator 生成 TS/文档副本；把逐字段 drift 纳入 checker。Rust 作为权威目标，不写成当前已实现。生成成本若不能复用现有 generator，退化方案是删除 Rust 重复表、明确只保留一份人工审计清单。 |
| `ui/src/lib/toolRunStore.ts`、`sessionToolRunStore.ts`、ToolRun event consumers、session `+page` | 同一 `ToolRunPayload` 被写入全局 live board 和 session terminal history 两个 store；两处有不同查询/淘汰生命周期，不能直接删一份。 | 合为一个按 ID 唯一持有 payload 的实体 map；全局 board 和 session history 只维护有序 ID 索引及各自 bounded/reconcile 规则。覆盖 stale refresh、终态移出 live board、history hydrate 和双索引容量，不复制 payload。 |

### 3.5 保留项：不是 Haven 旧版本兼容

| 文件 / 结构 | 保留理由 |
|---|---|
| `crates/llm/src/adapters/{openai,gemini,openai_responses}/*wire*` | Provider response alias/default 映射当前外部服务的 camelCase/snake_case、gateway variant 和可省略响应字段；属于运行时互通，不是读取 Haven 旧持久数据。逐 alias 保留有生产 fixture/当前 API 证据的项。 |
| `crates/mcp/src/protocol.rs::McpToolInfo.input_schema` | `inputSchema` 是当前外部协议字段名，保留 alias；只删旧协议握手。 |
| `crates/common/src/encoding.rs` | GBK/系统代码页回退用于读取用户现有文件内容，文件编码是当前输入能力，不是 Haven 应用版本兼容。 |
| `crates/tools/src/security.rs` 的路径、host process、network、live policy、revoke 和 proof 检查 | 这是当前授权安全网关；即使代码路径重复，也必须以同一策略 owner 抽取有序纯检查，不可删检查或复制第二套策略。 |
| `ToolResultEnvelope`、`generatedWireShape`、业务 contract mapper、renderer validator | 分别承载模型 observation 与 UI metadata 分离、IPC 线形状、领域身份/枚举和组件实际字段安全校验；验证层次不同，不合成一个宽松 parser。 |
| `ConfigLoader` 配置坏值备份与只读恢复、原子写/外部修改保护 | 这是防止覆盖用户有效配置的数据保护，不是旧版本兼容。保留。 |
| `crates/common/src/config/loader.rs` 的当前 TOML 字段 defaults | `deny_unknown_fields` 拒绝旧/拼错字段；缺省 defaults 同时允许用户只填写当前 schema 的必要设置。单凭字段缺省不能证明是旧兼容。保留明确属于当前 sparse-config 语义的默认；仅删除已由 writer/调用证明只接受旧形状的 `media_capabilities` 等具体 fallback。 |
| `McpToolExposure::Legacy` | 这是目前能配置的曝光级别，与 MCP transport protocol 的 legacy handshake 无关。保留。 |
| Ask/partial input/current optional DTO defaults、MCP feature response optional fields | 只要当前 writer 会省略该字段或当前 contract 允许缺省，它就是当前 wire 语义。仅删除由旧 writer-only 证据支持的 default。 |

## 4. 下一轮实施顺序

按依赖与破坏面排成以下单一队列；高影响切片直接执行并加深验收，不另设暂缓队列。

### Slice 1 — 修复 ToolRun result inject 的当前格式错配

修复 Agent envelope 到 UI parser/reducer 的路径；替换旧 Rust/UI fixtures。验收应使用 `format_tool_run_result_message` 当前输出作为 parser 输入，验证规范 ID/status/summary 正确展示、无效 JSON 保留原 supplement 且不生成伪成功、重复 supplement 不重复生成卡片。

### Slice 2 — 收敛通知设置运行时 owner

先修复 `+layout.svelte` 通知快照与 Settings 保存后 ToolRun completion store 分裂的问题。覆盖保存成功、持久化失败、durable-first partial apply 和初始 load 晚到；所有通知类别读取同一份配置。删除只供双 store 过渡使用的旧投影，不新增全局 service/store。

### Slice 3 — 删除确定的无消费者入口与内部 alias

一次清理 Interaction/Canonical/Usage aliases、只出不进 DTO 的旧反序列化/default、ToolRun completion 冗余投影字段、workflow 单版本字段、旧 UI operation/tool name/file renderer/Ask option shapes、Envelope 宽松 enum/Default、旧 transcript fixture、无 caller IPC 和 `memory_episodes` 特例。同步删除 messaging ack 的 `claim_token` 旧路径与 inbox 单版本/大行兼容，但保留 inbox 原子恢复。更新 IPC 生成产物、UI type guards、相关测试和当前契约文档。严格区分 `delete_tool_run` IPC 与 Tools 内部删除 capability。

Inbox 侧格式要按无兼容目标明确切换：升级前备份后重置 `inbox/` 的 mailbox、processing、backlog/cursor、registry 与 tombstone；其内容不会从旧 sidecar/超限行迁入。验收当前 `message_ids` ack、current envelope 限额、原子 claim/ack 与崩溃恢复。

### Slice 4 — 合并持久格式变更并做一次 schema v45 reset

移除 event_version plumbing；统一 ToolRun 写入与持久投影时间格式；收紧读端、删除 old SQLite/high precision fallback 和无消费者更新时间写口。检查所有 writer/SQL default 后一次改完。按项目 reset guide 备份/删除数据库并创建新 schema；验证 event append/replay、共享 projection clock、rollback 双水位/usage compensation 和 ToolRun outbox 行为。不得留下旧格式隐式导入。

### Slice 5 — 明确配置与 Settings patch 契约

先把 Settings response 与 patch 分开，再把 provider media capability 改为必填显式三布尔；更新 TOML schema、默认配置、预设、Settings、生成 DTO 和 help/reset 文档。验证未改 section 不变、显式清空能表达、持久化失败不 apply、partial runtime apply 语义不变；旧缺字段 config 按当前恢复策略备份并要求重建。保留已有配置保护措施。

### Slice 6 — 将 MCP protocol 固定到当前现代握手

删除旧 initialize negotiation、Legacy `McpProtocolMode`、Mcp-Session-Id 和 legacy HTTP SSE 路径以及对应测试。保留现代 stdio/HTTP 行为、SSE/request-stream 取消与 shutdown、现代 capability mapping 和安全检查。验证旧 peer 清晰失败，现代 peer 经两种 transport 完整连接、列举、调用和关闭；同步界面支持说明。

**实施完成（ADR 1299）**：旧握手/session/独立 GET SSE 和兼容测试已移除；连接只接受当前 `server/discover`。现代 stdio/HTTP 请求、request-scoped SSE 与 bounded process shutdown 保留。代码和集成门禁结果由 ADR 1299 记录。

### Slice 7 — 收紧模型生成的事实抽取结构

移除 `fact_extraction` / `fact_inference` 的宽泛类型 coercion；对 schema 不匹配的模型输出拒绝或跳过对应项，不将 null 变为空事实，不把对象/数组 stringify 后写入记忆。保留 JSON 响应 envelope 提取、字符串清洗、提示注入过滤和事实身份/实体验证。验收覆盖当前合法模型输出与各错型值、部分无效 batch 不污染有效事实。

### Slice 8 — 合并 App 转发层和重复数据/metadata 副本

移除 App 纯转发 adapter；把 ToolRun store 改为唯一 payload map + 双索引；把 command security metadata 设为单一权威并扩展现有 generator/checker；把 AgentEvent `{channel,payload}` projection 绑定到生成 shape registry。验收检查 owner 唯一、生成产物不 drift、UI 查询生命周期与 bounded limits 保持。

### Slice 9 — 延续第一轮仍成立的内部职责切片

前一轮中的通知配置单 owner 已提前为本轮 Slice 2；AppCommand confirmation owner、SessionStore 私有模块拆分、Tools `security.rs` 私有职责拆分、Agent `tool_runner.rs` 私有职责拆分仍成立，按 [第一轮源码审计方案](refactor-plan-source-audit-2026-10-17.md) 的事务与安全边界执行。第一轮计划中关于 command metadata “Rust 目前已是权威”的事实描述以本轮复核纠正；当前实际不是逐字段校验。本轮不重新把已验证的事务、session、authorization 不变量重复设计一遍。

## 5. 实施门禁与数据切换

- Slice 1/2 运行 UI type check 和 UI 测试；Slice 3 运行受影响 crate/UI 与 IPC contract 检查；Slice 4 运行 workspace locked tests/clippy 和投影/rollback/outbox 恢复场景；Slice 5 跨 Common/App/UI 运行完整 workspace 与 UI/IPC 门禁；Slice 6 运行 MCP/Tools 集成与 Windows 真实进程/HTTP 行为门禁；Slice 7/8/9 按跨 crate/跨端影响执行完整门禁。实施时失败就修切片，不以风险为由撤回排期。
- 数据库 v45 变更不迁移旧 DB：按发布重置说明备份后删除 `haven.db`、WAL/SHM。重置影响 session、transcript、memory、permissions、tool runs 和 usage。
- Inbox current shape 收敛同时备份并重置整个 `inbox/`；会清除 peer registry、待处理消息、backlog/processing、archive 和 deletion tombstone。不得只删 cursor 让旧 backlog 被部分重放。
- Settings DTO 和 provider config 破坏性变化无需 DB 重置；损坏/旧配置仍由现有 ConfigLoader 备份并进入只读恢复，提供当前模板重建步骤。
- 每个逻辑切片更新 ADR/current roadmap；历史 ADR 保留历史决策，不改写成当前实现事实。生成文件通过生成器更新，不手工修生成结果。

## 6. 审计结论

本轮已确认：Haven 自有的 interaction/transcript/usage/UI discriminator aliases 与 SQLite 时间格式兼容可以删除；单版本 `event_version` 和 `memory_episodes` 特例没有当前解码/迁移消费者；配置中的 `media_capabilities` 缺省推断是显式旧配置兼容；Settings 的全量 DTO 复用局部写入让 `apply_settings` 承担了大量隐藏保留逻辑。另一方面，LLM/MCP 当前 wire 字段、文件编码、授权检查、配置损坏保护、验证分层和 App/Session 生命周期 owner 都有当前消费者，不能按名称或“多层”印象删除。

当前版本 policy 允许破坏性清理。schema reset 和老 MCP peer 断连是预期变化，必须在实施说明中清楚写出，而不是因此延后执行。
