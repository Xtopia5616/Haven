# Haven 大文件与热点拆分执行计划

> 用途：把当前代码审计结果交给后续 agent，按稳定职责拆分大文件；允许破坏性结构重构，但保持行为、IPC、持久化和安全契约不变。
>
> 本文是执行计划，不授权新增功能或顺手清理无关代码。每个目标应独立完成、独立验证、独立提交。

## 1. 执行前必须阅读

执行 agent 开始前必须阅读：

- `AGENTS.md`
- `docs/development-standards.md`
- `docs/architecture.md`
- `docs/refactor-execution-guide.md`
- `docs/stability-refactor-plan.md`
- 与当前目标相关的 `docs/adr/` 文件

开始时运行 `git status --short`、`git diff --stat` 和 `git diff`，保留用户已有修改，不覆盖或代提交无关变更。

## 2. 审计结论

统计排除了 `target/`、`node_modules/`、`ui/build/` 和 `ui/.svelte-kit/` 等生成目录；行数包含注释和空行，仅用于定位热点。

| crate | Rust 总行数 | 生产代码 | 测试代码 | 源文件数 |
|---|---:|---:|---:|---:|
| `haven-agent` | 31.4k | 18.3k | 13.1k | 46 |
| `haven-tools` | 29.0k | 17.0k | 12.0k | 34 |
| `haven-llm` | 21.0k | 12.3k | 8.7k | 32 |
| `haven-memory` | 14.7k | 8.7k | 5.9k | 21 |
| `haven-app-binary` | 8.9k | 7.2k | 1.7k | 22 |
| `haven-mcp` | 2.4k | 2.0k | 0.4k | 2 |

最大的非代码文件是 `assets/models/silero_vad.onnx`（约 2.7 MB），它是模型文件，不进行代码拆分。

结论：当前优先做文件级拆分，不把 `agent`、`tools` 或 `llm` 直接拆成新 crate。它们已经按领域拥有较多子模块；贸然拆 crate 会扩大依赖、公共 API 和测试迁移范围。

## 2.1 兼容层原则

- 这是测试版项目，允许为了清晰的最终边界进行破坏性重构；不要为了保留旧的内部导入路径而长期维护 re-export、代理函数或双入口。
- workspace 内部调用点应在同一轮重构中迁移到新模块。不能新增依赖旧 facade 的代码，也不能把 facade 当作永久 API 设计。
- 如果拆分过程中确实需要 facade，它必须在文档或 ADR 中写明：用途、受影响调用点、删除条件和预计删除轮次；没有删除条件的 facade 不得保留。
- “完成”不等于“旧入口还能工作”。完成标准是调用方已迁移、旧入口已删除，或有明确且必要的外部稳定 API 理由。

## 2.2 兼容层与架构妥协审计（2026-09-01）

本次只读审计按“是否为了旧 Haven 内部设计继续存在”来判断，不把第三方协议适配、崩溃恢复和安全降级一律当成技术债。结论是：除 `bg.rs` 外，当前还有几处明确的内部兼容层，以及几处已经让当前数据模型变复杂的旧路径。

### A. 高置信度的内部兼容层：应在破坏性重构中删除

| 优先级 | 位置 | 当前妥协 | 目标动作 |
|---|---|---|---|
| P0 | [`crates/common/src/types.rs`](../crates/common/src/types.rs)、[`crates/agent/src/session/queues.rs`](../crates/agent/src/session/queues.rs)、[`crates/agent/src/session/mod.rs`](../crates/agent/src/session/mod.rs) | `Supplement` 是历史 struct，`FollowUp` 只是 type alias；队列同时保留 `add_supplement*` / `get_supplements` 和 `add_follow_up*` / `get_follow_ups`。生产恢复路径仍调用旧的 `add_supplement_with_attachments`。 | 选择 `FollowUp` 作为唯一类型和队列 API，直接迁移 `resume.rs`、ask 文档、测试与导出，删除旧方法、旧 type alias 和双重 re-export。`AgentEvent::Supplement` / `ProcessResult::Supplemented` 属于跨端事件契约，如要一并改名，单独登记 IPC 破坏性变更，不要靠第二套名字长期兼容。 |
| P0 | [`crates/app-binary/src/commands/skills.rs`](../crates/app-binary/src/commands/skills.rs) | `execute_skill` 接收 `confirmed: Option<bool>`，但代码明确忽略它；安全授权已经统一由 `SafetyGateway` 决定，当前 UI 也不发送该字段。 | 从 Tauri 命令签名、命令契约和测试中删除 `confirmed`，让 SafetyGateway 成为唯一授权入口。 |
| P0 | [`crates/app-binary/src/commands/session.rs`](../crates/app-binary/src/commands/session.rs)、[`ui/src/routes/+page.svelte`](../ui/src/routes/+page.svelte) | 新的 `effect` + `scope` 已经存在，但 Rust 和 UI 仍保留 `trust_session` / `trustSession`，并在缺少新字段时猜测旧语义。 | 同一轮中迁移所有 UI 调用到 `effect` + `scope`，删除 `trust_session` bridge；下一步可将决定收窄为有类型且必填的 DTO，避免继续允许“缺字段再猜”。 |
| P1 | [`crates/agent/src/types.rs`](../crates/agent/src/types.rs) | `ReActSnapshot.upgrade_tool_rounds` 已标注“只为进程内测试 fixture 保留”，生产解析和 resume 不再使用，但字段仍存在且大量测试逐个填空 Vec。 | 删除字段，集中更新测试 fixture。快照已明确采取“不支持旧形状、要求 reset”的策略，不需要再为已废弃的升级路径留空壳。 |
| P1 | [`ui/src/lib/ToolResultCard.svelte`](../ui/src/lib/ToolResultCard.svelte) | `toolResultParsing.ts` 已是解析权威，但 `ToolResultCard` 仍通过 `<script module>` re-export `parseToolResult` / `canRenderToolResult`，文档也明确称其为兼容 re-export。生产代码已直接导入新模块，剩余依赖主要在测试。 | 删除 re-export，测试直接从 `toolResultParsing.ts` 导入。保留 `ToolResultCard` 组件本身作为卡片壳，不保留旧模块路径兼容。 |
| P1 | [`crates/memory/src/embeddings.rs`](../crates/memory/src/embeddings.rs) | `search_episodes_by_keywords` / `_excluding` 是只返回文本的旧 public facade；新的 typed 查询返回 `entity_id + text`，用于正确去重，workspace 没有生产调用旧 facade。 | 迁移或删除旧 text-only 方法和对应测试，统一使用 typed hit，避免调用方继续丢失实体身份。 |
| P1 | [`ui/src/lib/sessionStatus.ts`](../ui/src/lib/sessionStatus.ts) | `ACTION_STATUSES = SESSION_STATUSES` 只是旧命名 alias，workspace 生产代码没有使用，只有 alias 自己的测试。 | 删除 alias 和测试，不要继续用 action 术语污染 session 状态模型。 |

### B. 已经影响当前架构的兼容妥协：先改模型，再删 fallback

