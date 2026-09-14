# Haven 架构与 crate 职责

> 版本: v1.1 | 日期: 2026-08-22
> 范围: `crates/` (Rust 后端, Tauri 2)
> 原则: **依赖单向、叶子优先**。上层 crate 只依赖下层，绝不反向依赖；共享数据与类型放叶子（`haven-common`），
> 组件职责按「谁拥有实现、谁只消费接口」划分。

---

## 1. 依赖图

```
                      ┌─────────────────────┐
                      │     haven-app-binary │   组合根 / 宿主边界（Tauri）
                      └──────────┬──────────┘
                                 │
        ┌─────────────────────┬──┴─────────────┬────────────┐
        ▼                     ▼                ▼            ▼
 ┌────────────┐        ┌────────────┐   ┌────────────┐  ┌────────────┐
 │ haven-agent│        │ haven-input │   │ haven-tools│  │ haven-mcp  │
 │ ReAct 编排 │        │ 输入采集/语音│   │ 工具执行   │  │ MCP 客户端 │
 └─────┬──────┘        └─────┬──────┘   └─────┬──────┘  └─────┬──────┘
       │          ┌──────────┘                │               │
       │          ▼                           │               │
       │   ┌────────────┐                     │               │
       │   │ haven-llm  │◄────────────────────┴───────────────┘
       │   │ 模型/媒体  │
       │   └─────┬──────┘
       │         ▼
       │   ┌────────────┐    ┌────────────┐    ┌────────────┐
       └──►│ haven-memory│──►│ haven-skills│◄──┘
           │ 持久化      │    │ 技能目录    │
           └─────┬──────┘    └────────────┘
                 ▼
          ┌────────────┐
          │ haven-common │  共享叶子：类型 / 配置 / 提示词 / 编码
          └────────────┘
```

实际依赖（见各 `Cargo.toml`）：

| crate | 依赖 | 说明 |
|---|---|---|
| `haven-common` | 无内部依赖 | 纯叶子，全 workspace 共享 |
| `haven-llm` | common | 只依赖共享层，不依赖任何业务 crate |
| `haven-memory` | common | 持久化（当前 SQLite schema、历史迁移、仓库） |
| `haven-skills` | common | 技能目录解析 |
| `haven-mcp` | common, llm | MCP 客户端 / 传输（媒体能力复用 LLM 协议） |
| `haven-tools` | common, memory, skills, mcp, llm, input | 工具注册表 + 各内置工具 |
| `haven-input` | common, llm | 录音 / VAD / STT 编排（**不实现 provider**） |
| `haven-agent` | common, llm, memory, tools | ReAct 循环 + 会话执行 |
| `haven-app-binary` | 以上全部 + tauri | 装配 + Tauri 命令 + 事件桥 |

> 依据 `crates/*/Cargo.toml` 实际 workspace 依赖整理。`haven-agent` 与 `haven-app-binary` 是最上层，
> 其余全部是它们的底层依赖。`haven-llm` 不允许被业务 crate 反向依赖。

`haven-mcp` 内部按职责分为 `protocol.rs`（MCP/JSON-RPC DTO 与内容归一化）、
`transport.rs`（stdio、Streamable HTTP、SSE 和进程边界）、`client.rs`（单服务器连接、
限流、重连与健康监控）和 `manager.rs`（多服务器 reconcile 与 LLM caller 适配）；
`lib.rs` 只保留模块声明和公共导出，`sse.rs` 保留为 SSE parser。

`haven-tools` 的工具核心按稳定边界分为 `tool_contract.rs`（Tool、ToolResult、typed
operation 与执行策略）、`registry.rs`（全局注册表、SessionCatalog、版本快照与 probe）和
`security.rs`（AuthorizationEngine、权限继承、disabled operation、路径沙箱与本机安全矩阵）；
`lib.rs` 只从这些模块重新导出 crate 公共 API，builtin 直接依赖对应模块。安全矩阵只有
`security.rs` 一个权威来源，SelfTool 的 ADR 0070/0071 迁移边界保持不变。

`document.rs` 是受管附件的本地派生边界：由 `media.extract` 对 PDF/DOCX/XLSX/PPTX
执行有资源上限的文本/表格抽取，输出带
`document_extract` provenance 的不可信内容；
不执行脚本、不解析外部实体、不向模型暴露宿主路径。抽取失败显式返回不可用结果，不能
把空文本当成成功；`files` 只负责文件系统边界并把 rich path 交给 `media`（ADR 0114、0129）。

`builtin/media.rs` 是 Agent 原生的媒体工具边界：模型通过显式 operation 选择统一的图片、
音频或文档能力。`inspect`、`describe`、`ocr`、`transcribe`、`extract` 使用 `asset_id`；
`record` 先产生受管音频资产，`play`、`speak`、`volume_*` 和 `mute_*` 访问本机音频设备；结果通过
compact `media` reference 回到工具 observation，后续调用可以复用同一个 asset id；持久化
附件仍使用 common 层的 `MediaInput`。窗口截图和录音
进入生成媒体根目录并登记 session lease，`files.read/summary` 对 rich path 也转交到该入口；
宿主路径和 base64 不进入模型工具契约（ADR 0123）。媒体派生调用的 usage 作为
`llm_usage.call_kind=media` 与 `call_kind=tool` 单独保留，不污染 Agent 主循环的累计
cache rate（ADR 0124）。原始附件的 MediaPlan 投影同时留下短的 asset→representation
notice，避免模型重复派生或猜测资产是否已经进入上下文（ADR 0129）。

媒体工具的公共契约与实现按职责拆分：`media.rs` 只保留 `MediaTool`、operation/schema、
安全元数据和统一调度；`media_reference.rs` 负责模态分类与模型媒体引用，
`media_asset.rs` 负责普通路径资产登记，`media_content.rs` 负责图片/音频/文档派生，
`media_generation.rs` 负责生成资产，`media_audio.rs` 集中负责统一媒体工具的音频分支和本机
音频设备适配（ADR 0133、0134、0136）。这些模块共同实现一个 `media` 聚合执行边界；模型
目录暴露 `media.inspect`、`media.describe`、`media.ocr`、`media.transcribe` 等点号 operation
view，不把聚合根作为模型入口。

Builtin 的模型目录统一按点号 operation view 暴露：例如 `files.read`、`files.outline`、
`files.summary`、`files.search`、`system.info`、`system.env.get`、`haven.config.config_get`
和 `media.inspect`。`files`、`system`、`haven`、`media` 以及其它聚合模块仍作为 native/Tauri
和内部执行边界；模型只接收对应的窄 schema。provider-facing surface 分层维护：提示词只常驻
第一层 family/root 摘要（例如 `system`、`agent`、`haven`），`tool_catalog` 按需提供第二层
root 和第三层 operation 的名称、描述与精确 schema；未选中的 builtin operation 保留在
host-owned deferred catalog，由 `load_builtin` 按 operation/root 原子加载到当前 session；
启用 Skill 只进入紧凑索引，由 `load_skill` 按名称加载为 session-scoped 的 `skill__...`；
MCP 服务器索引保持紧凑，仍由 `load_mcp` 按服务器加载并在 session catalog 中注册
（ADR 0127、0131、0137、0145、0148）。
模型可见 observation 对结构化结果优先保留错误、路径、hint 与续读游标；工具定义和失败结果
分别暴露静态/具体 retry safety，仓库会话的 shell/files 相对路径默认对齐 workspace root，
同时保留 Temp sandbox fallback（ADR 0128）。

