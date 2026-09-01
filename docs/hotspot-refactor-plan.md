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

### 阶段 D：拆 Tool contract、registry 和安全网关

目标：[crates/tools/src/tool.rs](../crates/tools/src/tool.rs)

- 规模：约 2,237 行，其中约 1,287 行是生产代码。
- 当前混合了：
  - `Tool`、`ToolResult`、`ToolSignals`、`ToolExecutionOutcome`、重试/并发契约
  - `ToolRegistry` 和 session catalog
  - `SafetyGateway`、权限继承、disabled operation、路径沙箱和 reparse point 检查
- 建议拆为 `tool_contract.rs`、`registry.rs`、`security.rs`；workspace 内部调用方直接迁移到新模块。只有确实属于外部稳定 API 的导出才保留，不能为旧内部路径长期维护薄 facade。
- 安全模块拆分时必须先建立目标接口，再迁移完整调用链；不能把安全检查复制到各 builtin。
- 不改变 deny 优先级、权限继承、路径规范化、UNC/device path 拒绝、超时未知终态和操作幂等性语义。
- `LOCAL_TOOL_SECURITY_MATRIX` 应继续只有一个权威来源，并保留安全回归测试。

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

### 阶段 G：低优先级复杂操作文件

目标：[crates/tools/src/builtin/self_tool.rs](../crates/tools/src/builtin/self_tool.rs)

- 规模：约 2,787 行，其中约 1,420 行是生产代码。
- `SelfOperation` 同时覆盖 config、skills、tools、MCP、logs、sessions/errors。
- 后续可按 config/skills、MCP、diagnostics/history 拆 handler 模块；保留一个 dispatcher。
- 这是高风险目标，必须先补齐每个 operation 的正向、错误和持久化测试，不要作为第一轮拆分。

## 4. 暂时不要做的事情

- 不把 `haven-agent`、`haven-tools`、`haven-llm` 直接拆成多个 crate。
- 不因为 `openai.rs`、`openai_responses.rs`、`anthropic.rs` 各约 2.5k 行就立即拆 provider crate；每个文件约一半是协议测试，先考虑把测试按 provider 移到独立测试模块。
- 不拆 `memory/src/repositories/facts.rs` 的生产 facade；它总计约 2,080 行，但生产代码约 507 行，图谱写入、查询和维护已经分别位于其他模块。
- 不修改 ReAct X12 写路径、`ReActSnapshot.events` 恢复权威、消息/步骤投影、rollback 双时钟或任何数据库 schema。
- 不借拆分机会修改 provider wire payload、工具重试、安全确认、IPC event shape 或 UI 交互。

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