| 优先级 | 位置 | 为什么不是简单删一行 | 目标架构 |
|---|---|---|---|
| P0 | [`crates/agent/src/rollback_support.rs`](../crates/agent/src/rollback_support.rs)、[`crates/common/src/types.rs`](../crates/common/src/types.rs) | rollback 已优先按 `UserInject.message_id` 精确定位，但无 id 的旧事件和 `CompactSummary` 仍按文本、wire prefix 匹配。代码明确说明：当前 compaction 没有保留原始 user message id，所以只能 content fallback；这让一个本应是身份操作的危险路径依赖文本相等。 | compaction/event provenance 必须保留被压缩 user message 的原始 `msg-*` 身份，rollback 全路径只接受精确 id；之后删除 `InjectSource::match_prefixes`、id-less content matching 和相关测试。保留 rollback 双时钟和 `last_msg_at` 语义不变。 |
| P1 | [`crates/agent/src/resume.rs`](../crates/agent/src/resume.rs)、[`crates/agent/src/resume_support.rs`](../crates/agent/src/resume_support.rs) | 当前有两套 resume authority：有效 snapshot 走 `events`，缺 snapshot 时从 `session_steps` 重新投影，并为缺失 provider id 生成 `call-*`。这不是单纯的模块拆分，而是两种可能不一致的 transcript 语义。 | 测试版可在发布边界选择严格方案：snapshot 缺失就提示 reset / 新会话，删除 snapshot-less projector；如果产品仍要保留灾难恢复，则必须把它命名为独立的显式 recovery mode，不能继续称为普通 resume，也不能继续扩展第二套投影语义。 |
| P1 | [`crates/agent/src/react/retries.rs`](../crates/agent/src/react/retries.rs)、[`crates/agent/src/react/turn.rs`](../crates/agent/src/react/turn.rs) | `awaiting_answer` 已是显式状态，但 loop 仍扫描 canonical tool 文本中的 `{"ask":true}`，并从不可解析文本猜问题。这是旧 observation 形状的 JSON heuristic，与结构化 ask 信号并存。 | 让 `awaiting_answer` / typed tool result 成为唯一来源；删除 `canonical_has_pending_ask`、`extract_pending_ask_question` 及 substring scan 测试。无法恢复结构化 ask 时 fail closed，而不是猜测。 |
| P1 | [`ui/src/lib/resumeMessages.ts`](../ui/src/lib/resumeMessages.ts)、[`ui/src/routes/+page.svelte`](../ui/src/routes/+page.svelte) | resume 仍识别旧的 `__ask__` sentinel、非 JSON observation、按内容删除重复 ask；rollback 还会在 optimistic bubble 没有 `msg-*` 时用“内容 + 最近时间”找 DB id。当前新路径已经按 step/message id 工作，这些是旧行和旧竞态的补丁。 | 在数据 reset / 新提交协议生效后，删除 sentinel、内容配对和内容找 id；提交时保证 optimistic bubble 与后端返回的 canonical `msg-*` 一一绑定，resume 只按稳定 id 合并。删除前必须补齐 reset 说明和并发提交回归测试。 |
| P1 | [`crates/common/src/config/endpoint.rs`](../crates/common/src/config/endpoint.rs) | `api_style` 已是协议字段，但空 `api_style` 时仍从历史 `provider` hint 推导；`wire_provider_hint` 又反向保留 provider 名以触发 DeepSeek/xAI 等特例。一个 `provider` 字段同时承担连接身份、厂商能力和协议选择，形成双字段/多语义模型。 | 配置模型明确拆成 `wire_protocol`（或现有 `api_style`）与必要的 `vendor`/provider identity；迁移配置后要求显式协议，不再通过 `provider` 猜测。`model`→`model_name`、`Stdio`/`Http` serde alias 也应在同一 reset 边界清理。厂商 preset 可保留为 UI 创建配置时的模板，不要继续作为运行时隐式 fallback。 |
| P2 | [`crates/tools/src/inbox.rs`](../crates/tools/src/inbox.rs)、[`crates/agent/src/react/context.rs`](../crates/agent/src/react/context.rs)、[`crates/tools/src/builtin/messaging.rs`](../crates/tools/src/builtin/messaging.rs) | ReAct 自动收件使用 durable `claim_and_archive` + `ack_claimed`，显式 `inbox` 工具仍使用立即 drain 的 `read_and_archive`。两条路径的崩溃和确认语义不同；代码注释直接称后者为 legacy path。 | 统一到一个 claim/project/ack 原语；显式工具如果需要同步返回，应在该原语之上做一次受控消费，而不是保留第二套文件状态机。若短期不能合并，必须写明它是用户可见的同步 adapter、禁止新增内部调用，并给出删除/合并条件。 |
| P2 | [`crates/agent/src/prompt.rs`](../crates/agent/src/prompt.rs) | `patch_system_memory` 仍能升级旧 `USER FACTS` / `Past conversation excerpts` 布局。当前 builder 已有明确的新布局，旧 patch 主要服务旧 snapshot 中的 system prompt。 | 在 snapshot reset 边界后只保留当前布局替换；删除 `strip_legacy_past_excerpts` 及旧 fence 分支，并保留当前 prompt-cache-friendly 的局部 patch。 |
| P2 | [`crates/tools/src/builtin/scheduled_action.rs`](../crates/tools/src/builtin/scheduled_action.rs) | `ScheduledActionFired` 对旧行允许 `session_id=None`、`prompt=None` 并回退到 `body`，导致同一 DTO 同时表示当前 Tool/Continue 语义和旧唤醒语义。 | 迁移或清理旧 scheduled rows 后，按 mode 要求相应字段；`prompt` 与 `body` 的语义不要再互相兜底。 |

### C. 不应误删的兼容/降级

以下目前看起来不是“为了不做内部重构而保留的旧架构”，不纳入本计划的删除清单：

- [`crates/llm/src/adapters`](../crates/llm/src/adapters) 对 OpenAI-compatible、Anthropic、Gemini、Deepgram 以及 MCP JSON-RPC wire shape 的字段别名和协议差异。这些是外部服务契约，不是 Haven 内部旧 API；Responses 的 developer-input downgrade、DeepSeek reasoning echo、prompt-cache capability probe 也属于供应商互操作。
- `crates/memory/src/migrations.rs` 的版本化 schema/data migration。它是有边界的历史数据迁移，不等同于永久保留内部双入口；若要整体清理，应另做数据库 reset/release 任务。
- UTF-8/GBK、PowerShell CLIXML、provider failover、媒体低置信度回退、进程崩溃恢复和超时保护。这些是平台/供应商/故障处理能力，除非后续证明它们只是旧内部实现的残留，否则不能按兼容层删除。
- `/history`、旧 tab 路径和 keep-alive 路由 redirect。它们是用户导航兼容，优先级低；若决定删除，应先确认没有需要保留的书签/深链接，再单独改路由契约。

### D. 建议执行顺序

1. 先删除无生产调用的 alias/空字段：`upgrade_tool_rounds`、`ToolResultCard` parsing re-export、`ACTION_STATUSES`、memory text-only search facade。
2. 再完成 FollowUp/Supplement、confirmation IPC 和旧 ask/retry signal 的单一命名/单一来源迁移；这些会触及跨 crate 或前端契约，按领域独立提交。
3. 然后处理 rollback provenance 和 snapshot-less resume。它们涉及数据语义，必须先写 ADR、补身份/重置测试，再删除 fallback。
4. 最后收敛配置双字段、inbox 双消费路径、scheduled row fallback 和 prompt 旧布局。每项都要明确是否删除旧数据；不要为了“以后可能有旧用户”把临时分支重新留回去。

本节中的“删除”均默认测试版破坏性变更：删除前同步更新旧测试、文档和发布重置说明；不得只删生产分支而保留旧 fixture 继续掩盖兼容入口。

## 2.3 战略级重构候选：哪些边界值得推翻（2026-09-01）

本节不是当前文件拆分任务的直接授权，而是基于依赖、状态和功能实现的二次架构审查。结论比较激进：当前最大的风险不是某个文件过大，而是同一业务事实被多个运行时容器、事件形态和投影路径重复维护。若只继续机械拆分，可能得到更多更小的模块，但不会消除根本复杂度。

测试版允许破坏性重构。以下候选应先各自写 ADR、定义重置范围和验收测试，再按领域独立迁移；不得把所有候选一次性合并成“全量重写”。

### 总体判断

建议把目标架构收敛为：

```text
Tauri / UI
    │  typed commands + session event stream
    ▼
ApplicationRuntime（装配、生命周期、配置快照）
    ▼
SessionSupervisor（全局调度、并发、取消）
    ▼
SessionActor（单会话唯一状态所有者）
    ▼
RunEngine（纯 ReAct 状态机）
    ├── ModelGateway（按请求能力路由）
    ├── ToolRuntime（工具执行与授权）
    └── SessionEventStore（append-only 事件）
             ├── transcript / history projection
             ├── action / usage / notification projection
             ├── live subscription
             └── memory / title 等后台消费者
```

目标不变量是：一个会话只有一个状态所有者；一个业务事实只有一个持久化权威；UI 的实时更新和恢复都消费同一条事件序列；后台任务和人工交互都有明确的持久状态机。SQLite、供应商 adapter 和 Windows 进程适配可以继续保留，它们不是本次要推翻的对象。

### A. P0：推翻 `SessionExecutor` 的“大总管”模型

当前 [`crates/agent/src/session/mod.rs`](../crates/agent/src/session/mod.rs) 的 `SessionExecutor` 同时拥有：session cache、FIFO dispatcher、运行集合、信号量、取消 token、状态 watch、pending queue、action completion、ask gate、confirm gate、scheduled confirm、partial store 和多组一次性 callback。`SessionInfo` 又把 follow-up/steering 队列放在另一层的 session mutex 里。这样会话的状态分散在多个 `HashMap + Mutex`，恢复、暂停、确认和结束都要跨多个 owner 协调。