CI 以 `scripts/check-crate-dependencies.ps1` 对此表执行内部 crate 依赖方向检查；新增或调整
跨 crate 依赖时，必须先更新本表与该检查，并记录 ADR。

---

## 2. 各 crate 职责

### 2.1 `haven-common` —— 共享叶子（数据与工具，无任何内部依赖）

- `config/`：TOML 配置 schema（`AppConfig` / `Settings` / 各子配置）+ `ConfigLoader` 文件边界；
  `ConfigService` 持有版本化 live snapshot、串行 typed patch、原子持久化和无密钥变更通知。
- `types.rs`：跨 crate 的规范类型 —— 实体 ID（`new_id` / newtype）、`CanonicalMessage` /
  `ContentPart` / `CanonicalToolCall`、`MessageAttachment`、`FollowUp`、`RiskLevel`、
  `HotkeyMode` / `ShellChoice` 等。
- `media.rs` / `media_detection.rs`：provider-neutral 的 `MediaAsset`、
  `MediaRepresentation`、能力画像、统一文件探测和纯 `MediaPlan` 计划器；只选择安全的
  raw/derived/managed 表示，不执行文件 I/O 或 provider 路由。文件/MIME 探测以
  `media_detection` 为唯一权威实现。
- `prompts.rs`：系统提示词与各专用 prompt 常量（含 `STT_SYSTEM_PROMPT`）。
- `encoding.rs` / `text.rs`：编码解码（UTF-8 → GBK 回退）、文本工具。

**判定标准**：凡被 ≥2 个 crate 共享、且不依赖任何业务逻辑的纯数据/纯函数，放这里。

### 2.2 `haven-llm` —— 模型与媒体能力的唯一实现方

- `adapters/`：按 **`api_style`（线协议）** 分发的 provider 适配与统一 `LlmClient` +
  `with_retry`。能力矩阵见 `adapters/capabilities.rs`：
  - `openai-chat` / `llama.cpp` → OpenAI Chat Completions；embedding 走 `/embeddings`
  - `openai-responses`（含 DeepSeek Responses thinking echo + `web_search`）；embedding 仍走 `/v1/embeddings`
  - `xai` → OpenAI chat + xAI Live Search `search_parameters`；embedding 走 `/embeddings`
  - `anthropic` → Messages API（可选 server `web_search_*`）；无 embedding
  - `gemini` → `generateContent`（可选 `google_search` grounding）；embedding 走 `batchEmbedContents`
  - `deepgram` / `assemblyai` → STT only
- 聊天页「联网搜索」为角色级 `off|auto|always`；仅
  `supports_builtin_web_search(api_style)` 为真时由对应适配器注入内置搜索工具，
  UI 对不支持的线协议灰显。
- 厂商扩展（DeepSeek `thinking` / Responses `reasoning.effort`、Kimi
  `thinking.type`+`keep` 等）挂在对应 adapter + provider/base_url/model 检测上，
  复用聊天页「思考强度」，不另开线协议。
- `router.rs`：`LlmRouter`，按 `EndpointRole`（small / default /
  image / audio / embedding）把请求路由到对应适配器。
- `request_pipeline.rs`：provider-neutral 的 `RequestPolicy`/`RetryPolicy`；
  为普通聊天、工具聊天、embedding 和流式端点尝试提供同一份重试预算快照与
  总超时执行语义。router 仍拥有熔断、限流和流式聚合，adapter 不实现第二套
  重试。
- `adapters/transport.rs`：所有 provider 共用的 reqwest client、代理/归因与
  认证头、HTTP 状态错误、流式响应头超时和健康检查；不解析 provider payload，
  也不拥有 router 的重试与路由状态（ADR 0024）。
- `adapters/stream.rs`：共享 SSE/JSON-lines framing、EOF flush 与空
  `StreamChunk` 基线；不解析 provider payload，也不拥有 HTTP 请求或重试状态
  （ADR 0025）。
- `adapters/embedding.rs`：OpenAI-compatible embedding 的 URL、请求体、响应
  排序/校验和 usage 转换；provider 选择 endpoint，transport 负责通用 HTTP
  错误（ADR 0026）。
- `adapters/web_search.rs`：内置 web search call 的 action 规范化、citation
  结果 DTO 和按 id 去重；provider 只捕获 wire 事件，Agent/UI 消费统一结果
  （ADR 0027）。
- `adapters/provider_features.rs`：vendor 检测、thinking/reasoning 映射、
  echo 判定及 reasoning 长度限制；Chat/Responses adapter 共享同一规则
  （ADR 0028）。
- `stt.rs` / `ocr.rs` / `tts.rs` / `image_gen.rs`：各专用客户端实现 + 统一分发入口
  （`build_stt_client` 等）。
- `media/`：provider-neutral 媒体原语——common 探测器类型、provider content parts、MediaPlan 投影和
  vision adapter。媒体理解、OCR/STT fallback、文档抽取和文生图由 `haven-tools` 的
  `builtin::media` 工具统一编排；图片/音频以内联 `ContentPart` 进入模型，普通文件落盘后
  以受管 `asset_id` 交给 `media`；视频只有在 capability profile 明确支持时才投影为
  `RawVideo`（当前 Gemini 支持，其它 adapter 显式返回不支持）。TTS 由
  `media.speak` 显式触发并在本机播放；Windows 设备适配仍隔离在
  `builtin/media_audio.rs::AudioRuntime`。
- `tts.rs` 的 TTS client 由 `haven-app-binary` 注入 `haven-tools`；它不是媒体工具的
  自动处理分支，因此用户文本不会因为关键词被隐式朗读。
- `registry.rs` / `stream_rules.rs`：模型注册表、流式规则（生产 router 默认启用 `code_block_abort`）。

**判定标准**：一切「与模型 / 云端 provider 打交道的实现」都在这里；其它 crate 只通过
`LlmRouter` / `*Client` trait 消费，不实现。

### 2.3 `haven-memory` —— 持久化与记忆存储

- `schema.rs`：唯一的当前 SQLite schema、FTS/embedding 维护对象、版本戳和
  初始化编排。数据库 schema 是严格的 reset contract，不在运行时承载历史迁移。
- `repositories/`：会话、消息、步骤、图谱、用量和任务的持久化读写；其中
  `fact_graph.rs` 集中负责 `memory_edges` 写入与图谱不变量，`fact_query.rs`
  负责事实读取、搜索/排序，`fact_maintenance.rs` 负责事实清理、衰减与矛盾
  扫描，`facts.rs` 负责事实类型、谓词策略和稳定 `Database` 外观。消息的
  `media_inputs` 是多模态 canonical 持久化投影；`attachments` 仅保留元数据兼容
  投影，并由受信 host 根目录重建历史预览。
- `embeddings.rs`：向量编码、相似度/ANN 查询和 embedding 存储操作。

schema 初始化不改变 X12：`messages` / `session_steps` 仍是投影，
`ReActSnapshot.events` 仍是恢复唯一权威；`UserInject` snapshot 只保存
`MediaInput` 元数据，reset 只替换持久化载体，不成为新的业务真源。