建议推翻为三层：

1. `SessionSupervisor` 只负责全局排队、并发 permit、启动/停止 session actor。
2. `SessionActor` 以 mailbox 串行拥有一个会话的 status、输入队列、interaction、run lifecycle、partial/checkpoint 和 action completion。
3. `RunEngine` 不再反向调用 executor 的几十个方法，而是接收不可变 `RunContext`，输出 typed `RunEffect` / `SessionCommand`。

迁移完成后应删除 session 级 `HashMap` 之间的交叉协调、`on_*` callback 网和把队列藏在 `SessionInfo` 里的运行时状态。对外仍可保留 `SessionHandle`，但它只能发送命令，不能暴露内部 mutex。

这不是为了换一种并发风格，而是为了让“一个 session 的所有状态变更按顺序发生”成为代码结构保证，而不是靠锁顺序、回调注册顺序和测试覆盖保证。

### B. P0：把 snapshot blob 权威改成数据库事件流，snapshot 降级为缓存

当前 [`crates/agent/src/types.rs`](../crates/agent/src/types.rs) 已经把 `ReActSnapshot.events` 定为 transcript 权威，但它仍作为整块压缩 blob 存储并频繁重写；同时 `messages`、`session_steps`、`AgentEvent` 和前端 resume builder 又分别承担投影或实时状态。恢复因此存在 snapshot authority、snapshot-less projector、DB projection merge 和 live event merge 多条语义路径。

更彻底的目标是新增版本化的 `session_events` append-only 存储：每个事件有 `session_id`、单调 `sequence`、事件类型、payload、时间和 run/step identity。`ReActSnapshot` 只保留为定期 checkpoint/cache，不再是唯一持久化真源。`messages`、`session_steps`、usage、action 和 UI live stream 都从同一批已提交事件投影；实时订阅按 sequence 重放，断线后从最后 sequence 继续。

这样可以直接消除或显著收窄：

- 整块 snapshot 每步重写和长会话的 O(n) 持久化成本；
- rollback 依赖 `last_msg_at` 与 event cursor 的双时钟；
- snapshot 缺失时另起一套 projector；
- UI 的 live/resume 内容匹配、旧 sentinel 和 optimistic bubble 补丁；
- `AgentEvent` 与 durable transcript 之间需要人工保持一致的映射。

这是本审查中最值得“重新定义数据模型”的一项，但必须先做事件 schema、顺序/幂等、事务提交和数据库重置 ADR。若暂时不做，应至少把当前 snapshot 方案当作明确的过渡架构，而不是继续向其中添加新的状态字段。

### C. P0：统一 ask、confirm 和其他人工阻塞为 `InteractionRequest`

当前 ask 与 confirm 在 [`crates/agent/src/types.rs`](../crates/agent/src/types.rs)、`SessionExecutor`、resume/rollback、Tauri command 和 UI 中都有各自的状态：`awaiting_answer`、`awaiting_confirm`、`scheduled_confirms`、`paused_awaiting_answer`、`paused_awaiting_confirm`，前端也分别有 ask controller 和 confirm queue。两者本质上都是“运行暂停，等待外部主体提交一个带 id 的决定”。

建议统一为一个持久化 `InteractionRequest`：

```text
InteractionRequest {
  id,
  session_id,
  kind: Ask { question, options } | Confirm { operation, risk, scope },
  source_step,
  status: Pending | Resolved | Expired | Cancelled,
  response,
}
```

普通用户回答、安全确认、定时任务确认和未来的权限/登录请求都走同一个 request/resolve/cancel 生命周期。UI 只需要一个 interaction store；后端只需要一个恢复、超时、回滚和幂等解析入口。`SessionStatus` 可以保留面向用户的显示状态，但不再为每一种等待原因复制一套 executor map 和 resume 分支。

这项重构还应明确“回答是对哪个 request 的回复”，禁止再根据当前是否 paused、文本内容或 tool observation 猜测输入归属。

### D. P1：把 background action 与 scheduled action 合并成真正的 `ActionService`

数据库已经用 `actions.kind` 区分 `background` / `scheduled`，但运行时仍是 [`crates/tools/src/bg.rs`](../crates/tools/src/bg.rs) 的 `BackgroundActions` 加上 [`crates/tools/src/builtin/scheduled_action.rs`](../crates/tools/src/builtin/scheduled_action.rs) 的 `ScheduledActionCenter` 两套状态机；它们再通过 `set_actions`、DB setter、事件 sink、agent consumer 和 fired/completion channel 互相接线。

建议把 action 统一成一个持久化状态机：

```text
Action { id, owner/session, kind, spec, state, output, error, timestamps }
Pending → Running → Succeeded | Failed | Cancelled | Expired
                 └→ Waiting（timer / dependency / interaction）
```

shell 后台执行、定时触发、等待另一个 action、完成后唤醒会话都只是不同的 `ActionSpec` / worker，不再是两套 registry。`actions` 表成为状态权威，内存 worker 只是执行句柄和短期输出缓存。这样可以统一重启恢复、取消、权限、历史、通知和 UI action board，也能消除 `session_id=None`/`prompt` 回退等旧语义。

### E. P1：收窄 `ToolsManager`，删除 callback service locator

[`crates/tools/src/lib.rs`](../crates/tools/src/lib.rs) 的 `ToolsManager` 同时管理 registry、MCP、skills、shell defaults、context limits、safety gateway、background/scheduled action、audio pipeline、self tool、router，以及通过 setter 注入的 agent spawner 和 memory recall。`app_state.rs` 以 `Arc<dyn Fn>` 把 agent 反向接回 tools，虽然避免了 crate 循环，却把组合根的依赖隐藏成运行时 callback 网络。

建议重划分为：

- `tool-core`：Tool contract、typed result/signal、registry/catalog、授权接口；
- `tool-runtime`：执行上下文、取消、超时、并发和 action/interaction port；
- `tool-builtins`：shell/file/system/audio 等具体能力；
- MCP/skills/agent/memory adapter：作为组合根注入的 capability implementation。

不一定马上新增四个 crate；先用模块和 trait 建立边界，再决定是否把 `tool-core` 单独成 crate。目标是 `ToolsManager` 成为 catalog/composition 对象，不再成为整个应用的 service locator；模型启动子 agent、查询 memory、创建 action 应通过明确的 capability port，而不是可变 callback slot。

### F. P1：重做 memory 与 prompt 的责任边界，并删除硬编码身份事实

当前 [`crates/agent/src/prompt.rs`](../crates/agent/src/prompt.rs) 同时负责 prompt render、工具/技能/MCP index cache、数据库 memory recall、向量模型调用和 MEMORY fence patch；[`crates/agent/src/inference.rs`](../crates/agent/src/inference.rs) 又同时负责事实抽取 outbox、LLM 仲裁、事实维护、embedding catch-up 和 recall。建议拆成：

1. `MemoryService`：只提供 typed query、memory proposal、commit、index status。
2. `MemoryWorker`：消费已提交会话事件，异步抽取事实、生成 embedding、维护索引。
3. `PromptContextProvider`：在 turn 边界取得一次有上限的上下文快照。
4. `PromptRenderer`：纯函数，把上下文快照渲染成 system message，不直接碰 DB、router 或 cache。

另外，[`crates/agent/src/layer.rs`](../crates/agent/src/layer.rs) 构造 `AgentLayer` 时会执行 `ensure_fact("user", "name", "Xtopia", ...)`。这不是合理的默认配置，而是产品身份数据与运行时初始化混在一起的明显 placeholder/功能错误。应删除；如果产品需要用户名称，应走首次设置/用户 profile，并明确来源、可修改性和是否允许进入 prompt。不能让每次启动隐式写入一条伪造的长期记忆。

### G. P1：模型路由从固定角色改成 capability/request policy

当前 [`crates/common/src/config/endpoint.rs`](../crates/common/src/config/endpoint.rs) 和 [`crates/llm/src/router.rs`](../crates/llm/src/router.rs) 固定 six slots：small/default/balanced/image/audio/embedding，再用 `api_style`、provider hint、`stt_use_audio_model`、`vision_use_image_model` 和 balanced fallback 叠加语义。它能工作，但新增能力时会继续增加 slot、布尔开关和特判。

更清晰的模型是：请求声明 `RequestKind` / `Capability`（chat、fast_chat、vision、transcription、embedding、image_generation、speech_synthesis），配置声明 provider capability 和有序 fallback policy，router 只执行 policy。`balanced` 不再是隐藏的全局逃生出口，而是一个显式 fallback chain；provider identity、wire protocol、model capability 也分别建模。

此项不要求重写 provider adapter。adapter 仍保留为外部 wire compatibility；推翻的是 Haven 内部配置和路由语义，目标是让“能不能做、用哪个模型、失败后是否回退”成为可观察的策略，而不是六个字段和多个 bool 的组合。

### H. P1：让前端也消费同一事件 reducer，而不是维护 live/resume 两个世界

当前 [`ui/src/routes/+page.svelte`](../ui/src/routes/+page.svelte) 仍是聊天编排、事件订阅、确认队列、输入提交和展示状态的汇合点；[`ui/src/lib/resumeMessages.ts`](../ui/src/lib/resumeMessages.ts) 又独立把 messages + steps 组装成另一种消息世界。即便当前已经大量使用稳定 id，live-only、DB-only、streaming、ask legacy 和 optimistic bubble 仍需要复杂 merge 规则。

目标应是一个 typed `SessionReducer`：

- 初次打开：从 session event sequence replay；
- 实时更新：追加同一种事件；
- 断线恢复：从 last sequence 补 replay；
- UI 组件：只渲染 reducer 产生的 `SessionView`。

这会让 `+page.svelte` 退回路由编排层，ask/confirm 进入统一 interaction store，tool card 只按注册表渲染。后端事件流完成前，可以先把现有 Tauri event 与 resume DTO 适配到同一个 reducer，但不要继续增加第三套 UI merge 特例。

### I. P2：应用启动改成有生命周期的 `ApplicationRuntime`

[`crates/app-binary/src/app_state.rs`](../crates/app-binary/src/app_state.rs) 现在既是组合根，又启动 memory maintenance、retention、prewarm、bootstrap、MCP/skills discover、dispatcher 和多种后台 consumer；很多 `tokio::spawn` 任务没有统一的 owner、取消 token 或 shutdown join。建议抽出 `ApplicationRuntime`，集中持有服务句柄、后台任务和 shutdown token；`AppState` 只暴露 Tauri command 所需的稳定 handles，`lib.rs` 只负责宿主适配。

这不是为了把启动代码分成更多文件，而是为了确保窗口关闭、配置热替换、数据库关闭和测试 teardown 时，所有后台任务都有明确的停止语义。启动失败也应返回阶段化的诊断，而不是部分服务已经 spawn 后继续运行。

### J. 暂不推翻的边界

以下内容当前看起来是合理的稳定边界，除非新的证据证明其实现有功能错误，不建议为了“彻底重构”而重写：

- `haven-llm` provider adapter 的外部协议映射、SSE/JSONL framing、厂商差异和 failover；
- `SafetyGateway` 的 deny-first 授权原则、路径/进程安全检查和负向测试矩阵；
- `haven-input` 的 CPAL/VAD/录音生命周期与 `haven-llm` 的 provider 实现分离；
- SQLite WAL、schema version/migration 和 Windows 编码/进程树终止等平台故障处理；
- Tauri DTO 的单一事件映射点，以及已有的命名边界。

### K. 激进路线的执行顺序

如果决定按“允许破坏性重构”的路线走，建议顺序如下，每一步都独立提交：

1. 写 ADR 并确定 reset boundary：session snapshot、actions、旧配置、UI local state 是否全部清空；先建立事件、interaction、action 的行为测试。
2. 建立 `SessionEventStore` 和投影测试，先迁移一个完整的 session create → user input → one turn → tool result → resume 链路。
3. 引入 `SessionActor` / `SessionSupervisor`，暂时把旧 ReAct 引擎包在 actor 内；新链路稳定后删除旧 executor maps/callbacks。
4. 迁移 `InteractionRequest` 和 `ActionService`，删除 ask/confirm 双状态机与 background/scheduled 双 registry。
5. 收窄 tools、memory、prompt 和 model routing 的 ports；删除 callback setter、prompt DB 访问和硬编码身份事实。
6. 迁移 UI `SessionReducer` 与 `ApplicationRuntime`，最后删除 snapshot-less projector、内容匹配、旧 sentinel 和旧事件 merge 分支。

在第 2 步之前，不应开始大规模 provider 或 UI 视觉重写；在第 6 步完成之前，不应宣布“事件统一”完成。上述战略候选与本文件第 3 节的机械拆分是两条不同路线：文件拆分可以先做，但一旦选定战略路线，相关模块拆分应服务于新边界，不能把临时 facade 固化成最终架构。

## 2.4 用户决策：允许重写的范围（2026-09-02）

用户明确决定：上一节“暂不推翻”的六类内容中，保留第一个和最后一个的核心边界；中间四类可以按破坏性重构处理，后续拆成多个独立任务逐步实现。

### 保留核心边界

1. **Provider adapter**：保留 `haven-llm` 作为唯一 provider 协议实现层，保留外部 wire compatibility。可以拆文件、抽共享 transport/stream/retry、重做内部 capability 描述，但不做一次性 provider 协议重写。
2. **Tauri DTO/event bridge**：保留 `haven-app-binary` 集中做 Rust 内部事件到 Tauri DTO 的适配和 snake_case/camelCase 边界转换。可以逐步接入带 sequence 的事件流和前端 reducer，但不把映射职责分散回 agent/tools/UI。

“保留”不代表禁止修复或整理；它表示不改变这两个边界的基本职责，不把它们列入本轮概念级推翻范围。

### 保留边界下的改进方向

#### 1. Provider adapter：保留协议适配，重做内部组织

当前 [`crates/llm/src/adapters`](../crates/llm/src/adapters) 的方向是正确的，但 `openai.rs`、`openai_responses.rs`、`anthropic.rs` 等文件同时包含 wire DTO、请求构造、响应解析、流式事件、usage 转换、错误分类和大量协议测试，导致修改一个协议细节时很难判断影响范围。

不推翻 provider adapter，但建议逐步改成以下内部结构：

- `wire.rs` / `request.rs`：只负责 provider 请求 DTO 和序列化；
- `response.rs` / `stream.rs`：只负责响应、SSE/JSONL 事件和 EOF flush；
- `mapping.rs`：把 provider payload 转成统一 `LlmResponse`、tool call、usage 和 finish reason；
- `features.rs`：集中声明 thinking、web search、cache、vision、audio 等能力及厂商差异；
- `tests/fixtures`：用脱敏的 golden request/response/stream fixture 做协议回归。

同时执行以下收敛：

1. transport、SSE framing、重试、超时和错误分类只能由已有公共管线负责；adapter 不再复制第二套 retry/stream policy。
2. `provider`、`api_style` 和 vendor 特判不再散落在各 adapter 的字符串判断中；adapter 对外声明 typed `ProviderCapabilities`，router 根据 capability/request policy 选择能力。
3. usage、finish reason、tool call identity、reasoning block 顺序等统一映射规则保留单一实现；provider 只补真正的 wire 差异。
4. 所有外部协议差异都必须有正向、空流、截断、错误、超时和未知字段测试；删除旧内部 alias 时不删除外部兼容行为。
5. 日志和诊断只记录 provider、model、status、retry reason 等非敏感元数据，禁止把完整 request、API key、prompt 或 cache key 带出 adapter。

这样做的收益是降低 provider 文件的修改半径、让新增能力可以复用统一接口，同时不承担一次性重写所有外部协议的风险。未来 `ModelRouter` 改成 capability/request policy 时，只需让 adapter 提供能力声明，不需要再次改写协议实现。

#### 2. Tauri DTO/event bridge：保留集中适配，去除组合根中的业务副作用

当前 [`crates/app-binary/src/events.rs`](../crates/app-binary/src/events.rs) 的集中映射点是正确的，但 `TauriEmitter` 除了 channel/DTO 转换，还承担标题缓存、secondary event、toast/Windows notification 和 chunk sequence 等行为。与此同时，前端的 live event 与 resume DTO 仍需在 [`ui/src/lib/resumeMessages.ts`](../ui/src/lib/resumeMessages.ts) 中手工合并。