**判定标准**：只负责 SQLite 生命周期与记忆数据持久化；Agent 编排、LLM
provider 协议和 UI 展示逻辑不得进入本 crate。

Agent 的 `memory_index.rs` 是 embedding 编排边界：它负责 embedding provider 调用、
有界 catch-up、模型切换清理和 LSH 重建；向量行的 scope、敏感过滤、规范化与 keyword
融合由 `haven_memory::recall::MemoryRetriever` 统一负责。`InferenceEngine` 只编排
事实抽取/维护并使用这两个组件；事实抽取 outbox 以 `kv_store` marker 持久化，
不把 provider 网络调用下沉到 Memory；Memory 只
接收已获取的向量并执行同步数据库读取。事实维护的 SQL 清理与矛盾候选读取由
`fact_maintenance.rs` 负责；维护调度、LLM 仲裁、提案门禁与并发控制仍属于 Agent，
二者通过既有 `Database` 外观连接（ADR 0022、0063）。

### 2.4 `haven-input` —— 输入采集与语音生命周期

- `capture/`：CPAL 采集线程 + 环形缓冲 + 重采样。
- `vad.rs`：tract ONNX 语音活动检测（含常驻 worker 线程）。
- `lib.rs` 的 `InputPipeline`：录音状态机（start / stop / cancel）、VAD 判定 →
  自动停止、`transcribe()` 把 WAV 交给 `SttClient`。
- `hotkey.rs`：快捷键字符串解析为中性 `KeyCombo`（与平台解耦）。

**判定标准**：管「何时/怎么采」——录音生命周期、VAD、把音频交给 STT；**不实现**任何
provider（STT 客户端来自 `haven-llm`）。

### 2.5 `haven-agent` —— ReAct 编排与会话执行

- `react/`：ReAct 循环（`loop` / `turn` / `response_cycle` / `stream_step` / `tool_batch` / `tool_batch_execute` / `tool_batch_policy` / `tool_batch_plan` / `context` / `inject` / `turn_end` / `snapshot_io` / `retries` / `hooks` / `hook_policy` / `transcript` / `state` / `request_context`），按 Run → Turn → ToolBatch 分层；`ReActState` 统一持有当前 run 的 events、canonical 和 branch points，所有边界共享同一运行态。`loop` 只负责 run 预算与生命周期，`turn` 负责阶段编排，`response_cycle` 负责一次采样后的空响应/截断重试，`tool_batch_plan` 固化 assistant 调用顺序和跨层身份，`tool_batch_execute` 负责批次准入、并发执行、取消与按序提交，`tool_batch_policy` 负责失败分类与重试提示，`tool_batch` 负责工具执行原语、确认生命周期与结果状态。`RequestContext` 从 durable canonical 生成不可变的 provider 请求视图，统一承载 sanitize、retry nudge 和一次性重试指令，不反写 transcript；`context` 只收集有边界的上下文项，`inject` 只经 `apply_transcript` 投影，`turn_end` 负责最终事件与暂停边界，`hooks` 只定义扩展契约，`hook_policy` 装配生产副作用策略。
- 流式输出由 `stream_step` 产生，`event.rs` 用一个有序 chunk 队列归并 thought/reasoning；provider retry 通过 `agent:stream_reset` 标记新的输出代次，UI 只清理 live stream block，不修改 durable transcript。`streamAggregator` 只合并相邻且同身份的 chunk，保留交错输出顺序；最终 thought/reasoning 投影仍是丢 chunk 时的权威修复路径。
- **X12 持久化契约**：`apply_transcript` 是 events→投影的统一 writer；`messages`/`session_steps` 为物化投影（UI/抽取/rollback 读投影；LLM resume 读 events）。多模态输入在 ingress 接受 `MessageAttachment`，但事件/数据库 canonical 投影使用 `MediaAsset → MediaRepresentation → MediaPlan`，snapshot 不保存 inline bytes；OCR/STT 成功追加派生表示且保留 raw asset。
- **工具调用身份契约**：同一 assistant tool batch 内，`action_index` 是 provider 调用数组的零基稳定位置，`step_id` 是该调用的持久执行行/卡片身份，`tool_call_id` 是 provider 调用身份；`session_steps` 与 ReAct events 同步保存三者。确认恢复必须按完整身份关联，禁止按工具名、参数或 observation 文本猜测；缺失 snapshot 不再从步骤投影重建 ReAct transcript，旧数据按 reset 边界处理。
- **工具参数验证契约**：执行前只验证，不用 schema default、首个 enum 或类型占位符改写输入；无效参数以包含 `action_index`、工具名和验证明细的失败 observation 返回给模型，避免改变副作用语义。
- `session/`：`SessionExecutor` 门面 + `dispatcher` / `queues` / `status` / `tool_runner`（FIFO、信号量、steering/follow_up、confirm）。
- `layer.rs` + `ingress.rs` / `resume.rs` / `resume_support.rs`：对外入口与 resume 恢复；`resume_support` 只提供确定性的候选合并、悬空工具调用修复和运行时工具选择恢复。
- `canonical.rs`：发送前 `sanitize_canonical` 闸门。
- `inference.rs` / `memory_index.rs` / `prompt.rs` / `compactor.rs` / `rollback.rs` / `rollback_support.rs` / `title.rs` / `event.rs` / `partial.rs`；`memory_index` 只适配 embedding provider 与索引生命周期，`prompt` 通过 typed memory recall 组装 bounded MEMORY fence；`rollback.rs` 编排生命周期与 DB 双时钟，`rollback_support` 只操作 events 和 branch cursor。
- `fact_extraction.rs`：事实抽取 DTO、LLM 字段 coercion、标签/谓词规范化、prompt
  字段清洗和 JSON array 提取；`InferenceEngine` 负责调度与持久化（ADR 0029）。
- 调用 `LlmRouter`、执行 `haven-tools` 工具、写 `haven-memory`、
  通过 `AgentEvent` 对外发事件。

**步数预算（Phase 7/8 / J1）**：`session.max_steps` 是**单次 run**上限。pause / ask / confirm 后再次
resume 会按 `per_run_cap = max(max_steps, start_step - 1 + max_steps)` 再给满额。可选
`session.session_max_steps: Option<u32>`（默认 `None` = 不限）在绝对 `step_number` 上截断：
`effective_max = min(per_run_cap, session_max_steps)`。详见
`docs/stability-refactor-plan.md`。

**判定标准**：会话的业务编排中心，不知道也不关心 provider 细节 / 录音硬件细节。

### 2.5.1 多 Agent 协作（Plan A）

同一机器上多个 session 通过内置工具 `agent` + 文件总线协作；**不**单独做通讯 Tab（产品约束：协作过程以工具结果形式出现在对话页）。

```
Parent session                    Child session(s)
     │  agent operation=spawn          │
     │──────────────────────────────►  │  peer_kickoff（低信任 brief）
     │  agent operation=request        │
     │──────────────────────────────►  │  inbox auto-inject / reply
     │  ← reply (in_reply_to) + receipt│
```

| 层 | 位置 | 职责 |
|---|---|---|
| 工具 | `haven-tools` `builtin/messaging.rs` | 统一工具名 `agent`；`operation=` list / children / history / send / inbox / ack / reply / profile / request / spawn / status / join / wait / stop / collect；通过 `MessagingService` 调用 |
| 服务 | `haven-tools` `messaging_service.rs` | 唯一应用层消息 port：校验 Envelope identity、claim/complete/retry/expiry、request/reply selective wait 与 receipt 生命周期 |
| 传输 | `haven-tools` `inbox.rs` | JSONL file transport adapter：`%APPDATA%/haven/inbox` 的 registry / mailbox / archive / lock；不向应用暴露同步 drain 语义 |
| 编排 | `haven-agent` `layer::spawn_peer_session` | 先落库 `peer_kickoff` 并 inbox 注册 parent，再 Pending 调度；返回 `queued`（相对 `session.max_concurrent`） |
| 接线 | `haven-app-binary` `app_state` | 安装 `AgentSpawner` 回调（tools 不依赖 agent） |
| 运行时 | `react/context.rs` + `react/inject.rs` | `context` 负责每步 heartbeat、通知或每 3 步通过 `MessagingService::claim` poll inbox；每个 envelope 保留为独立上下文项，投影 durable 后由 `MessageClaim::complete` ack 并发 receipt；`inject` 经 `apply_transcript` 注入带消毒后的 `id`/`in_reply_to`/`subject`；`InjectSource::CrossSession` |
| 生命周期 | `session/status.rs` | 终端态/`end_session` → BFS 子孙 system notice + 无嵌套 cascade 结束；`type=system` 仅运行时 |
| 信任 / 记忆 | `inference.rs` | 跳过 `peer_kickoff` 与跨会话注入文本的 fact 抽取 |
| UI | 对话页 tool card | `agent` 结构化卡片；自动同伴邮件以 `agent`/`inbox`/`auto` 卡片展示；kickoff 左侧「低信任委托」 |

协议约定：同伴消息 ≠ 用户指令；`id` 是稳定的 `msg-{uuid32}`，`in_reply_to` 对齐 request id，
`delivery_attempt` 记录 at-least-once 重投次数；批量消息必须走 `send → claim → process → ack`。
显式 `agent.inbox` 默认只 claim 不 ack，处理完成后由 `agent.ack(message_ids|claim_token)` 确认；
`claim_token` 是进程内整批 receipt，崩溃后由 durable processing 状态触发 at-least-once 重投，
而不是丢失消息。`agent.history` 为只读恢复入口。`agent.status/join/wait/stop/collect` 只允许当前 session 或其后代，
并通过 `AgentController` 进入真实 `SessionExecutor` 状态机，`stop` 走正常取消与终端清理路径。
当前跨进程仍使用 JSONL adapter，未来可替换为 SessionActor mailbox；子会话默认工作目录仍为
Temp（全局约束）。

### 2.5.2 内置 `system` 工具（机器信息与系统控制）

统一实现：`haven-tools` 的 `builtin/system.rs`。`env` / `registry` / `power` 以及桌面能力仍可
在代码中由聚合实现承载，但模型目录统一暴露 `system.*`、`process.*`、`clipboard.*`、
`input.*`、`window.*` 点号 operation view；`files.*` 与 `media.*` 也遵循同一规则。聚合根
只保留给 native/Tauri 或内部路由，不作为模型可见入口。

| scope | 能力 | 风险 |
|---|---|---|
| `info`（默认） | 只读机器快照；`category=` 细分 | Safe |
| `env` | 环境变量 get/set/unset/list；`scope=process/user/machine`，list 可用 `name` 作前缀过滤 | get=Low；list/set/unset=High |
| `registry` | Windows 注册表 get/set/delete_value/delete_key/list；值删除要求 `name`，键删除为递归删除 | 读=Medium；值写/删=High；键删=Critical |
| `power` | 电源 status / lock / sleep / hibernate | status=Safe；lock/sleep=High；hibernate=Critical |
| `display` | 监视器几何 + DPI/缩放 + 刷新率 | Safe |
| `process` | 进程 list / kill | list=Low；kill=High |
| `clipboard` | 剪贴板 read / write / history | read/history=Low；write=Medium |
| `input` | 键鼠 type / key / click / move / scroll | move/scroll=Low；其它=Medium |
| `window` | 窗口 list / foreground / focus / close / screenshot / OCR / UI tree / observe / invoke / set_value / toggle / select / wait | 读/观察=Low；语义操作/focus=Medium；close/OCR=High |

`ActionService`（`haven-tools/src/action_service.rs`）是后台与定时任务的统一查询/状态/取消门面；
`BackgroundActions` 和 `ScheduledActionCenter` 当前仍是内部 worker，但 model-facing `actions.*` 和
app action board 都只读取规范化 task row。`InteractionRequest`（`haven-agent/src/interaction.rs`）
是 ask、confirm 和 scheduled confirm 的共同生命周期投影，快照保留旧字段用于兼容读取，新的交互状态以
`Pending → Resolved | Expired | Cancelled` 表达。

Clipboard 的文本、HTML、图片和文件列表都从 `clipboard` 根工具进入；图片/文件读取先复制到受管媒体
资产并只向模型返回 `asset_id`。`media.render` 复用有界文档表示管线按页返回结果，不新增独立的
`audio`、`file_search` 或 HTTP 搜索根工具。

`scope=info` 的 `category`：

| category | 内容 |
|---|---|
| `overview`（默认） | os + user（当前）+ locale + cpu + memory + disks + network_summary |
| `os` | 名称/版本/内核/发行版 id/主机名/架构/产品厂商与型号/uptime/boot |
| `cpu` | 品牌/厂商/频率/物理·逻辑核/总占用 + `per_core` |
| `memory` | total/used/available/free/swap + usage_pct |
| `disk` | 挂载点/文件系统/kind(HDD\|SSD)/容量/占用/可移动/只读 |
| `network` | 网卡名/MAC/IP/MTU/状态/累计收发（受 `max_output_chars` 截断） |
| `user` | 当前用户/域/计算机名/home/temp/cwd + 本机用户列表 |
| `locale` | 时区偏移/本地与 UTC 时间/系统 locale / UI 语言 |
| `all` | 上述全量（网络仍按预算截断） |

实现依赖：`sysinfo` + Windows `windows-sys`（Gdi / Globalization / Power）。电源寿命字段为秒（Win32 `SYSTEM_POWER_STATUS`）。

### 2.5.3 权限 / 确认（AuthorizationEngine）

决策顺序（fail-closed）：

1. `NetworkPolicy` / `SandboxMode` / `tool_settings.disabled_operations` / `allowed_paths` → **Blocked**
2. 操作契约的 `Required` 或 `Critical` → 保留强制确认底线
3. 永久拒绝（`SecurityConfig.permissions`，Always）→ **Blocked**
4. 会话拒绝 → **Blocked**
5. 永久允许 / 会话允许 → **AutoApproved**（但不能越过强制确认底线）
6. `PermissionMode`：`Plan`（仅显式只读操作）/ `Default`（编辑、命令和外部效果询问）/
   `AutoEdit`（安全编辑自动执行）/ `Autonomous`（High/Critical 仍询问）
7. 否则 → `RequiresConfirmation`（事件只带后端生成的安全摘要 + `permission_key`）

决策细化：永久拒绝 → 会话拒绝 → 永久允许 → 会话允许。拒绝授权写工具根键（覆盖同工具全部子操作）；允许写精确键。