不拆散 bridge，但建议逐步改成以下结构：

- `event_registry.rs`：唯一登记 event name、producer、consumer、顺序、幂等和敏感字段；
- `agent_wire.rs` / `session_wire.rs` / `action_wire.rs` / `recording_wire.rs`：按领域定义 DTO 和映射；
- `event_envelope.rs`：统一携带 `schema_version`、`sequence`、`session_id`、`run_id` 和必要的 correlation id；
- `event_emitter.rs`：只负责序列化和向 Tauri emit，不负责业务状态更新；
- `notification_projector.rs`：单独负责 toast、Windows 通知和安全的 display title。

重点改进如下：

1. 给 session 事件增加持久、单调的 `sequence`，让 UI 能从最后 sequence 继续接收，而不是依赖时间、内容或事件到达顺序猜测。
2. 保留当前 typed DTO 和 snake_case/camelCase 边界，但稳定业务字段不再默认使用 `serde_json::Value`；动态工具参数和 provider 原始 payload 才保留 `Value`。
3. 后端事件、数据库恢复事件和前端 `SessionReducer` 使用同一套语义；Tauri bridge 只做 transport adapter，不再让 `resumeMessages.ts` 继续承担第二套业务投影。
4. 流式 chunk 可以继续是高频临时事件，但必须带明确的 `message_id`、`run_id`、chunk sequence 和 reset boundary；终态事件必须能独立重放和幂等处理。
5. 将标题缓存、通知和 secondary event 从 DTO 映射函数中移出，避免 bridge 既改变 UI 状态又发送 UI 事件，造成不可测试的顺序依赖。
6. 每个命令/事件补充 contract test：字段、版本、顺序、重复投递、断线重放、未知字段和敏感字段泄漏均要覆盖。

这样做的收益是保留稳定的 Tauri 边界，同时让 bridge 变成可测试、无业务状态的适配层；未来引入 `SessionEventStore` 和前端 `SessionReducer` 时，不需要再推翻 Tauri 接入方式。

### 允许破坏性重构

以下四项可以删除当前实现、重建新模型；每项必须独立写 ADR、先建立行为/负向测试，再迁移一条完整调用链，最后删除旧实现和旧测试入口。

1. **安全授权：`SafetyGateway`**
   - 目标：用 typed `AuthorizationRequest` / `AuthorizationDecision` / capability scope 取代分散的字符串 permission key、旧 confirmation 字段和多入口猜测。
   - 必须保留：deny-first、永久/会话授权、路径和进程安全检查、TOCTOU 防护、scheduled/MCP/skill/Tauri 统一过闸。
   - 完成标志：所有副作用入口只有一个授权决策入口，前端不能通过 `confirmed` 或旧字段绕过它。

2. **输入与媒体：`InputPipeline` / `MediaGateway`**
   - 目标：将硬件采集、录音生命周期、转写、OCR/ASR、附件分析和媒体生成重划分为明确的 `MediaService` / capability job；输入层只拥有采集，provider 选择和 fallback 由媒体服务统一处理。
   - 可以删除：`provider == "llm"` 的双路径特判、重复 STT 路由、隐式 eager preprocessing 和不透明的媒体 fallback 组合。
   - 必须保留：录音取消/VAD 语义、原始附件可用性、低置信度降级、能力不可用时的可观察错误和 headless 测试能力。

3. **持久化与 Memory：`Database` facade / session projections / memory orchestration**
   - 目标：允许重做当前数据库 API、session event storage、投影事务和 Memory/Prompt/Inference 分层；优先目标是 `SessionEventStore` + domain stores，而不是继续扩大一个全能 `Database` facade。
   - SQLite 可以继续作为底层 adapter，但不再把 SQLite connection、cache invalidation 和 SQL repository 细节暴露给 agent/tools；如果未来替换数据库，也只替换 store adapter。
   - 必须保留：数据版本边界、可验证 migration/reset、事务一致性、敏感记忆过滤、embedding 生命周期和用户可见数据删除语义。

4. **Windows shell/process：`bg.rs` 与后台进程生命周期**
   - 目标：可以重做 command plan、process handle、输出流、取消/终止、超时和 action 状态机，最终与统一 `ActionService` 对接；不把“后台 shell”继续当成一套特殊的内存 registry。
   - 可以删除：当前 `BackgroundActions` 的内部状态组织、事件 sink/channel 交叉接线和与 scheduled action 分离的运行时模型。
   - 必须保留：`CREATE_NO_WINDOW`、PowerShell 编码、GBK/CLIXML 解码、输出上限、进程树终止、取消竞态、超时未知终态和 Windows 负向测试。

### 后续任务拆分建议

不要把四项放在一个“大重写”任务中。建议拆成以下独立任务，并在每项结束时删除旧链路：

1. `refactor(security)`: `SafetyGateway` typed authorization model；
2. `refactor(store)`: `SessionEventStore` / domain store / Memory boundaries；
3. `refactor(media)`: `MediaService` / input artifact / capability jobs；
4. `refactor(process)`: Windows process runtime / output pipeline / `ActionService` integration。

其中 store 任务会影响会话、Memory、action 和 UI resume，必须先定义事件及事务契约；media 可以相对独立推进；process 任务必须与 action state machine 一起验收。provider adapter 和 Tauri bridge 只作为稳定适配边界被新实现调用，不纳入上述四项的整体替换。

## 2.5 反向审计补充：不能只按文件拆分的四项（2026-09-02）

在上述候选之外，进一步检查配置更新、跨 session 通信、模型可见管理工具和 builtin tool contract 后，又发现四项不应被遗漏的重构方向。它们不是“还有几个大文件要拆”的重复清单，而是当前实现中仍然存在的跨域状态、过宽权限面和弱类型契约问题。前三项可以独立立项；第四项与第 2.3 节 E 的 `ToolsManager` 重构强相关，但仍应作为可单独验收的子任务记录。

这四项同样遵循本测试版本的破坏性重构原则：先写 ADR 和行为/负向测试，再迁移完整调用链，最后删除旧路径；不为保留旧内部调用方式而长期留下第二套语义。

### L. P1：把配置更新重做为 `ConfigService` 与版本化运行时快照

当前 [`crates/common/src/config/loader.rs`](../crates/common/src/config/loader.rs) 的 `ConfigLoader` 同时承担配置模型、TOML 读写、密钥保留和设置合并；而 [`crates/app-binary/src/commands/settings.rs`](../crates/app-binary/src/commands/settings.rs) 的 `update_settings` 保存后，又分别更新 LLM router、STT/media、Tools、Agent、MCP、SafetyGateway、日志和 hotkey。`hot_swap_router` 还在 [`crates/app-binary/src/commands/mod.rs`](../crates/app-binary/src/commands/mod.rs) 中单独重建 router 相关运行时。

当前代码已经需要在保存前重新从磁盘加载 MCP、skills 和 tool settings，以防 settings form 的不完整 payload 覆盖专用命令刚写入的内容。这些保护测试是必要的，但也说明配置权威和运行时应用逻辑已经分散：一次变更可能出现磁盘已写入、部分服务已替换、后续服务应用失败的半完成状态。

建议重做为单一的配置应用服务：

```text
ConfigService
 ├── versioned RuntimeConfig snapshot
 ├── typed ConfigPatch / validation
 ├── atomic persistence
 ├── derived RuntimeApplyPlan
 ├── apply / rollback / restart-required result
 └── ConfigChanged(version, diff)
```

目标和边界：

- `ConfigService` 是配置快照、版本和持久化的唯一权威；其他服务只消费不可变快照或明确的 typed patch，不再各自读写 `ConfigLoader`。
- 将“配置修改”和“运行时应用”建模成一个可观测的事务或阶段化计划：先校验和写入，再按依赖顺序应用；失败时返回具体阶段、回滚结果或 `restart_required`，不能只留下半更新的运行时。
- 用 typed patch 替代稳定配置路径上的任意 dotted JSON `set_value_at`；动态扩展点仍可保留 `serde_json::Value`，但必须有明确的 allowlist、版本和校验器。
- 通过 `ConfigChanged { version, diff }` 通知 router、MCP、skills、Tools、media、日志和 hotkey；每个消费者声明是否支持热应用、是否需要重建以及失败后的降级语义。
- 配置文件损坏、并发写入、密钥脱敏、专用管理命令与 settings form 并发修改必须有正向、冲突和恢复测试。

收益是把“配置已保存但运行时状态不一致”从约定变成结构上不可忽略的状态；同时也为第 2.3 节 I 的 `ApplicationRuntime` 提供唯一的运行时重配置入口。若暂时不做，至少不要继续向多个 command 和 `self` tool 增加新的配置 setter。

### M. P1：把 inbox 和多 Agent 协作重做为 `MessagingService`

当前 [`crates/tools/src/inbox.rs`](../crates/tools/src/inbox.rs) 是一个文件型 JSONL 消息总线，里面同时处理 agent 注册、mailbox、archive、锁、过期锁、processing 状态和读取语义；`read_and_archive` 还是旧的同步消费路径，`claim_and_archive` / `ack_claimed` 则是 ReAct 路径。[`crates/tools/src/builtin/messaging.rs`](../crates/tools/src/builtin/messaging.rs) 又把 list/send/inbox/reply/profile/request/spawn 等操作集中在一个多操作工具中，并通过 callback 把 agent spawn 反向接回 app/agent 层。

这使消息的投递、认领、确认、重试、重复消费和 Agent 生命周期分散在文件锁、工具 dispatcher、callback 和 session 逻辑中。它已经不只是“把 inbox.rs 拆成几个模块”的问题，而是 crash recovery、幂等和请求生命周期没有一个权威模型。

建议建立：

```text
MessagingService
 ├── typed Envelope
 ├── durable message identity
 ├── claim / ack / retry / expiry
 ├── request / reply / receipt lifecycle
 ├── in-process SessionActor mailbox
 └── file transport adapter（仅在确需跨进程时保留）
```

目标和边界：

- 统一 `send → claim → process → ack` 语义；删除旧的同步 `read_and_archive` 与 ReAct 专用消费路径之间的双轨行为。
- 每条消息有稳定的 message identity、sender、recipient、session、correlation、attempt 和 delivery state，重复投递必须可检测且不会重复执行不可幂等副作用。
- session 内部通信优先走 `SessionActor` mailbox；如果仍需跨进程或外部工具互操作，可以保留 JSONL 作为 transport adapter，但不能让 wire format 同时承担内部状态机。
- `spawn`、request、reply、receipt 统一走请求生命周期和 supervisor/actor port，不再通过可变 callback slot 隐藏 agent 依赖。
- 锁超时、进程崩溃、服务重启、收件箱积压、目标 session 消失和低信任上下文注入必须有恢复、重试和负向测试。

收益是让跨 session 协作从“文件队列加若干工具操作”变成可恢复的领域服务，能与第 2.3 节 A 的 `SessionActor`、第 2.3 节 D 的 `ActionService` 形成清晰的命令和事件边界。若应用最终只在单进程运行，可以减少文件总线；若确实需要跨进程，则只保留文件传输适配，不保留重复的内部消费模型。

2026-09-02 已完成第一条迁移切片：`MessagingService` / `MessageTransport` 成为应用层入口，
`InboxBus` 收窄为 JSONL transport adapter；`agent` 工具、ReAct inbox、peer lifecycle 均使用
`send → claim → process → complete`，并记录稳定 message id 与 `delivery_attempt`。完整
`SessionActor` mailbox、supervisor port 以及 spawn callback 的删除仍留在后续阶段；本切片不保留
运行时 `read_and_archive` 兼容路径。

### N. P1：拆掉 `self` 超级管理工具，重建受限的 Admin Surface

当前 [`crates/tools/src/builtin/self_tool.rs`](../crates/tools/src/builtin/self_tool.rs) 同时提供状态、config get/set、skills、tools、MCP、logs、sessions/errors 等管理能力。`SelfToolContext` 还直接持有 config loader、数据库、router、日志回调和弱引用的 `ToolsManager`，因此模型可见的一个 `self` 工具实际覆盖了多个服务的读写入口。

其中通用 `config_set(path, value)` 尤其容易把稳定配置契约退化为字符串路径和任意 JSON；MCP/skills/tool/log 操作又各自拥有持久化和 live apply 逻辑。即便把 handler 机械拆到多个文件，权限面和副作用边界仍然没有改善。

建议重建为窄而明确的管理面：

```text
DiagnosticsService   （只读状态、日志摘要、错误和 session 诊断）
ConfigAdmin          （typed patch，统一经过 ConfigService）
SkillAdmin           （技能生命周期和 allowlist）
McpAdmin             （MCP 配置、连接和健康状态）
SessionDiagnostics   （只读历史/运行信息）
```

目标和边界：

- 读操作和写操作分离；默认模型工具目录只暴露必要的窄工具，不能让一个 dispatcher 获得整个应用的管理权限。
- 删除普通模型路径上的任意 `config_set`，改成 allowlisted typed admin commands；高风险变更统一经过 `SafetyGateway` 和 `ConfigService`。
- 将 MCP、skills、日志和 session 诊断的持久化/运行时变更交还给各自 domain service，admin surface 只负责鉴权、调用和结果整形。
- 每个管理操作必须声明 capability、风险等级、是否可在 session 内执行、是否需要用户确认和是否允许重试。
- 保留诊断能力，但敏感配置、API key、完整 prompt、完整命令输出和隐私内容不得进入工具结果或普通日志。

收益是把“模型管理应用自身”的能力从一个高耦合、高权限工具变成可审计的 capability surface；也能让第 2.3 节 E 的 ToolsManager 收回 service locator 职责，并让第 2.4 节的 `SafetyGateway` 成为所有管理副作用的统一入口。机械拆分 `self_tool.rs` 可以作为过渡，但最终完成标准不是“dispatcher 还在，只是 handler 分文件”，而是旧超级工具和任意配置写入口被删除。

2026-09-02 已完成第一条受限 surface 切片：模型目录改为六个 capability-scoped
工具，旧 broad `haven` 不再注册；任意 dotted `config_set` 已删除，skills/tool/MCP/log
配置写入统一使用 `ConfigService::apply_patch`。日志行和 session/error 诊断已增加
脱敏与内容边界。native `SelfTool` structured entry 仍暂时供 Tauri commands 共用，
明确作为迁移期边界，待 `TypedToolOperation` 与 domain admin service 完成后删除。

### O. P1/P2：把多操作工具改成 typed `ToolOperation` 契约

当前 files、memory、system、audio、messaging、self 等 builtin 大量采用：

```text
Tool + operation: String + serde_json::Value
```

这种形式短期内方便把多个操作挂在一个模型工具名下，但会把参数校验、权限判断、风险等级、幂等性、错误语义、UI tool card 和测试都变成 `operation` 字符串分支。结果是工具表面看似稳定，内部却仍然有许多未类型化的隐式契约。

建议把运行时内部契约改成：

```text
TypedToolOperation
 ├── typed args
 ├── typed output
 ├── capability / scope
 ├── risk level
 ├── idempotency policy
 └── cancellation / timeout policy
```

目标和边界：

- 每一个能力操作都拥有明确的 args、output、错误类型和 metadata；dispatcher 负责选择 operation，不负责解释一大串 JSON 分支。
- `ToolRegistry` 可以继续按领域把多个 operation 分组成少量 LLM-facing tools，避免模型工具数量失控；但授权、执行、重试和 UI contract 必须解析 typed operation，而不是裸字符串。
- `serde_json::Value` 只保留在 provider 原始载荷、真正动态的 MCP 扩展点或明确声明的扩展边界；稳定业务参数默认使用 Rust 类型。
- capability、risk、idempotency 和 side-effect scope 与 operation 一起注册，使 `SafetyGateway`、ActionService、审计日志和 UI 能复用同一份 metadata。
- 每个 operation 都要有成功、缺参、错误类型、取消、超时、重复调用、越权和未知字段测试；删除旧 operation alias 和旧 dispatcher 分支后才算完成。

这项不是要求把每一个 operation 都暴露成独立的 provider tool，而是要求“模型分组”和“运行时契约”分层。它应作为第 2.3 节 E `ToolsManager/tool-core` 重构的独立子任务；收益是让工具授权和行为契约按 capability 组织，而不是继续按字符串和调用方约定组织。

2026-09-02 已完成第一条端到端 typed 切片：haven_config 的
config_get/logs_level 使用 TypedToolOperation，其 args/output/error、
capability/scope/risk/idempotency/cancellation/timeout/concurrency metadata
来自同一 operation；provider JSON 只在 adapter 边界转换。剩余 admin capability
仍通过临时 SelfTool facade，必须在后续 domain 切片中迁移并删除，不能把
TypedToolAdapter 退化成新的万能 dispatcher。