权限键：`permission_key(tool, params)` → `tool` / `tool.operation` / `system.power.lock`；授予父键可覆盖子操作。旧的冒号聚合键只在匹配边界兼容，配置加载发现这类 operation key 时备份并按重置策略处理。
操作 view 的 `OperationPolicy.permission_key` 是权威身份；契约另外声明 `effect`、`data_sensitivity`、
`network_access` 和执行并发，不能由风险等级、并发属性或前端字段推断。`SecurityConfig` 另外保存
`sandbox_mode`（`read_only` / `workspace_write` / `full_access`，可选 `writable_roots`）与
`network_policy`（`deny` / `restricted` / `open`）；工作区可写模式拒绝无法约束的 opaque 子进程，
Windows 子进程通过 Job Object 回收进程树；受限网络只允许经过 SSRF/DNS 校验并固定地址的 HTTP/MCP
目的地，禁止跨源重定向。

确认 UI：拒绝 / 仅本次 / 本对话允许 / 始终允许；拒绝菜单含本对话拒绝、始终拒绝。永久授权写入 `config.toml`，设置页可按工具查看、撤销或一键清除。确认收据绑定规范化输入 hash、权限 key、策略 revision、风险和过期时间，执行前再次验证；原始 shell、网络、文件和扩展参数不进入 renderer。普通全量设置保存不拥有权限规则，避免 stale form 清空授权。

### 2.5.4 Admin Surface

模型看到 `haven.diagnostics.*`、`haven.config.*`、`haven.skills.*`、`haven.tools.*`、
`haven.mcp.*` 等点号 operation view，以及独立的 `actions.*`、`schedule.*`、
`preferences.*`、`checklist.*`。聚合器只负责内部路由，
每个 operation 继续复用子工具自己的严格 schema、风险等级、幂等性、并发资源和
session 归属；因此 `mcp_add` 是 High，而 `mcp_list` 是 Low，二者不会因共用根名
而被压平。

配置写入使用 `ConfigService::apply_patch` 的 typed patch；普通模型路径没有任意
`config_set(path, value)`。诊断结果只提供脱敏、截断后的日志和 session 元数据，不能
返回 API key、完整 prompt、完整命令输出或会话正文。原 `SelfTool` 仍只作为 native
Tauri command 的 structured surface，未直接注册进模型目录；`haven` view 通过受限 adapter
路由到同一实现。高风险、网络、媒体、文件和跨 session 协作仍保留独立的内部实现边界，
以维持各自的确认、路径、provider 和生命周期边界；模型看到的名称仍遵循点号 view 契约。

### 2.6 `haven-app-binary` —— 组合根 + 宿主边界（Tauri）

- `app_state.rs`：装配 `AppState`（db / router / tools / executor / agent / pipeline / shell /
  `config_service` / media clients / stt_client）。
- `config_runtime.rs`：根据 `ConfigChanged` 生成 runtime apply plan，区分 live consumer 和
  `restart_required` consumer；运行时编排留在组合根，不下沉到 `haven-common`。
- `commands/recording.rs`：host 校验并落盘上传附件、分配 `asset_id`，并由 app-binary
  在启动/每日维护时清理超过历史保留期的 `file-{uuid32}` 批次；模型工具不能触发这条
  清理路径（ADR 0115）。
- `event_bridge.rs`：`AgentEvent` → 前端 channel 和显式 wire DTO 映射，包含 action
  生命周期投影与通知副通道。
- `handlers.rs`：`ShellHandler` / `InputHandler` 的 Tauri、输入管线和托盘适配，包含
  录音生命周期、VAD、自动停止和托盘图标更新。
- `bootstrap.rs`：Tauri 启动、后台初始化、托盘、全局快捷键、单实例、自启、日志和退出
  编排；不承载领域逻辑。
- `lib.rs`：模块声明、移动端 `run()` 入口和必要的 crate 内导出。
- `commands/*`：全部 Tauri IPC 命令（recording / session / action / history·memory / model / mcp /
  skills / memory / settings / log）。
- `desktop.rs` / `events.rs` / `autostart.rs`。

**判定标准**：唯一能同时看到所有 crate 的地方；负责把事件桥到前端、把前端命令调到后端，
不承载业务逻辑。

---

## 3. 易混边界（历史演进遗留，现已收敛）

### 3.1 input 与 llm 都碰 STT

| | `haven-input` | `haven-llm` |
|---|---|---|
| 角色 | **消费方**：录音 → VAD → WAV → 调 `SttClient` | **实现方**：`LlmClient::transcribe` + `build_stt_client` / `adapter_for` |
| 复用点 | `InputPipeline::transcribe`（用户麦克风录音） | `haven-tools::builtin::media`（agent 的受管资产；工具内统一走 STT / 多模态 fallback） |

同一个 `SttClient` 被两处复用是**有意的共享**，不是职责重复：input 走「用户录音」路径，
工具层的 `media` 走「agent 资产」路径。云端 STT（Whisper / Groq / Gemini / Deepgram /
AssemblyAI）与 chat 共用 `adapter_for` 分发；`provider = "llm"` 走
`LlmRouter::transcribe_audio`（原生 `transcribe`，否则 multimodal chat 回退）。
MCP STT 仍走独立 `McpSttClient`（依赖 `McpToolCaller`）。两条录音路径的语义也保持显式
不同：UI 麦克风是 `voice input`，只提交转写文本并用 `rec-*` 关联事件；`media.record` 是
`recorded media asset`，先登记 WAV 并返回可复用的 `asset_id`，再附带转写结果。两者共享
STT 实现与事件规范，但不会隐式互相升级为另一种生命周期。

### 3.2 媒体编排归属

媒体理解与生成统一位于 `haven-tools::builtin::media`，通过 `asset_id`、工具 schema、权限和
tool usage 进入 ReAct。`haven-llm::media` 只保留 modality、provider content parts、
MediaPlan 投影和 vision 等 provider-neutral 原语；`haven-agent` ingress 只负责持久化原始输入。
本次破坏性收敛删除旧的 `MediaGateway` 与隐式 eager preprocessing，详见 ADR 0130。

工具的一次性图片理解由共享 `MediaTool` 编排，并调用
`LlmRouter::analyze_image` 这一 provider-wire adapter；后者只负责将已经读取的
bytes 规范化为一次 vision 请求，不负责 asset lookup、工具权限、生命周期或跨 provider
fallback。`media` 的音频同样由 `MediaTool` 统一处理专用 STT、超时、置信度和
`LlmRouter::transcribe_audio` fallback；媒体派生结果携带 canonical `MediaInput`，不再
以宿主路径作为跨工具引用（ADR 0122、0123、0130、0136）。`files` 只拥有路径安全、读写
和进入注册表的边界；分类与媒体 handoff 分别位于 `file_classification.rs` 和
`file_media_handoff.rs`，普通路径资产登记位于 `media_asset.rs`。`MediaTool` 的所有
asset/device 分支都通过 common 的 `MediaRepresentationKind` 和 `MediaResult` 外壳投影，
UI、Agent 与 provider 只在各自边界做场景适配。

### 3.3 agent 对 input 的依赖（2026-08-18 清理）

- **改前**：`agent → input` 的唯一理由是重导出历史输入类型（`session.rs`），agent 不调用任何
  input 能力，属于不必要的耦合。
- **改后**：`FollowUp` 下沉到 `haven_common::types`，`agent/src/session/mod.rs` 改为
  `pub use haven_common::types::FollowUp`，删除 `haven-input` 依赖与 `input/src/message.rs`。
  现在 `agent` 与 `input` 分层不互相依赖（都只依赖 common / llm）。

### 3.4 依赖方向的守则

- `agent` 不依赖 `app-binary`；provider 不依赖业务 crate；`input` 不依赖 `agent`。
- 两端组件（agent 与 input）的接缝（录音结果 → `process_transcript` → agent）统一在
  `app-binary` 编排，不通过 crate 依赖互相调用。

---

## 4. 相关文档

- `docs/conventions.md` —— 日志 / 错误 / 通知 / 命令返回规范
- `docs/naming.md` —— 各层命名与跨层 camelCase 边界
- `docs/development-standards.md` —— 架构、契约、安全、测试与变更治理规范
- `docs/ipc-contracts.md` —— Tauri 命令与事件的跨端 DTO 契约目录
- `docs/stability-refactor-plan.md` —— 稳定性与可维护性重构计划（测试版可破坏性变更）
- `docs/refactor-execution-guide.md` —— 重构实施顺序、阶段验收与执行模板

---

## 变更记录