### 补充四项的任务拆分建议

本节四项不要和原来的四项合并为一个大重写；建议分别建立以下任务：

1. `refactor(config)`: `ConfigService`、版本化 runtime snapshot、typed patch 和 apply plan；
2. `refactor(messaging)`: `MessagingService`、Envelope、claim/ack/retry 和 SessionActor mailbox；
3. `refactor(self-admin)`: Diagnostics/Config/Skill/MCP admin surface，删除超级 `self` dispatcher；
4. `refactor(tool-contract)`: typed `ToolOperation`、capability metadata 和 operation contract tests。

建议依赖顺序为：先定义 `ConfigService` 和 `ToolOperation` 的边界，再接入 `SafetyGateway`；`MessagingService` 在 `SessionActor`/`SessionSupervisor` 的 mailbox 方向确定后迁移；`self-admin` 最后迁移，因为它同时依赖配置、工具注册、MCP、skills、诊断和安全授权。`tool-contract` 可以与 `ToolsManager` 并行设计，但必须在 `self-admin` 完成前提供新的管理操作注册方式。

这四项与第 2.3 节已有候选的关系如下：`ConfigService` 为 `ApplicationRuntime` 提供运行时配置入口；`MessagingService` 接入 `SessionActor`；`self-admin` 收窄 `ToolsManager` 的管理面；`ToolOperation` 是 `ToolsManager`/`SafetyGateway` 的 typed 执行契约。它们不是重复计数，而是补齐原有目标架构中配置、通信、管理和工具协议四个横切边界。

## 3. 执行顺序

### 阶段 A：先拆测试集中文件，低风险

目标：[crates/agent/src/integration_tests.rs](../crates/agent/src/integration_tests.rs)

- 规模：约 5,185 行，全部是测试。
- 按职责拆成多个测试模块，建议至少分为：
  - 生命周期、消息持久化与 resume/rollback
  - 工具参数验证与 confirmation 恢复
  - ReAct 核心循环、steering、ask、暂停/恢复
  - 工具批次、并发、取消和终态投影
- 把共享 mock、数据库夹具、emitter 和测试工具集中到一个 `support` 模块，避免复制。
- 保留当前测试的可见性和测试名称；不要因为移动文件而删除覆盖场景。
- 这是优先级最高、行为风险最低的一步。

验收：

```powershell
cargo test --locked -p haven-agent
```

2026-09-02 已完成阶段 A：`integration_tests.rs` 保留测试入口，83 个集成测试按
生命周期、resume/rollback、canonical、工具参数验证、ReAct、工具批次拆入独立模块，
共享 mock、emitter、数据库夹具和测试工具集中在 `integration_tests/support.rs`。
这次只改变测试文件布局，不新增 ADR，也不改变生产行为或测试覆盖范围。

### 阶段 B：拆 MCP 单文件实现

目标：[crates/mcp/src/lib.rs](../crates/mcp/src/lib.rs)

- 规模：约 2,241 行，其中约 1,891 行是生产代码。
- 当前混合了四类职责：
  - MCP/JSON-RPC DTO、请求构造和 content block 提取
  - stdio 与 Streamable HTTP transport、SSE 读取和进程启动
  - 单服务器 `McpClient`、限流、重连和健康监控
  - 多服务器 `McpManager`、配置 reconcile 和 `McpToolCaller` 适配
- 建议拆为 `protocol.rs`、`transport.rs`、`client.rs`、`manager.rs`；已有的 `sse.rs` 继续保留。
- `lib.rs` 最终只保留模块声明和真正需要的公共导出。workspace 内部调用方应迁移到新模块；如果阶段性保留旧导出，必须遵守 §2.1 并在本阶段末删除，除非它确实是外部稳定 API。
- 不改变 MCP wire shape、`Mcp-Session-Id`、stdio 进程回收、健康监控、限流或二进制 payload 上限。

验收：

```powershell
cargo test --locked -p haven-mcp
cargo test --locked -p haven-tools --test mcp_integration
```

2026-09-02 已完成阶段 B：`haven-mcp/src/lib.rs` 收窄为模块声明和公共导出，
协议/内容归一化、stdio/HTTP/SSE transport、单服务器 client、限流/重连/健康监控、
多服务器 manager/caller 及测试分别迁入 `protocol.rs`、`transport.rs`、`client.rs`、
`manager.rs`、`tests.rs`；保留既有 `sse.rs`。MCP wire shape、session header、进程回收、
取消/超时和 payload 上限未变，无临时 facade。

### 阶段 C：拆后台任务与 shell 辅助模块

目标：[crates/tools/src/bg.rs](../crates/tools/src/bg.rs)

- 规模：约 2,658 行，其中约 1,682 行是生产代码。
- 建议按以下边界拆分：
  - `shell_runtime.rs`：shell 命令构造、PowerShell 编码、代理探测、输出日志路径
  - `background_actions.rs`：`BackgroundActions`、状态机、action registry、事件 sink
  - `output.rs`：输出收集、UTF-8/GBK 处理、CLIXML/ANSI 清洗、错误摘要和 Windows 诊断
  - 必要时再把进程树终止和 live tail 读取放到 `process.rs`
- `bg.rs` facade 只允许作为临时迁移措施，不是目标架构。优先在同一轮中直接迁移所有 `crate::bg::*` 调用点并删除它；只有在拆分过程中确实需要分步编译时，才短暂保留 `bg.rs` 的 `pub use`。
- 如果暂时保留 `bg.rs`，必须在该提交/ADR 中写明删除条件；不得新增对 facade 的调用，阶段完成前应再次搜索调用点并删除 facade。不能以“兼容性”作为长期保留理由。
- 不改变 `CREATE_NO_WINDOW`、PowerShell `-EncodedCommand`、输出容量上限、日志落盘、取消和进程树终止语义。
- Windows 专属路径必须继续保留对应的条件编译和负向测试。

验收：

```powershell
cargo test --locked -p haven-tools
cargo clippy --workspace --locked -- -D warnings
```

2026-09-02 已完成阶段 C：shell runtime、background actions、output 清洗和进程/流处理
分别迁入 `shell_runtime.rs`、`background_actions.rs`、`output.rs`、`process.rs`，原有
测试按职责拆分且数量保持不变。workspace 内调用点已全部离开 `crate::bg`；`bg.rs`
仅保留旧公共路径的薄 re-export facade，待下游消费者迁移后删除，不再新增调用。

### 阶段 D：拆 Tool contract、registry 和安全网关

目标：[`crates/tools/src/tool_contract.rs`](../crates/tools/src/tool_contract.rs)、
[`crates/tools/src/registry.rs`](../crates/tools/src/registry.rs)、
[`crates/tools/src/security.rs`](../crates/tools/src/security.rs)

- 规模：约 2,237 行，其中约 1,287 行是生产代码。
- 当前混合了：
  - `Tool`、`ToolResult`、`ToolSignals`、`ToolExecutionOutcome`、重试/并发契约
  - `ToolRegistry` 和 session catalog
  - `SafetyGateway`、权限继承、disabled operation、路径沙箱和 reparse point 检查
- 建议拆为 `tool_contract.rs`、`registry.rs`、`security.rs`；workspace 内部调用方直接迁移到新模块。只有确实属于外部稳定 API 的导出才保留，不能为旧内部路径长期维护薄 facade。
- 安全模块拆分时必须先建立目标接口，再迁移完整调用链；不能把安全检查复制到各 builtin。
- 不改变 deny 优先级、权限继承、路径规范化、UNC/device path 拒绝、超时未知终态和操作幂等性语义。
- `LOCAL_TOOL_SECURITY_MATRIX` 应继续只有一个权威来源，并保留安全回归测试。

2026-09-02 已完成阶段 D：`tool_contract.rs` 收口 Tool/ToolResult、typed
`ToolOperation`、重试/并发/取消/超时契约和注册声明；`registry.rs` 收口全局注册表、
`SessionCatalog`、版本快照和 `RegistryProbe`；`security.rs` 收口 `SafetyGateway`、权限继承、disabled
operation、路径沙箱及 UNC/device/reparse-point fail-closed 检查。workspace 调用点已
直接迁移到新模块，删除旧 `tool.rs`，不保留内部路径 facade；`LOCAL_TOOL_SECURITY_MATRIX`
仍只有 `security.rs` 一个权威来源。原有 contract、registry、安全拒绝/权限继承、路径
安全/TOCTOU、timeout unknown/idempotency 和 typed metadata 测试全部按模块迁移，测试数量
保持不变。

验收：

```powershell
cargo test --locked -p haven-tools
cargo test --locked -p haven-agent
cargo clippy --workspace --locked -- -D warnings
```

### 阶段 E：收窄 app-binary 组合根

目标：[crates/app-binary/src/lib.rs](../crates/app-binary/src/lib.rs)

- 规模：约 1,925 行，其中约 1,368 行是生产代码。
- 当前混合了：
  - `TauriEmitter` 与 AgentEvent → IPC payload/channel 映射
  - `HavenShellHandler`、`HavenInputHandler` 宿主适配
  - Tauri 启动、托盘、全局快捷键、单实例和后台初始化
  - shortcut/tray 等辅助函数
- 建议抽出 `event_bridge.rs`、`handlers.rs`、`bootstrap.rs`；`lib.rs` 只作为组合根和 `run()` 入口。
- 事件映射必须保持单一登记点；每个 wire DTO 的 snake_case/camelCase 边界不能被拆散。
- 不改变启动顺序、后台初始化、托盘唤醒、快捷键录音生命周期、通知双通道和 session/action 事件形状。

验收：

```powershell
cargo test --locked -p haven-app-binary
cargo check --workspace --locked
```

2026-09-02 已完成阶段 E：`haven-app-binary/src/lib.rs` 收窄为模块声明、移动端
入口和必要导出；`event_bridge.rs` 收口 `TauriEmitter`、AgentEvent 到 Tauri
wire payload/channel 的唯一映射及 action 生命周期投影，`handlers.rs` 收口
`HavenShellHandler`、`HavenInputHandler` 与托盘图标适配，`bootstrap.rs` 收口
Tauri 启动、后台初始化、托盘、全局快捷键、单实例和退出编排。快捷键转换仍
通过 `lib.rs` 的必要 crate 内导出供 settings command 使用，未新增兼容 facade；
原有启动顺序、事件 shape、通知双通道和 105 个 app-binary 测试保持不变。

### 阶段 F：UI 视图拆分

这些目标应在 Rust 热点完成并稳定后处理。

#### Settings

目标：[ui/src/lib/views/SettingsView.svelte](../ui/src/lib/views/SettingsView.svelte)

- 规模：约 1,705 行，script 部分约 1,041 行。
- 建议拆为设置页外壳/离开保存流程、General 设置、Limits 设置；模型和媒体设置继续由已有 `ModelSettings.svelte` 承担。
- 配置 snapshot、dirty 检测、远端默认模型 reconcile 和保存流程应集中在一个明确的状态边界，不要在多个组件双写。

#### Model settings

目标：[ui/src/lib/views/ModelSettings.svelte](../ui/src/lib/views/ModelSettings.svelte)

- 规模：约 1,480 行。
- 将 provider/model role 配置与 STT/OCR/TTS/image generation 媒体配置拆成两个视图或子组件。
- 保持模型发现、api style、key 状态、默认模型同步和能力灰显行为不变。

#### Memory

目标：[ui/src/lib/views/MemoryView.svelte](../ui/src/lib/views/MemoryView.svelte)

- 规模：约 1,293 行。
- 按现有 tab 拆为 session history、long-term facts、memory recall 三个子视图。
- 保持分页/搜索/删除/导出、事实来源筛选、resume，以及 session message/usage store 的单一写入路径。

UI 验收：

```powershell
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

2026-09-03 已完成阶段 F：`SettingsView.svelte` 收窄为 settings tab、唯一的
snapshot/dirty/save/leave 状态边界和日志对话框；General 与 Limits 展示分别迁移到
`SettingsGeneral.svelte`、`SettingsLimits.svelte`。`ModelSettings.svelte` 保留
provider/model role、模型发现和 provider 编辑，STT/OCR/TTS/文生图及媒体 key 对话框
迁移到 `MediaSettings.svelte`。`MemoryView.svelte` 保留 session/fact/recall 的 IPC、
事件监听、resume、消息/用量 store 与 tab 编排，三个 tab 的展示分别迁移到
`SessionHistory.svelte`、`LongTermFacts.svelte`、`MemoryRecall.svelte`。

拆分没有新增 Tauri 命令、事件监听或 wire shape；子组件通过共享的 `$props()` 对象和
显式回调编辑状态，保存 payload 仍只从 `SettingsView` 生成，记忆数据仍只由
`MemoryView` 写入 store。旧的大视图分支已删除。剩余边界是 `ModelSettings` 仍同时承载
provider discovery 与 provider CRUD（两者共享同一模型缓存和引用迁移，后续如继续拆
应先补组件测试），以及 settings 的持久化状态仍集中在父视图，这是为保持离开保存守卫
单一来源而有意保留的边界。

### 阶段 G：低优先级复杂操作文件

目标：[crates/tools/src/builtin/self_tool.rs](../crates/tools/src/builtin/self_tool.rs)

- 规模：约 2,787 行，其中约 1,420 行是生产代码。
- `SelfOperation` 同时覆盖 config、skills、tools、MCP、logs、sessions/errors。
- 如果只是执行本阶段的低风险文件拆分，可以先按 config/skills、MCP、diagnostics/history 拆 handler 模块；但保留 dispatcher 仅是过渡，不能作为最终架构。
- 真正的目标和删除条件见第 2.5 节 N：迁移到受限 admin surface 后删除超级 `self` dispatcher 和任意配置写入口。
- 这是高风险目标，必须先补齐每个 operation 的正向、错误和持久化测试，不要作为第一轮拆分。

## 4. 暂时不要做的事情

- 不把 `haven-agent`、`haven-tools`、`haven-llm` 直接拆成多个 crate。
- 不因为 `openai.rs`、`openai_responses.rs`、`anthropic.rs` 各约 2.5k 行就立即拆 provider crate；每个文件约一半是协议测试，先考虑把测试按 provider 移到独立测试模块。
- 不拆 `memory/src/repositories/facts.rs` 的生产 facade；它总计约 2,080 行，但生产代码约 507 行，图谱写入、查询和维护已经分别位于其他模块。
- 机械拆分阶段不修改 ReAct X12 写路径、`ReActSnapshot.events` 恢复权威、消息/步骤投影、rollback 双时钟或任何数据库 schema；进入第 2.3/2.4/2.5 的明确重构任务后，按对应 ADR 处理这些边界。
- 机械拆分阶段不借机修改 provider wire payload、工具重试、安全确认、IPC event shape 或 UI 交互；provider adapter、SafetyGateway、Tauri bridge、ConfigService 和 ToolOperation 的概念级调整必须在各自任务中单独验收。

## 5. 可选的 crate 级后续方向

如果完成上述文件拆分后仍需要降低 `haven-tools` 的跨域耦合，可以另立任务评估 `haven-tool-core`：

- 放置稳定的 `Tool`、`ToolResult`、`ToolExecutionOutcome`、`ToolConcurrency`、`ToolRegistry`、`SafetyGateway` 契约。
- `haven-tools` 保留 builtin、background action、MCP/skill adapter 和具体执行逻辑。
- 这是独立的 crate/API 重构，必须单独写 ADR、迁移调用方并跑完整 workspace 门禁；不要和本计划的文件拆分混在一个提交中。

## 6. 每个拆分目标的完成标准

1. 生产行为和测试行为不变；移动测试不能减少覆盖场景。
2. 新模块职责单一，原入口文件只保留真正需要的公共导出或组合编排；临时兼容 facade 不算完成，除非已记录删除条件和必要性。
3. 没有新增反向依赖、循环依赖、重复实现或第二个契约真源。
4. 相关 ADR/架构文档在确实改变边界时同步更新。
5. 至少运行目标 crate 的测试、workspace 编译和严格 Clippy；跨端目标额外运行 UI check/test/build。
6. 提交前运行 `git diff --cached --check`，精确暂存路径，并使用符合规范的 `refactor(...)` 提交。

## 7. 全部阶段完成后的门禁

```powershell
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

按项目规范，仍保留在约 800 行以上的文件必须在对应 ADR 或变更说明中写明保留理由，不能仅以“历史文件”作为理由。