| 日期 | 内容 |
|---|---|
| 2026-09-12 | §2.5 Tools / Agent / App：删除 `MediaGateway`、coverage、intent 与 ingress eager preprocessing；由单一共享 `MediaTool` 统一 OCR、STT fallback、文档抽取和显式媒体生成，并同步 UI 媒体结果契约（ADR 0130） |
| 2026-09-12 | §2.5 Tools / UI / Security：删除独立 `audio` 模型工具，将录音、播放、TTS、音量和静音纳入 `media` operation 分支；旧 audio 配置/权限按测试版策略重置（ADR 0133） |
| 2026-09-12 | §2.5 Tools：按公共契约、媒体引用、内容派生、生成/资产登记和测试职责拆分 `media` 内部模块；模型入口与运行时行为不变（ADR 0134） |
| 2026-09-12 | §2.5 Common / LLM / Tools / Agent / UI：统一媒体探测、视频 raw 表示、MediaResult 外壳和 STT MediaPlan 投影；区分 voice input 与 recorded media asset，并拆出文件分类/handoff 与路径资产模块（ADR 0136） |
| 2026-09-12 | §2.5 Agent / Tools / LLM：统一 producer→asset_id→media consumer；files rich path、window OCR、录音均收敛到同一资产链，并在模型请求中说明 MediaPlan 表示（ADR 0129） |
| 2026-09-12 | §2.5 Tools / Agent / Common：按实时路由能力裁剪媒体与录音 operation；structured-first observation 保留恢复字段；仓库会话默认工作区路径；区分只读重试安全性并补充 runtime capability snapshot（ADR 0128） |
| 2026-09-12 | §2.5 Tools / Agent：补充 model-facing schema 压缩、可恢复文件读取与 `files.outline`、能力过滤及显式 memory-empty 语义；保持聚合工具公共名称不变（ADR 0127） |
| 2026-09-12 | §2.5 Tools / LLM / Agent / UI：完成 P1 operation view、搜索/outline 结构化模型视图、文档页游标、原生视频 ContentPart、session-scoped 偏好/清单和 memory 空结果诊断；按测试版 reset 边界删除 FollowUp/confirmation/ask/rollback/provider-style 内部兼容层（ADR 0131） |
| 2026-09-13 | §2.5 Tools / Agent / UI / Security：模型与 UI 统一使用 `root.operation` 点号 view；files/system/haven/media 及其它 operation-based builtin 不再以聚合根注册，启用 Skill 直接注册，删除 `load_skill`（ADR 0137） |
| 2026-09-14 | §2.5 Tools / Agent：将 provider-facing 工具定义改为核心常驻 + builtin/Skill/MCP 按 session 分层加载；新增 `load_builtin` / `load_skill`，保留 `load_mcp`，完整 schema 只进入当前 session（ADR 0145） |
| 2026-09-14 | §2.5 Tools / Agent：提示词只保留 `system` / `agent` / `haven` 等第一层 family/root 摘要；新增 `tool_catalog` 提供分页的 family/root/operation 发现与精确 schema 查询（ADR 0148） |
| 2026-09-14 | §2.5 Tools / UI / MCP：工具页按 `ToolManifest.identity` 实现 family/root/operation 三级树；复核并统一 MCP 渐进连接的目录版本监听，保证 `tools/list_changed` 使分页 cursor 失效（ADR 0148） |
| 2026-09-13 | §2.6 UI：工具页将同一 operation root 收束为一张可展开卡片，保留每个 operation 的独立 Schema、风险和启用状态（ADR 0139） |
| 2026-09-10 | §2.5 Tools：将 PDF/DOCX/XLSX/PPTX 的受限本地抽取收口到 `document.rs`，经受管 `files` read 返回有 provenance 的不可信派生表示（ADR 0114） |
| 2026-09-02 | §2.5 Tools：将 Tool contract、registry/catalog 与 AuthorizationEngine 拆分为 `tool_contract.rs`、`registry.rs`、`security.rs`，直接迁移 workspace 调用点并保持安全/执行契约不变（阶段 D） |
| 2026-09-02 | §2.5 Tools：haven_config 完成首条 TypedToolOperation 切片，typed metadata 与 provider JSON adapter 分层；其余 admin facade 仍待迁移（ADR 0071） |
| 2026-08-22 | §2.4.1 多 Agent（Plan A）：`agent` 工具、InboxBus、spawn/cascade、低信任与 UI 展示 |
| 2026-08-18 | 初版；历史输入类型从 `haven-input` 下沉 `haven-common::types`，去除 `agent → input` 依赖 |
| 2026-08-20 | 曾增加 memory / react 改进文档（后续合并为已归档的 backlog） |
| 2026-08-21 | 删除 `memory-architecture.md` / `react-architecture-improvements.md` |
| 2026-08-26 | 用 `stability-refactor-plan.md` 取代历史 backlog，重构目标改为稳定性与可维护性 |
| 2026-08-27 | §2.3 Memory：将当前 schema 与历史迁移拆为独立模块，保持版本链和 X12 契约不变（ADR 0013） |
| 2026-08-29 | §2.3 Memory：将事实图谱写入与事实查询/维护分出内部 `FactGraph` 边界，保持 Database API 与 X12 契约不变（ADR 0019） |
| 2026-08-29 | §2.3 Memory：将事实读取、搜索/排序与维护策略分出内部 `fact_query.rs` 边界，保持 Database API 与持久化语义不变（ADR 0020） |
| 2026-08-29 | §2.3 Agent/Memory：将 embedding provider 调用、有限索引 catch-up、向量召回与 LSH 重建收口到 `memory_index.rs`，保持 Database API 与召回语义不变（ADR 0021） |
| 2026-08-29 | §2.3 Memory：将事实清理、衰减、来源规范化与矛盾候选扫描收口到 `fact_maintenance.rs`，保持 Database API 与维护语义不变（ADR 0022） |
| 2026-08-29 | §2.2 LLM：将聊天、工具、embedding 与流式端点尝试的重试/总超时策略收口到 `request_pipeline.rs`，保持 router 路由与 provider wire 契约不变（ADR 0023） |
| 2026-08-30 | §2.2 LLM：将 provider 共用 HTTP client、认证头、状态错误、流式 header 超时与健康检查收口到 `adapters/transport.rs`，保持 provider wire 契约不变（ADR 0024） |
| 2026-08-30 | §2.2 LLM：将 SSE/JSON-lines framing、EOF flush 与空 stream chunk 基线收口到 `adapters/stream.rs`，保持 provider wire 契约不变（ADR 0025） |
| 2026-08-30 | §2.2 LLM：将 OpenAI-compatible embedding 的请求/响应规范化收口到 `adapters/embedding.rs`，保持 provider wire 契约不变（ADR 0026） |
| 2026-08-30 | §2.2 LLM：将内置 web search call 规范化、citation 结果和按 id 去重收口到 `adapters/web_search.rs`，保持 Agent/UI 结果契约不变（ADR 0027） |
| 2026-08-30 | §2.2 LLM：将 vendor 检测、thinking/reasoning 映射、echo 判定与长度限制收口到 `adapters/provider_features.rs`，保持 Chat/Responses wire 契约不变（ADR 0028） |
| 2026-09-07 | §2.2 LLM：按官方协议规范化 Gemini、Anthropic、DeepSeek 与 Responses 的请求字段、thinking state、工具调用 ID 和流式终止事件（ADR 0094） |
| 2026-09-07 | §2.2 LLM：所有 OpenAI-compatible Chat/Responses 工具统一使用 object-root 投影；判别型 root union 通过 `dependentSchemas` 保留嵌套分支约束（ADR 0095） |
| 2026-08-30 | §2.4 Agent：将事实抽取 DTO、字段 coercion、标签/谓词规范化、prompt 清洗与 JSON array 提取收口到 `fact_extraction.rs`，保持抽取与持久化语义不变（ADR 0029） |
| 2026-08-30 | §2.6 UI：将会话消息 map、草稿/会话迁移、rollback 截断与流式 sequence 去重收口到 `ui/src/lib/sessionMessages.ts`（ADR 0030） |
| 2026-08-30 | §2.6 UI：将会话 token usage、LLM 调用明细、恢复/清理与用量格式化收口到 `ui/src/lib/sessionUsage.ts`（ADR 0031） |
| 2026-08-30 | §2.6 UI：将流式 chunk 排队、按帧归并、sequence 去重、step block 关联与同步 flush 收口到 `ui/src/lib/streamAggregator.ts`（ADR 0032） |
| 2026-08-30 | §2.6 UI：将 shell、notify、generic/raw 工具结果 body 按 kind 注册到独立 renderer 组件，`ToolResultCard` 保留公共卡片壳与复杂工具分支（ADR 0033） |
| 2026-08-30 | §2.6 UI：将 `files` 工具的文件操作、目录和读取结果收口到 `ToolFileResult.svelte`，并由 renderer registry 按工具名选择（ADR 0034） |
| 2026-08-30 | §2.6 UI：将 `system` 工具的机器指标、显示器、环境变量筛选/复制与电源状态收口到 `ToolSystemResult.svelte`，并由 renderer registry 按工具名选择（ADR 0035） |
| 2026-08-30 | §2.6 UI：将 `process` 工具的筛选、显示上限、CPU/内存指标和状态表格收口到 `ToolProcessResult.svelte`，并由 renderer registry 按工具名选择（ADR 0036） |
| 2026-08-30 | §2.6 UI：将 `window`、`actions`、`schedule` 结果分别收口到对应 renderer 组件，并由 registry 按工具名选择（ADR 0037） |
| 2026-08-30 | §2.6 UI：将 `http`、`clipboard`、`web_search` 结果分别收口到对应 renderer 组件，并由 registry 按工具名选择（ADR 0038） |
| 2026-08-30 | §2.6 UI：将 `files` 的搜索结果与普通文件结果分流到对应 renderer，并由 registry 按结果 shape 选择（ADR 0039） |
| 2026-08-30 | §2.6 UI：将 `agent` 工具结果收口到 `ToolAgentResult.svelte`，并由 registry 按工具名选择（ADR 0040） |
| 2026-08-30 | §2.6 UI：将工具结果解码与 custom shape 分类收口到 `ui/src/lib/toolResultParsing.ts`，`ToolResultCard` 保留兼容 re-export（ADR 0041） |
| 2026-08-30 | §2.6 UI：将会话切换/token 概览与模型/联网搜索菜单收口到 `SessionToolbar.svelte`、`ModelToolbar.svelte`，路由页只编排状态与回调（ADR 0042） |
| 2026-08-30 | §2.6 UI：将每步用量聚合、缓存命中率与 token tooltip 收口到 `ui/src/lib/sessionUsagePresentation.ts`，路由页保留响应式状态适配（ADR 0043） |
| 2026-08-30 | §2.6 UI：将 Agent thought/reasoning、web search、补充输入、工具 action/output/observation 的事件 handler 收口到 `ui/src/lib/chatAgentEventHandlers.ts`，路由页只保留状态与监听器编排（ADR 0044） |
| 2026-08-30 | §2.6 UI：将 Agent 用量与上下文压缩事件投影收口到 `ui/src/lib/chatUsageEventHandlers.ts`，路由页只保留监听器编排（ADR 0045） |
| 2026-08-30 | §2.6 UI：将 `confirm:requested` 到确认队列项的安全事件投影收口到 `ui/src/lib/chatConfirmationEventHandlers.ts`，路由页保留队列与授权 IPC 编排（ADR 0046） |
| 2026-08-30 | §2.6 UI：将 session 生命周期事件的状态投影与终态清理收口到 `ui/src/lib/chatSessionEventHandlers.ts`，路由页保留响应式状态回调（ADR 0047） |
| 2026-08-30 | §2.6 UI：将欢迎态、消息列表、后台等待提示与错误继续按钮收口到 `ui/src/lib/ChatMessageTimeline.svelte`，路由页保留滚动容器与业务回调（ADR 0048） |
| 2026-08-30 | §2.6 UI：将 ask 选项选择、批量回答、忽略、恢复清理与重复提交防护收口到 `ui/src/lib/chatAskInteraction.ts`，路由页保留输入编排（ADR 0049） |
| 2026-08-30 | §2.2 LLM：将 endpoint 健康、熔断状态机、role 索引与健康槽位初始化收口到 `crates/llm/src/endpoint_health.rs`，router 保留并发存储与请求时机（ADR 0051） |
| 2026-08-30 | §2.6 UI / 持久化：删除已到期的 `stores.ts` 消息/用量兼容 re-export 与未压缩 ReAct snapshot 读取回退；旧数据按发布说明重置（ADR 0052） |
| 2026-08-30 | §2.2 LLM：将流式上下文估算、idle scaling、规则门禁、chunk 聚合与首 chunk 前重试收口到 `crates/llm/src/streaming.rs`，router 保留 endpoint 编排（ADR 0053） |
| 2026-08-30 | §2.4 Agent：将增量事实抽取窗口、transcript 构造、来源解析与提案安全门禁收口到 `crates/agent/src/fact_inference.rs`，inference 保留调度与写入编排（ADR 0054） |
| 2026-08-30 | §2.6 UI：将默认模型发现缓存、设置投影、provider 能力归一化与刷新代次收口到 `ui/src/lib/chatModelSync.ts`，路由页保留响应式状态与菜单编排（ADR 0055） |
| 2026-08-31 | §2.5 Agent：将 ReAct loop 拆为 Run/Turn/ToolBatch，明确一次采样边界、steering 优先级和工具结果的 canonical 顺序（ADR 0056） |
| 2026-08-31 | §2.5 Agent：以 `react::ReActState` 统一 Run/Turn/ToolBatch 的 events、canonical 与 branch points；provider sanitize 和失败 retry nudge 收口为临时请求态（ADR 0057） |
| 2026-08-31 | §2.5 Agent：以 `RequestContext` 统一 provider 请求视图；inbox envelope 保留独立边界；流式 thought/reasoning 通过有序队列与 `agent:stream_reset` 隔离重试代次（ADR 0058） |
| 2026-08-31 | §2.5 Agent / 持久化：为 ReAct 工具调用保存 `step_id + action_index + tool_call_id` 稳定身份，确认恢复与无快照投影按身份关联；参数验证改为结构化失败，不再猜测 schema 值（ADR 0059） |
| 2026-08-31 | §2.5 Tools：工具重试改由操作幂等性策略决定；取消/超时统一为结构化执行结果；Shell/Skill/MCP 的未知终止禁止自动重试；工具超时与重试配置只在显式设置时覆盖 intrinsic policy（ADR 0060） |
| 2026-08-31 | §2.5 Agent/Tools/Memory：工具批次改为有界可取消调度，工具声明只读/资源/独占并发策略；统一 observation 截断与 step 生命周期终态（ADR 0061） |
| 2026-09-01 | §2.5 Agent：将 Turn 响应策略与工具批次身份计划提升为独立边界；恢复运行使用绝对步数终点判断工具失败重试（ADR 0062） |
| 2026-09-01 | §2.4 Memory / Agent / Tools：以 `MemoryQuery` / `MemoryRecall` / `MemoryRetriever` 统一关键词、向量、敏感过滤与 prompt 记忆缓存，provider 只负责获取向量（ADR 0063） |
| 2026-09-01 | §2.5 Agent：将 turn-start 上下文收集与投影分离，统一本地队列/inbox 优先级；单工具与并行工具共用 plan、取消修复和 ordered projection（ADR 0064） |
| 2026-09-01 | §2.5 Agent/Tools/Memory：收紧上下文、工具与记忆路径的失败安全边界，避免数据库/向量/action 故障被静默伪装（ADR 0065） |
| 2026-09-01 | §2.3 Memory / §2.5 Agent/Tools：将查询、prompt memory、工具定义与 token estimate 缓存分别提取为有界/版本化结构，按 key/domain 失效并用内容指纹守住上下文一致性（ADR 0066） |
| 2026-09-01 | §2.5 Agent：上下文 Additional context 改为单项有界拼接，compaction 改为 token-aware 规划，摘要输入/输出有界并保留工具轮次与稳定前缀（ADR 0067） |
| 2026-09-02 | §2.5 Agent/Tools：以 `MessagingService` 统一 Envelope identity、claim/complete/retry/expiry 与 request/reply/receipt；InboxBus 收窄为 JSONL transport adapter（ADR 0069） |
| 2026-09-02 | §2.5 Tools：将模型可见的 `haven` 管理入口收窄为六个 capability-scoped admin tools；删除任意 dotted `config_set`，诊断结果增加脱敏与内容边界（ADR 0070） |
| 2026-09-02 | §3 阶段 B：将 `haven-mcp` 的 protocol、transport、client、manager 与测试从单一 `lib.rs` 拆出，保持 MCP 外部契约不变 |
| 2026-09-02 | §3 阶段 C：将 `haven-tools` 的 shell runtime、background actions、output/process helpers 从 `bg.rs` 拆出；旧 `bg` 路径暂留薄 facade |
| 2026-09-05 | §3 阶段 C 收尾：删除已无 workspace 调用方的旧 `bg` facade，统一使用拆分后的模块与 crate-root 导出（ADR 0082） |
| 2026-09-02 | §3 阶段 E：将 `haven-app-binary` 的事件桥、宿主 handler 与 Tauri 启动编排从 `lib.rs` 拆至 `event_bridge.rs`、`handlers.rs`、`bootstrap.rs`，保持启动与 IPC 契约不变 |
| 2026-09-03 | §3 阶段 F：将 Settings/Model/Memory 三个 UI 大视图按 tab 与职责拆至 `SettingsGeneral`、`SettingsLimits`、`MediaSettings`、`SessionHistory`、`LongTermFacts`、`MemoryRecall`，父视图保留唯一状态、IPC、事件与保存边界 |
| 2026-09-08 | §2.6 UI：工具卡统一显示各自参数与结果的 token 估算；provider 真实总量仅保留在会话级统计，删除首个工具卡的 step 聚合展示（ADR 0099） |
| 2026-09-08 | §2.6 UI / §2.3 Memory：聊天 token 摘要改为可展开明细，展示上传/生成、缓存命中率、当前上下文预算与累计费用；用量持久化增加最后一次上下文快照（ADR 0102） |
| 2026-09-08 | §2.5 Tools：将 `haven_session_diagnostics` 合并到 `haven_diagnostics`，统一模型可见诊断入口并保留会话数据脱敏与独立并发资源（ADR 0103） |
| 2026-09-08 | §2.3 Memory：删除历史 schema/data migration，数据库收敛为严格 v16 当前契约；统一 FTS5、事实/episode 类型域、向量维度与 RRF 混合召回，并在 provenance 落库前限长脱敏（ADR 0105） |
| 2026-09-08 | §2.3 Memory / §2.5 Agent：事实抽取 outbox 增加可恢复的 `kv_store` pending marker，session 删除与 orphan cleanup 统一回收 cursor、节流和队列状态；移除启动时伪造的默认姓名事实（ADR 0107） |
| 2026-09-10 | §2.3 Memory / §2.5 Agent / §2.6 App：消息新增 v17 `media_inputs` canonical 投影；managed uploads 统一覆盖图片/音频/文件；OCR/STT 表示持久化并保留 raw；旧快照与 compact summary 在 snapshot 边界剥离 inline bytes（ADR 0121） |
| 2026-09-10 | §2.5 Tools / §2.6 LLM：工具与 media gateway 统一走 `LlmRouter::analyze_image`；`files.read` 增加音频转写；删除 raw-byte multimodal helper（ADR 0122） |
| 2026-09-10 | §2.5 Tools / §2.6 App：新增 asset_id-only `media` 工具；窗口截图改为受管生成媒体并返回可继续消费的 asset id；managed 图片/音频从 `files.read` 转交 canonical media 派生入口（ADR 0123） |
| 2026-09-10 | §2.3 Memory / §2.4 Agent / §2.6 UI：媒体派生结果只保留一个 `media.content`，模型观察移除运行时元数据；工具拥有的媒体 LLM 调用以 `call_kind=media`、其它工具内部 LLM 调用以 `call_kind=tool` 单独持久化与展示，Agent 缓存率只统计 `call_kind=agent`（ADR 0124） |
