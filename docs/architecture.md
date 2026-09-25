# Haven 架构与 crate 职责

> 版本: v1.6 | 日期: 2026-09-25
> 范围: `crates/` (Rust 后端, Tauri 2)
> 原则: **依赖单向、叶子优先**。上层 crate 只依赖下层，绝不反向依赖；共享数据与类型放叶子（`haven-common`），
> 组件职责按「谁拥有实现、谁只消费接口」划分。

---

## 1. 依赖图

```
haven-app-binary（组合根 / 宿主边界）
├── haven-agent（ReAct 编排）
├── haven-input（输入采集 / VAD）
├── haven-tools（工具执行）
└── haven-mcp（MCP 客户端）

haven-agent ──► haven-tools, haven-memory, haven-llm, haven-common
haven-tools ──► haven-input, haven-mcp, haven-memory, haven-skills, haven-llm, haven-common
haven-mcp   ──► haven-llm, haven-common
haven-input / haven-llm / haven-memory / haven-skills ──► haven-common
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
| `haven-input` | common | 录音 / VAD / PCM/WAV 采集（不实现 provider 或转写） |
| `haven-agent` | common, llm, memory, tools | ReAct 循环 + 会话执行 |
| `haven-app-binary` | 以上全部 + tauri | 装配 + Tauri 命令 + 事件桥 |

> 依据 `crates/*/Cargo.toml` 实际 workspace 依赖整理。`haven-agent` 与 `haven-app-binary` 是最上层，
> 其余全部是它们的底层依赖。`haven-llm` 不允许被业务 crate 反向依赖。
> 语音转写的运行时调用路径是 app → tools → llm；`haven-input` 只产出采集结果，不直接依赖 `haven-llm`。

`haven-mcp` 内部按职责分为 `protocol.rs`（MCP/JSON-RPC DTO 与内容归一化）、
`transport.rs`（stdio、Streamable HTTP、SSE 和进程边界）、`client.rs`（单服务器连接、
限流、重连与健康监控）和 `manager.rs`（多服务器 reconcile 与 LLM caller 适配）；
`lib.rs` 只保留模块声明和公共导出，`sse.rs` 保留为 SSE parser。

`haven-tools` 的工具核心按稳定边界分为 `tool_contract.rs`（Tool、ToolResult、typed
operation 与执行策略）、`registry.rs`（全局注册表、SessionCatalog、版本快照与 probe）和
`security.rs`（AuthorizationEngine、权限继承、disabled operation、路径沙箱与本机安全矩阵）。
在这组稳定模块之上，`OperationRegistry` 持有已安装、deferred 与 session operation；
`OperationCatalog` 是模型可见投影；`AuthorizedExecutor` 做熔断、启用检查、校验、执行和结果分类。
crate-private `ToolAuthorizationPolicy` 负责从 live session lookup 或 turn snapshot 生成同一 typed
`AuthorizationRequest`；未命中工具的保守 fallback 也只有一份。它不作 allow/deny/confirm 决定，
该决定仍由调用方在 `execute_tool` 之前交给唯一的 `AuthorizationEngine`，不在工具 future 里阻塞。
`OperationSpec` 是运行时
策略和 manifest 的唯一定义，覆盖 builtin operation view、root tool，以及 MCP/Skill adapter；spec 没有
handler。`policy_for` 是一次调用的 `OperationPolicy`，`catalog_policy` 是目录上界。`ToolManifest`、
`ToolPolicy`、`ToolPresentation` 只由 `project_tool_manifest` 投影，保留为 Tauri/UI 的 IPC 形状。`tool_core.rs`
组合 registry 与 authorization。`tool_runtime.rs` 用一份不可变 `PlatformRuntime` 承载模型、媒体、
tool settings、context limits、shell 与 security，热更新整份替换。crate-private
`ToolRuntimeCoordinator` 是 `ToolCore`、`ToolRuntime`、`ToolBuiltins` 的组装 owner，并负责
PlatformRuntime 发布、MCP discovery config/index 更新和 builtin catalog rebuild 的工具侧顺序。
messaging 与 memory recall 是进程服务，在 `wire_startup` 里绑定一次，不放进这份快照；
`admin_surfaces` 随成功的 catalog rebuild 写入 `BuiltinCatalog`。`tool_builtins.rs` 组合 MCP/Skills
与具体 builtin provider。MCP、skills、授权、媒体资产、action 与 live output 由构造时交出的
`ToolServices` 提供，调用方不再向 `ToolsManager` 逐个取服务。组合根仍是 `ApplicationRuntime`，
不另建 `AppRuntime`。`ToolsManager` 是对外 façade，保留执行与授权入口、session overlay/asset
lease 操作、目录投影、runtime capability 请求和录音转写入口；启动及 runtime/catalog 更新转发给 coordinator。
能力判断由 tools crate 唯一构造的 crate-private `ToolCapabilitySnapshot` 收口：prompt runtime、
媒体 operation catalog、TTS/STT 与录音 gate 使用同一 typed 能力值，搜索优先级由它统一投影。
snapshot 每次从当前 `PlatformRuntime`、Router config 与 MCP index 重建；三者没有共同版本钟，故当前不缓存。
该 snapshot 只含能力结果，不暴露 Router、MCP manager、Database 或授权执行 facade（ADR 0331）。
配置侧仍由 app-binary 的 `RuntimeConfigCoordinator` 持有 config apply gate，并准备/发布 Router 与媒体
client；model edit 的完整提交和应用也由它持有。`SettingsRuntimeApplyCoordinator` 从共享 target plan 生成
Settings 有序阶段，驱动命令提供的执行回调，并唯一记录当前 phase、snapshot version、Router published、
restart-required targets 与失败/警告。Security、MCP、context、logging、hotkey 等实际副作用仍由既有 owner
执行；Settings edit/no-op 仍由命令按同一次 `ConfigService::edit` 保留旧 hotkey、snapshot 和 change。该
coordinator 不复制 Router prepare/publish，也不为半失败状态增加 compensation/rollback。MCP Tauri 命令负责
持久化和连接/刷新动作，完成后请求 ToolsManager façade 重建 catalog；连接及其 `catalog_version` 仍由
`McpManager` 持有。应用退出顺序由 `ApplicationRuntime` 负责，coordinator 不增加独立 shutdown 生命周期
（ADR 0333、0337）。
安全矩阵只有 `security.rs` 一个权威来源，五个 Admin surface 由 ADR 0070/0071 定义的 typed
operation 实现。边界见 ADR 0162、ADR 0211、ADR 0212 与 ADR 0213。

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
host-owned deferred catalog，由 `tool_catalog` 的 `action=load` 按 operation/root 原子加载到当前 session；
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
- 聊天页「联网搜索」为命名模型级 `off|auto|always`；仅
  `supports_builtin_web_search(api_style)` 为真时由对应适配器注入内置搜索工具，
  UI 对不支持的线协议灰显。
- 厂商扩展（DeepSeek `thinking` / Responses `reasoning.effort`、Kimi
  `thinking.type`+`keep` 等）挂在对应 adapter + provider/base_url/model 检测上，
  复用聊天页「思考强度」，不另开线协议。
- `request_descriptor.rs`：crate-private `RequestDescriptor` 显式并列承载逻辑请求用途和所需
  `Capability`；用途到能力的映射复用 `RequestKind::required_capability()`。Router 将同一
  descriptor 传至 complete、embedding、raw stream 与 aggregated stream 执行边界；health check
  与 native transcription 也在 Router route/permit boundary 从原 `RequestKind` 构造 descriptor。
  health adapter 调用和已选 client 的 native `transcribe` 不再推导 route capability；STT fallback
  以独立 `AudioChat` purpose 重新进入 aggregated route。usage owner 仍由调用方表达（ADR 0319、0329、0339）。
- `model_directory.rs`：crate-private `ModelDirectory`，从 Router 的配置 snapshot
  构造 provider client map 与以 `RequestKind` 为原 key 的 primary route；route 保存筛选时
  使用的 descriptor，执行解析要求收到的 descriptor 与 route 相同。它还集中 client/model
  选择、capability profile 和 endpoint/context-window metadata 查询；生产路由同时要求
  所需 `Capability` 与可用凭据，测试注入只跳过凭据过滤。metadata 借用 Router 的单一
  `RouterConfig` snapshot，不复制配置真源。配置/metadata helper 继续接收 `RequestKind` 并
  经 `RouterConfig::route` 校验 route，不调用 provider 或投影 usage/health；`capability_profile`
  只读取已选 adapter 的本地 wire profile。`connection_status` 与 `prewarm_all` 是明确的健康
  probe，会调用 health check 并按既有规则投影 outcome（ADR 0316、0329、0339）。
- `router.rs`：`LlmRouter` 保留配置 snapshot 与请求执行状态，拥有 route/client 选择、
  health/circuit、rate-limit cooldown、semaphore 与 stream rules，并为执行器提供配置
  snapshot 和 health/rate-limit outcome closure。每个 request kind 仍只走唯一 primary；
  同一模型内重试耗尽后直接返回错误，不跨 provider/model 切换缓存命名空间（ADR 0192）。旧 `llm.roles` 仅在
  配置加载时转换，不进入生产路由；`CallExecutor` 执行 complete/embedding，`StreamExecutor`
  只执行 raw stream 建流与 permit 包装；`AggregatedStreamExecutor` 执行聚合流状态机。
  执行器已接收显式 descriptor。public request DTO 继续以 `RequestKind` 表达兼容 route key，且
  descriptor 的 `purpose` 仍是 `RequestKind`；health/native transcription 已在路由边界使用
  descriptor，无需再传进不拥有 route 语义的 adapter。`LlmCallKind` usage role 继续由 Agent/Tools
  调用方显式设置，不由 Router 推断（ADR 0339）。
- `call_executor.rs` / `stream_executor.rs` / `aggregated_stream_executor.rs`：接收 Router
  已解析的 descriptor、model/client 与单份 `RequestPolicy`，复用 request pipeline 执行 complete/embedding、
  raw stream 建流或聚合流执行，并经 Router 注入的窄 outcome closure 投影健康状态。raw
  `PermitStream` 持有 permit 到 stream
  drop；聚合执行器集中首次 `on_chunk` 交付前重试、规则触发后的 guidance 重试、取消、总 timeout、attempt
  hooks 和最终结果交接。Router 的 permit 覆盖完整聚合执行；health/cooldown 状态仍由 Router
  更新（ADR 0318、0327、0328）。
- `streaming.rs`：只执行单条 provider stream 的创建后消费与聚合，负责 idle timeout、取消、
  stream rule 检查、chunk 顺序与 `LlmResponse` usage/content 累积；逻辑请求的多 attempt 状态机归
  `AggregatedStreamExecutor`（ADR 0328）。
- `request_pipeline.rs`：provider-neutral 的 `RequestPolicy`/`RetryPolicy`；
  为普通聊天、工具聊天、embedding、raw stream 建流和 aggregated streaming endpoint
  尝试提供同一份重试预算快照与总超时执行语义。Router/执行器共用这些 helper；adapter
  不实现第二套重试。
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
  `fact_graph.rs` 集中负责 `facts` 写入与图谱不变量，`fact_query.rs`
  负责事实读取、搜索/排序，`fact_maintenance.rs` 负责事实清理、衰减与矛盾
  扫描，`embedding_store.rs` 以窄异步 `MemoryEmbeddingStore` 提供嵌入索引生命周期
  的持久化端口，`memory_recall_store.rs` 以 `MemoryRecallStore` 提供异步 typed
  keyword/vector recall、可见事实 hydration、revision 与完整 recall 端口；
  `action_store.rs` 以异步 typed `ActionStore` 提供后台/定时 action 与 completion outbox
  的窄持久化端口，并在 Memory 内调度 SQLite blocking 操作；`facts.rs`
  负责事实类型、谓词策略和稳定 `Database` 外观。消息的
  `media_inputs` 是多模态 canonical 持久化投影；消息返回对象中的 `attachments` 仅是
  ingress/UI DTO。数据库的 `ui_metadata` 只保留 UI 展示与受管资产保留所需的元数据，
  并由受信 host 根目录重建历史预览，不参与 provider 规划或 transcript 恢复。
- `embeddings.rs`：向量编码、相似度/ANN 查询和 embedding 存储操作。

schema 初始化不改变 X12：`session_events` 经 `SessionStore` 追加并按
sequence replay，是会话恢复、rollback、交互重建和实时订阅的唯一事件权威；
`messages` / `session_steps` 仍是投影，生产路径没有独立的 ReAct checkpoint 表，
也不把可恢复的 ReAct JSON 写回数据库。`ReActState` 只存在于进程内作为投影
scratch；完整 `events`、interaction、usage、run budget 和多套 cursor 不得写入
数据库快照。`sessions.react_state` 已随 schema v28 删除；旧库按 reset 丢弃，不迁移，
测试 transcript 只投影 `session_events`。
`UserInject` 事件只保存 `MediaInput` 元数据，reset 只替换持久化载体，不成为新的业务真源。

**判定标准**：只负责 SQLite 生命周期与记忆数据持久化；Agent 编排、LLM
provider 协议和 UI 展示逻辑不得进入本 crate。

Agent 的 `memory_service.rs` 是 prompt/worker 共用的 typed memory 边界：它集中管理
有界候选、recall、embedding/index 句柄和 prompt-memory cache；向量行的 scope、敏感
过滤、规范化与 keyword 融合仍由 `haven_memory::recall::MemoryRetriever` 统一负责；
`MemoryRecallStore` 调度 recall SQL 并返回 typed domain results。Agent 保留 prompt
查询归一化、embedding provider 调用、候选合并与预算；`MemoryEmbeddingStore` 负责
embedding 生命周期读写和 LSH 维护，`memory_index.rs` 保留模型路由、provider 校验、
批处理和维护门控（ADR 0021、0303、0304）。`MemoryService` 构造并持有共享的
`MemoryFactStore`，`MemoryWorker::load_known_facts` 通过其有界读取端口取得抽取上下文；
blocking 调度、事实有效置信度顺序、敏感事实过滤和 limit 属于 Memory，Agent 仍负责
prompt 行格式、subject 前缀与字段清洗（ADR 0307）。
`MemoryFactExtractionStore` 通过同一 `MemoryService` owner 提供 ordinary/summary extraction 的
typed cursor、共享节流时间戳，以及 ordinary transcript projections（ADR 0308、0312）。Agent 负责窗口构造、
LLM 调用、候选解析/清洗和事实写入策略；`MemoryFactStore::persist_inferred_batch` 在一个
blocking closure 与 SQLite 事务中完成存在性检查、图谱 upsert 和 `FactSourceRef` 持久化，
调用方仍决定新事实置信度下限，写入失败时整批回滚（ADR 0309）。`MemoryMaintenanceStore` 还
提供 LLM 维护的 typed persistence ports：矛盾候选读取、门控后按 id demote、predicate count DTO
读取和逐 predicate rewrite（ADR 0311）。Agent 保留模型配置门禁、提示词/上下文、serde 解析、
候选过滤、安全 gate、日志和降级策略；列表读取失败仍跳过对应 LLM pass，矛盾 demote 失败仍
按原策略计 0，predicate rewrite 失败仍 warning 并继续后续提案。每个 store 调用独立进入
blocking pool；多条 rewrite 不合并事务，repository 内单条 rewrite 原有事务不变。确定性维护也
通过 `MemoryMaintenanceStore` 的逐项 typed 操作调度 repository 方法：facts 去重、敏感事实删除、
规则矛盾 keeper、低置信度 flush、孤儿 embedding、orphan extraction/event cursor 与 source ref 清理。
Agent 继续控制步骤顺序、日志、计数和部分失败后的聚合错误；确定性步骤仍是独立 blocking 操作，
没有新增事务。周期路径把 cancellation token 传到确定性 SQLite 操作边界（ADR 0310）。
生产 `MemoryWorker` 的 raw Database 使用现已清零：摘要 episode cursor 与共享节流戳也经
`MemoryFactExtractionStore` 读写。`MemoryService` 私有保留 backing `Database` 作为实现细节，用于
构造 typed stores 与 embedding index，不向 Agent Worker 暴露 raw handle。embedding catch-up 与
LSH lagging 检查沿用 `MemoryService` 的 `MemoryEmbeddingStore` 边界。
`memory_worker.rs` 只编排事实抽取、durable outbox、维护、提案提交和索引 catch-up；
`MemoryWorker` 是唯一的后台记忆编排入口。`prompt_context.rs`
在 turn 边界取得一次工具/运行时/记忆快照，`prompt_renderer.rs` 以纯函数渲染 system
message 与 MEMORY fence，不访问 DB、router 或 cache。事实抽取 outbox 以 `kv_store`
marker 持久化，不把 provider 网络调用下沉到 Memory；事实维护的 SQL 清理与矛盾候选
读取由 `fact_maintenance.rs` 负责，`MemoryMaintenanceStore` 提供确定性与 LLM 维护 persistence
操作的异步 typed 边界；维护调度、LLM 仲裁、提案门禁与并发控制仍属于 Agent（ADR 0022、0063、0169、0310、0311）。

### 2.4 `haven-input` —— 输入采集与语音生命周期

- `capture/`：CPAL 采集线程 + 环形缓冲 + 重采样。
- `vad.rs`：tract ONNX 语音活动检测（含常驻 worker 线程）。
- `lib.rs` 的 `InputPipeline`：录音状态机（start / stop / cancel）、VAD 判定 →
  自动停止、PCM/WAV 序列化和采集侧错误。
- `hotkey.rs`：快捷键字符串解析为中性 `KeyCombo`（与平台解耦）。

**判定标准**：管「何时/怎么采」——录音生命周期、VAD 和音频产出；**不实现** provider
调用、转写或 fallback。

### 2.5 `haven-agent` —— ReAct 编排与会话执行

- `react/`：ReAct 循环（`loop` / `turn` / `effects` / `response_cycle` / `stream_step` / `tool_batch` / `tool_batch_execute` / `tool_batch_policy` / `tool_batch_plan` / `context` / `inject` / `turn_end` / `event_boundary` / `retries` / `hooks` / `hook_policy` / `committed_ui` / `transcript` / `state` / `request_context`），按 Run → Turn → ToolBatch 分层；`ReActState` 统一表示当前 run 的 events、canonical、branch points、retry nudge 和 turn cancel，所有边界共享同一运行态。它目前仍由 actor 外的循环持有；[ADR 0214](adr/0214-react-run-inside-session-actor.md) 规定热 transcript 改由 `SessionState` 独占，一次 run 在 actor 任务内执行，并且只在 yield 点借用 `&mut SessionState`。`loop` 只负责 run 预算、生命周期和按序应用 `EffectBatch`，`turn` 负责模型阶段编排并产出 effect batch，`effects` 是 transcript、UI-only 投影、branch point 和 pause 的唯一按序应用边界；turn 终态与工具批次的 durable 提交都走这里，turn-start 注入和 stream chunk 仍留在各自边界，`response_cycle` 负责一次采样后的空响应/截断重试，`tool_batch_plan` 固化 assistant 调用顺序和跨层身份，`tool_batch_execute` 负责批次准入、并发执行、取消与按序提交，`tool_batch_policy` 负责失败分类与重试提示，`tool_batch` 负责工具执行原语、确认生命周期与结果状态。`RequestContext` 从 durable canonical 生成不可变的 provider 请求视图，统一承载 sanitize、retry nudge 和一次性重试指令，不反写 transcript；`context` 只收集有边界的上下文项，`inject` 只经 `apply_transcript` 投影，`turn_end` 只组装最终 effect batch，`event_boundary` 负责事件流完整性与生命周期边界，`hooks` 只定义扩展契约，`hook_policy` 装配生产副作用策略。
- 流式输出由 `stream_step` 产生，`event.rs` 用一个有序 chunk 队列归并 thought/reasoning；provider retry 通过 `agent:stream_reset` 标记新的输出代次，UI 只清理 live stream block，不修改 durable transcript。`streamAggregator` 只合并相邻且同身份的 chunk，保留交错输出顺序；最终 thought/reasoning 投影仍是丢 chunk 时的权威修复路径。
- **X12 持久化契约**：ReAct 将 live transcript 作为 `SessionCommitted` domain intent 提交给 `SessionStore`；Agent 负责 ReAct 事件 payload 与消息/步骤语义，Memory 将 intent 翻译为物化行。Store 在同一 SQLite 事务中先追加 `session_events`，再写入 intent 指定的 `messages` / `session_steps` 投影；投影失败时整笔回滚，事务提交后才使 cache 失效并广播事件。Agent 随后由 `CommittedUiPublisher` 按 `session_events.sequence` 发布 Thought、Action、Observation、Supplement、ingress MediaPlan 与 Compaction，再更新进程内 canonical。assistant Thought 消息行与 durable event 同事务提交；共享 `step-*` 的 Thought 执行步骤作为可修复的后置 Store 投影写入，失败不会撤销已提交事件或重复发布。流式分片只用 `chunk_seq`；WebSearch、Usage，以及请求准备阶段的 MediaPlan（`event_seq` 为空）不占用这条 durable 序号。同一 sequence 的并行工具卡按 `(eventSeq, stepId)` 去重。交互请求也必须由 `SessionActor` 命令追加为 domain event，恢复只 replay 事件流；resume、rollback 和实时重放均从 event sequence 读取，事件流本身承载恢复游标。rollback 的 event cursor 用于 active transcript 投影，event sequence 用于 append-only timeline；`last_msg_at` 只用于截断物化消息投影，三者由 `SessionStore` 封装且不得互相推导或作为 transcript 真源。多模态输入在 ingress 接受 `MessageAttachment`，但事件/数据库 canonical 投影使用 `MediaAsset → MediaRepresentation → MediaPlan`，事件不保存 inline bytes；OCR/STT 成功追加派生表示且保留 raw asset。合法旁路限于 ingress seed、recovery partial、终态 action-result、UI-only ask/confirm notice；turn-end 防御性 search-final 仍可在既有 ToolCall event 后直接补 message projection，单独跟踪收口。
- **工具调用身份契约**：同一 assistant tool batch 内，`action_index` 是 provider 调用数组的零基稳定位置，`step_id` 是该调用的持久执行行/卡片身份，`tool_call_id` 是 provider 调用身份；`session_steps` 与 ReAct events 同步保存三者。确认恢复必须按完整身份关联，禁止按工具名、参数或 observation 文本猜测；缺失事件流不再从步骤投影重建 ReAct transcript，旧数据按 reset 边界处理。
- **工具参数验证契约**：执行前只验证，不用 schema default、首个 enum 或类型占位符改写输入；无效参数以包含 `action_index`、工具名和验证明细的失败 observation 返回给模型，避免改变副作用语义。
- `session/`：`SessionSupervisor` 只负责 registry、并发 admission 和生命周期；`SessionActor` 的 mailbox 只接收外部命令（提交、steering、交互、取消、后台结果、messaging 和 supervisor 生命周期）。run budget、usage、stream identity、token estimate 和热 transcript 的目标主人是 actor 任务内的 `SessionState`，由 run 在 yield 点以函数调用访问，而不是内部 mailbox 命令（ADR 0214）。当前代码尚未完成这道迁移，仍经 mailbox 往返；在此之前不得再增加同类内部命令。inbox 通知游标、轮询节拍和标题缓存已在 `SessionState`；进程级 heartbeat 合并仍留在 `MessagingPoller`；`TurnEngine` 只推进一次 turn 并产出 `EffectBatch`，`RunEngine` 负责应用批次和 run 边界；`dispatcher` / `queues` / `status` / `tool_runner` 只提供各层协作能力。
- `layer.rs` + `ingress.rs` / `resume.rs` / `resume_support.rs`：对外入口与 resume 恢复；`resume_support` 只提供确定性的候选合并、悬空工具调用修复和运行时工具选择恢复。
- `canonical.rs`：发送前 `sanitize_canonical` 闸门。
- `memory_worker.rs` / `memory_service.rs` / `memory_index.rs` / `prompt_context.rs` / `prompt_renderer.rs` / `prompt.rs` / `compactor.rs` / `rollback.rs` / `rollback_support.rs` / `title.rs` / `event.rs` / `partial.rs`；`memory_service` 统一 typed memory/embedding/cache 边界，`prompt_context` 取得 bounded turn snapshot，`prompt_renderer` 纯渲染 bounded MEMORY fence；`rollback.rs` 编排生命周期与 DB 双时钟，`rollback_support` 只操作 events 和 branch cursor。
- `fact_extraction.rs`：事实抽取 DTO、LLM 字段 coercion、标签/谓词规范化、prompt
  字段清洗和 JSON array 提取；`MemoryWorker` 负责调度与持久化（ADR 0029、0169）。
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
| 接线 | `haven-app-binary` `app_state` | 安装一个 typed `MessagingRuntime`，同时提供 SessionActor mailbox 与 peer 生命周期（tools 不依赖 agent） |
| 运行时 | `react/context.rs` + `react/inject.rs` | `context` 负责每步 heartbeat、通知或每 3 步通过 `MessagingService::claim` poll inbox（receiver、节拍和标题缓存在 `SessionState`，heartbeat 合并仍是进程级）；每个 envelope 保留为独立上下文项，投影 durable 后由 `MessageClaim::complete` ack 并发 receipt；`inject` 经 `apply_transcript` 注入带消毒后的 `id`/`in_reply_to`/`subject`；`InjectSource::CrossSession` |
| 生命周期 | `session/status.rs` | `interrupt_session`/`end_session` 先取消并立即返回控制结果；若 run 仍在收尾，terminal cleanup、partial promote 与 actor 移除延迟到 dispatcher 的 run-exit 边界；终端态继续 BFS 子孙 system notice + 无嵌套 cascade 结束；`type=system` 仅运行时 |
| 信任 / 记忆 | `memory_worker.rs` | 跳过 `peer_kickoff` 与跨会话注入文本的 fact 抽取 |
| UI | 对话页 tool card | `agent` 结构化卡片；自动同伴邮件以 `agent`/`inbox`/`auto` 卡片展示；kickoff 左侧「低信任委托」 |

协议约定：同伴消息 ≠ 用户指令；`id` 是稳定的 `msg-{uuid32}`，`in_reply_to` 对齐 request id，
`delivery_attempt` 记录 at-least-once 重投次数；批量消息必须走 `send → claim → process → ack`。
显式 `agent.inbox` 默认只 claim 不 ack，处理完成后由 `agent.ack(message_ids|claim_token)` 确认；
`claim_token` 是进程内整批 receipt，崩溃后由 durable processing 状态触发 at-least-once 重投，
而不是丢失消息。`agent.history` 为只读恢复入口。`agent.status/join/wait/stop/collect` 只允许当前 session 或其后代，
并通过 `MessagingRuntime` 进入真实 `SessionSupervisor` / `SessionActor` 状态机，`stop` 走正常取消与终端清理路径。
同进程 session 优先使用 SessionActor mailbox；跨进程仍使用 JSONL adapter 作为 fallback；子会话默认工作目录仍为
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

`ActionService`（`haven-tools/src/action_service.rs`）是后台与定时任务的唯一运行时状态机；
shell 进程、定时器和 action dependency 共享一个 action map、一个生命周期 sink 和一个
completion bus。统一状态为 `waiting → running → completed | failed | cancelled`；定时任务的
`kind` 只表示任务类型，不再作为状态值。model-facing `actions.*` 和 app action board 都
直接读取规范化 task row。background completion outbox 与 scheduled fire recovery 共用纯
`haven_common::action_lease::ActionLease<T>` claim core：outbox 在 `BEGIN IMMEDIATE` 事务中以
稳定 `action_result_id` 和 SQLite UTC deadline 判断 30 秒 claim，随后仍由原 SQL/CAS 写入；
scheduled fire recovery 以 `action_id` 和单调时钟使用 15 分钟进程内 lease。现有契约没有独立
的 claimant owner token，也没有 lease renewal 操作。scheduled 终态和无 consumer 回滚会清除
其 pending fire 与 lease；background completion lease 过期后可再次 claim，直到 transcript
durable 后按 `action_result_id` ack。ActionStore 仍各自拥有 outbox、scheduled trigger 的
事务和 CAS；Tools 不持有 raw `Database` 或安排 SQLite blocking 工作。CAS 仲裁、内存 board、
终态持久化修复重试判定由 crate-private `ActionPersistenceRetryPolicy` 纯 typed owner 收口：
background/scheduled worker 均无 retry deadline/预算，保留 1 秒起步、指数退避、30 秒封顶；scheduled
每次 store 调用内部原有的 3 次/50 ms 重试仍保留。策略不持有 clock、sleep、store 或 terminal arbitration。
ActionService 继续按 kind 执行各自 CAS/outbox、内存状态、事件发布、重试等待与生命周期。当前 background
shell 没有 action-level 执行 timeout；scheduled `due_at` 是触发时刻。AgentLayer 对 background completion
做 durable transcript 投影/入队的 100 ms 重试与 outbox ack 也保持独立；provider/LLM retry 和 Agent
ReAct tool-call retry 不属于 action persistence retry（ADR 0305、0332、0334）。
调用边界并不是一个共享的执行 owner：后台 shell 的 child process 由 `ActionService` 启动并回收；
scheduled fire 由 `ActionService` 按 `Waiting → Running` durable CAS 后交给 AgentLayer，AgentLayer/
tool runner 执行 scheduled tool 或继续会话，再调用 `complete_scheduled` / `fail_scheduled`。
scheduled trigger 的输入分类和 due-time 计算由 crate-private 纯 typed policy
`ScheduledTriggerRequest`/`ScheduledTriggerCandidate` 承担；ActionService 仍读取 horizon 配置并拥有
durable admission、board insertion、timer/watch worker、fire、terminal commit/retry 与 lifecycle event。
这只是 trigger admission 的窄边界，不是 `Immediate`/`At`/`After` 与 execution 的完整 Job 模型；
schedule tool 对 LLM 输入的前置验证仍保留在工具边界，App command/event adapter 仍只做 UI DTO 投影（ADR 0343）。
完整 lifecycle 审计没有发现需要迁移到另一个纯 transition policy 的重复判断：status graph 与 terminal claim 已由
`ActionStatus::can_transition_to` / `action_terminal::can_claim_terminal` 单点定义；background admission 直接进入
`running`，`waiting → running` 只属于 scheduled fire。提交前后的重复检查跨越 durable CAS 与内存投影/回滚边界，保留为竞态校验。
执行副作用、outbox、retry 与 UI finished 投影继续按 kind 分流；trigger/execution、deadline/claim identity 和 restart recovery
语义需先决策，当前不引入新的 Job 状态或自动 replay（ADR 0352）。该审计还发现 `ActionService::set` 的 scheduled
admission cleanup 曾移除 Running row，导致 Agent terminal callback 找不到内存 entry；ADR 0353 已将清理条件限定为
terminal scheduled entry，并通过 completion、cancel、no-consumer recovery 和 restart 回归固定边界。Waiting/Running
entry 保持原路径；durable Waiting schedule 仍在启动时恢复，遗留 durable Running row 仍标为 failed 且不重放。
Action 输出 tail 的长度策略由 `ActionService` 持有的 crate-private `ActionOutputPort` 唯一配置；
foreground shell card 与 background action 共用该 policy 生成有字符上限的 `ActionOutputTail`，
consumer 仅拿不可 `Debug`/`Serialize` 的 `ActionTailSnapshot`。`agent:tool_output` 仍用
`session_id/step_id`，`action:output` 仍用 `action_id`；App mapper 对后者只投影身份、状态和 bounded
output。终态仍通过原 `action:finished` / background completion 路径承载最终已收集输出；完成提交、取消
或 shutdown 清理 live tail。此边界没有统一 background/scheduled 的完整 UI projection 或 Job lifecycle（ADR 0338）。
UI action board 以 `actionStore` 的 action id 索引作为权威前端 registry；layout 的 `activities` 只是 Svelte
reactive mirror，不再维护第二份 action lifecycle reducer。`list_actions` 与四个 lifecycle channel 共用
`mapActionPayload`。created/updated/output 统一 upsert；refresh 的请求序号与 `actionStateVersion`
只阻止旧 hydration 覆盖较新状态，不是事件去重。Background finished 先 upsert 终态，再由
`finalizeBackgroundActionMessages` 投影到仍绑定该 action 的工具卡；scheduled finished 则从 board 删除，
通知由 Agent 的 `notification:show` 提供。列表刷新会移除 terminal background history，scheduled live row
仍由 ActionService board 返回。`TaskCenter` 的状态文案、取消能力和 background result 投影继续按 kind 区分；
Rust bridge 只为 background 提供 `action:output`，scheduled action 不持有 tail。没有 durable event identity，
因此不新增 UI event dedup 或统一 Job reducer；本轮只复用已测试的 terminal background projection（ADR 0344）。
`InteractionRequest`（`haven-agent/src/interaction.rs`）
是 ask、confirm 和 scheduled confirm 的共同生命周期投影，快照通过 `interactions` 保存当前
请求；旧快照不做运行时兼容读取，新的交互状态以 `Pending → Resolved | Expired | Cancelled`
表达。

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

能力标识：`permission_key(tool, params)` → `tool` / `tool.operation` / `system.power.lock`；授予父键可覆盖子操作。冒号 operation key 不再参与运行时匹配，配置加载发现这类 key 时备份并按重置策略处理。
操作 view 的 `OperationPolicy.permission_key` 是权威身份；契约另外声明 `effect`、`data_sensitivity`、
`network_access` 和执行并发，不能由风险等级、并发属性或前端字段推断。`SecurityConfig` 另外保存
`sandbox_mode`（`read_only` / `workspace_write` / `full_access`，可选 `writable_roots`）与
`network_policy`（`deny` / `ask` / `restricted` / `open`，默认为 `ask`）；`ask` 下无法约束的 opaque 子进程进入普通确认流程，
`deny` / `restricted` 仍直接拒绝，`open + workspace_write` 仍拒绝无法约束的 opaque 子进程，
Windows 子进程通过 Job Object 回收进程树；受限网络只允许经过 SSRF/DNS 校验并固定地址的 HTTP/MCP
目的地，禁止跨源重定向。`ask` 与 `restricted` 共用可验证目的地边界，但默认将网络操作交给确认流程，
不会因为默认配置而静默拒绝普通公网请求。

确认 UI：拒绝 / 仅本次 / 本对话允许此操作 / 更多允许选项；更多选项按“本对话或永久”与“当前操作、功能组、工具”组合展示，永久授权和扩大范围需要二次确认。拒绝菜单同样支持操作、功能组、工具层级。后端只接受当前 capability 的合法父级，不能由 renderer 发明任意权限键。永久授权写入 `config.toml`，安全设置子视图可按工具查看、撤销或一键清除。确认收据绑定规范化输入 hash、权限 key、策略 revision、风险和过期时间，执行前再次验证；原始 shell、网络、文件和扩展参数不进入 renderer。普通全量设置保存不拥有权限规则，避免 stale form 清空授权。授权结果另带稳定 `AuthorizationReasonCode`，调用方不得解析错误文案。

### 2.5.4 Admin Surface

模型看到 `haven.diagnostics.*`、`haven.config.*`、`haven.skills.*`、`haven.tools.*`、
`haven.mcp.*` 等点号 operation view，以及独立的 `actions.*`、`schedule.*`、
`preferences.*`、`checklist.*`。聚合器只负责内部路由，
每个 operation 继续复用子工具自己的严格 schema、风险等级、幂等性、并发资源和
session 归属；因此 `mcp_add` 是 High，而 `mcp_list` 是 Low，二者不会因共用根名
而被压平。

配置写入使用 `ConfigService::apply_patch` 的 typed patch；普通模型路径没有任意
`config_set(path, value)`。诊断结果只提供脱敏、截断后的日志和 session 元数据，不能
返回 API key、完整 prompt、完整命令输出或会话正文。五个 capability root 由各自的
`TypedToolOperation` 实现，provider JSON 只在 `TypedToolAdapter` 边界转换；native Tauri
command 保存同一组 typed request 并调用对应 surface，未注册 broad `haven` dispatcher。
`AdminContext` 只注入 `SessionStore` 与 `MemoryFactStore` 等 capability-scoped typed handles，
不暴露通用数据库 facade；诊断列表和总数由 `SessionStore` 异步端口提供，最近 50 条状态分组、
limit、创建时间倒序和 errors 的 status 过滤顺序保持原样。组合根将 `MemoryFactStore`
同时提供给 `MemoryTool` 和应用 runtime；缺少对应可选 capability 时保留既有 unavailable 行为
（ADR 0306）。
高风险、网络、媒体、文件和跨 session 协作仍保留独立的内部实现边界，以维持各自的确认、
路径、provider 和生命周期边界；模型看到的名称仍遵循点号 view 契约。

### 2.6 `haven-app-binary` —— 组合根 + 宿主边界（Tauri）

- `runtime.rs`：`ApplicationRuntime` 是应用级生命周期 owner，集中持有服务句柄、
  app-scoped task handles 和根 `CancellationToken`；`shutdown`/`teardown` 统一输入、
  session、action、MCP 与 bootstrap worker 的停止顺序。领域 worker 仍由所属 crate
  释放，但必须接收 runtime 子 token 或响应领域 shutdown。
- `app_state.rs`：装配 `AppState`（runtime / 瞬态录音状态 / bootstrap 状态 / UI
  confirmation）；命令通过 runtime 稳定句柄消费 db / router / tools / executor /
  agent / pipeline / shell / `config_service` / media clients / stt_client。
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

聊天 UI 的会话状态 facade 位于 `ui/src/lib/sessionReducer.ts`：它导出稳定的
`SessionReducer` / `reduceSession` API、Observable wrapper 和唯一的 `sessionStateStore`。
状态转换按职责位于 `ui/src/lib/sessionReducer/`：lifecycle、transcript/messages、
interaction、usage、Agent stream，以及共享的 types 和 immutable replay/state helper。
领域模块只接收显式 state/action，不互相导入；facade 保留跨域 resume/clear 组合，
live event、resume、rollback 同步和 reconnect replay 都只通过 typed `SessionAction` 迁移。
`createSessionSelectorStore` 只读订阅同一 writable，按选择值引用门控通知；它不复制 reducer
状态，selector 的最后一个订阅者离开时释放 root subscription（ADR 0322）。
`ui/src/lib/chatController.ts` 负责会话命令的异步编排：
权威 resume reload、interaction 保留、切换与终态会话内存回收、rollback、continue、end/interrupt 和
`submitTranscript` 提交适配；它通过 typed dependency 接收 invoke、reducer dispatch、
session snapshot、通知/错误报告和页面回调，不持有 Svelte state 或 DOM。
`ui/src/lib/chatEventController.ts` 只组合聊天页的 session/app/agent/usage handler map 并拥有
异步注册/释放生命周期；它通过显式 typed dependencies 连接页面 reducer、错误/ask/stream 清理、
session refresh、hotkey 与 model refresh 回调，不持有 Svelte state 或 DOM。`ui/src/lib/events.ts`
是共享 listener registration 和领域 mapper 的调用入口，`chat*EventHandlers.ts` 继续负责既有
事件到页面状态/副作用的适配（ADR 0315）。session lifecycle 的 Rust wire DTO 由
`crates/app-binary/src/events.rs` 中的 `SessionLifecycleEvent`、`SessionErrorEvent`、
`SessionTitleUpdatedEvent` 和 `SessionDeletedEvent` 定义；唯一前端转换位于
`ui/src/lib/contracts/session.ts` 的 `mapSessionEvent`。它把 Rust/Tauri 的 snake_case 字段映射为
handler/reducer 使用的 camelCase，忽略新增 wire 字段；缺失或类型错误的必需字段会 fail closed
并由 listener 层记录。可选 `waiting_reason` / `reason` 缺省映射为 `null`，未知 status 降级为
`error`，未知等待原因降级为 `null`。Rust DTO、channel、payload 与 reducer 语义不变（ADR 0330）。
Action board 与 lifecycle event 共用 Rust `events.rs::ActionEvent` wire DTO：
`ui/src/lib/contracts/action.ts::mapActionPayload` 是其唯一前端运行时 validator/mapper，
`actionStore.refreshActions` 的 command rows 和 `events.ts` 的 action lifecycle listeners 都调用它。
必需 `id`/`kind` 或已声明字段类型无效时丢弃整行/事件；未知附加字段忽略，未知 status 降级为
`failed`，未知 kind fail closed。mapper 不接触 ActionService completion outbox；动态
`tool_args` 仍是执行/完成边界上的 JSON 扩展字段，不进入 `ActionEvent` UI DTO（ADR 0335）。
录音与转写事件已完成镜像审计：Rust `events.rs` 的命名 DTO 是 wire shape 权威；
`ui/src/lib/contracts/recording.ts` 只声明路由消费的 camelCase DTO，并由唯一的
`mapRecordingEvent` 转换。没有第二份 snake_case wire interface，也没有布局内的字段映射；
`recordingEventListeners` 是这组事件唯一进入该 mapper 的 listener 边界。转换保留既有可选字段
省略、畸形值安全默认、未知附加字段忽略与 VAD 字符串透传行为，不拒绝未知 signal/state，
因为 Rust DTO 将它们定义为字符串而非封闭枚举（ADR 0340）。
Settings 的完整 wire shape 由 `haven_common::config::Settings` 所有；前端不再从多个页面直接读取
`invoke('get_settings')` 的原始结果，所有读取经 `ui/src/lib/settingsCommand.ts::loadSettings` 和
`ui/src/lib/contracts/settings.ts::parseSettingsPayload`。validator 只检查根对象，保留未知配置字段、
未知枚举字符串和既有 snake_case 配置字段；null/非对象继续作为空结果，命令错误原样进入现有 catch。
Settings 表单状态仍由 `SettingsView` 持有，`settingsSaveAction` 与 `settingsGuard` 只负责纯 UI 状态，
没有额外的 settings store 或第二个 update serializer。`hotkey:rebind` 事件已由
`ui/src/lib/contracts/app.ts::mapAppEvent` 唯一映射 `old_binding` / `new_binding` 到 camelCase；
`settings.ts` 的诊断 command parsers 继续校验 ADR 0007 的命名响应 DTO。app-shell 事件的批量
`appEventListeners` 与单条 `registerAppListener` 共用 `mapAppEvent` adapter；ToolsView 的 MCP/Skills
刷新监听也经过该入口，布局只拥有 MCP 通知副作用，Skills 不再保留空 listener。Rust MCP status 使用
serde 外部标记 enum，既有 pass-through mapper 不校验其变体，因此可保留未知变体和附加字段；明确投影的 hotkey/interaction 字段仍只输出
已知 camelCase DTO 字段。布局通知与 ToolsView 刷新是不同副作用，不做 event dedup。`SessionResumeResponse`
中的 interactions 仍由原 session resume normalizer 处理。Agent wire DTO 仍由 Rust `events.rs` 定义；
`contracts/agent.ts::mapAgentEvent` 是唯一 runtime validator/mapper，删除重复的 TS snake_case wire
interfaces，忽略未知附加字段并透传 enum-like 字符串与动态扩展值。`agentEventListeners` 对 malformed
payload 记录不含 payload 的 warning 并丢弃；聊天页与布局订阅互不重叠，共用同一 session reducer，通知、
usage fallback 与 media plan 双副作用保持原 owner（ADR 0347）。另保留既有 SessionCompleted/SessionError
主事件加 `session:updated` secondary fan-out；聊天页终态 handler 会重复执行部分 cleanup，跨 channel 没有共享
event identity，本切片不修改 session contract/reducer。其余 app event/command contract mirror、
以及 session mapper 内部 camelCase 类型和字段映射仍待 Phase 8 逐域审计。Action board 的活跃
`list_actions`/`cancel_action` 经 `actionCommands.ts`；list response 复用 `mapActionPayload`，cancel
request/result 使用命名 TS contract，`actionStore` 不直接 invoke（ADR 0348）。其余命令仍按域审计，
不引入全局 codegen。
`+page.svelte` 保留 view/scroll 与 dialog/loading/menu 状态、输入路由与 ask 决策、model sync、
resume target/auto-restore、新会话入口及非 chat-event teardown；在 mount 时创建 controller、等待
listener ready 后再 settings/load/restore，并在 destroy 时 dispose。旧 `sessionMessages.ts`、
`sessionUsage.ts` 仅保留兼容投影，`streamAggregator.ts` 只负责排队后 dispatch chunk action
（ADR 0160、0313、0315、0320、0322）。Phase 8 剩余 Rust DTO 到 TypeScript contract/mapper
generation、旧手写 mirror 清理，以及 ask/input 决策、复杂 view state 与启动恢复的编排边界。
`ModelSettings.svelte` 当前保留命名模型、Provider CRUD 与 discovery 的页面编排，已补
组件行为测试，后续再按 discovery / mutation 边界拆分。

**判定标准**：唯一能同时看到所有 crate 的地方；负责把事件桥到前端、把前端命令调到后端，
不承载业务逻辑。

---

## 3. 易混边界（历史演进遗留，现已收敛）

### 3.1 input、tools 与 llm 的 STT 边界

| | `haven-input` | `haven-llm` |
|---|---|---|
| 角色 | **采集方**：录音 → VAD → PCM/WAV → `RecordingResult` | **provider 适配方**：`LlmClient::transcribe` + `build_stt_client` / `adapter_for` |
| 编排方 | `haven-tools::builtin::media` 的 `MediaTranscriber` 统一专用 STT → LLM fallback | `LlmRouter::transcribe_audio` 只负责选定模型上的 provider-wire/native-to-chat capability fallback；不做 provider/model failover |

用户语音入口和工具资产入口共享同一个 `MediaTranscriber` 策略，不再各自实现转写：应用通过
`ToolsManager::transcribe_recording` 把采集到的 WAV 交给工具边界。云端 STT（Whisper / Groq /
Gemini / Deepgram / AssemblyAI）与 chat 共用 `adapter_for` 分发；`provider = "llm"` 走
`LlmRouter::transcribe_audio`（原生 `transcribe`，否则 multimodal chat 回退）。
MCP STT 仍走独立 `McpSttClient`（依赖 `McpToolCaller`）。两条录音路径的生命周期仍显式
不同：UI 麦克风是 `voice input`，只提交转写文本并用 `rec-*` 关联事件；`media.record` 是
`recorded media asset`，先登记 WAV 并返回可复用的 `asset_id`，再附带转写结果。`record`
只依赖采集管线，转写不可用时仍可保留资产并返回结构化 capability 状态。

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
| 2026-09-25 | §2.5 Tools / §2.6 UI：审计 background/scheduled action board 生命周期投影；复用既有 background terminal transcript finalizer，保留 scheduled 删除、Agent 通知、kind-specific display/cancel 和无 UI event dedup 边界（ADR 0344） |
| 2026-09-25 | §2.5 Tools：穷举审计 background/scheduled ActionStatus 与 terminal claim；已有纯策略 owner 覆盖唯一共享判断，不新增完整 Job transition policy，记录 trigger/execution 与恢复语义的未决决策（ADR 0352） |
| 2026-09-25 | §2.5 Tools：scheduled admission 只回收 terminal 内存 entry，保留 Running row 供 Agent terminal callback、取消与 no-consumer recovery 使用；Waiting 恢复、restart cleanup、CAS 和事件顺序保持（ADR 0353） |
| 2026-09-25 | §2.6 App / UI：ToolsView 的 MCP/Skills 单条订阅统一经过 `mapAppEvent`；保留 MCP 通知与刷新两个不同副作用、未知 status variant 与 pass-through 扩展字段，删除布局无效 Skills listener（ADR 0346） |
| 2026-09-25 | §2.6 App / UI：Agent 事件删除重复的 snake_case TS wire interfaces，并由唯一 `mapAgentEvent` 校验/映射未知 payload；未知 enum 字符串、动态扩展、usage error fallback、空通知默认与各自副作用 owner 保持。记录 SessionCompleted/SessionError 双 channel fan-out 的既有终态 cleanup 重叠，本切片不改 session contract（ADR 0347） |
| 2026-09-25 | §2.6 App / UI：Action board 活跃 `list_actions`/`cancel_action` 统一经过 typed command boundary；list rows 复用 Action mapper，扁平 request 与 boolean result 有命名类型，wire/error/UI 行为保持（ADR 0348） |
| 2026-09-25 | §2.6 App / UI：`get_settings` 读取统一经过唯一 `settingsCommand.ts` 入口与开放式根对象 validator；保留未知配置字段/枚举、原错误处理，hotkey event 继续走既有 camelCase mapper，不改 Rust DTO 与保存顺序（ADR 0341） |
| 2026-09-25 | §2.6 App / UI：录音与转写事件审计确认 Rust DTO 是 wire 权威，前端只保留 camelCase 消费 DTO 和单一 mapper；补充未知 VAD 字符串、扩展字段、畸形默认、channel 集合与到达顺序回归覆盖，无 DTO 或生产逻辑变化（ADR 0340） |
| 2026-09-25 | §2.2 LLM：审计 health/native transcription descriptor 边界；两者已在 route/permit 前使用同一语义映射，metadata/config helpers 是只读 route 查询，新增 contract tests，无无效 wrapper（ADR 0339） |
| 2026-09-25 | §2.5 Tools / UI：ActionService 持有唯一共享 tail 长度策略，foreground/background 消费 bounded typed snapshots；两条既有 event identity/wire 与 terminal/outbox 时序保持（ADR 0338） |
| 2026-09-25 | §2.5 Tools：scheduled trigger 输入分类与 due-time 计算收口到纯 typed policy；ActionService 保留配置读取、durable admission、timer/fire 与终态，scheduled execution 仍由 AgentLayer/tool runner 承担（ADR 0343） |
| 2026-09-25 | §2.5 App 配置：Settings runtime apply 的有序阶段与 phase/failure 元数据归 `SettingsRuntimeApplyCoordinator`；现有副作用 owner、Router prepare/publish、no-op 与半失败语义保持，补偿/rollback 仍未决（ADR 0337） |
| 2026-09-25 | §2.5 Agent / §2.3 Memory：ReAct transcript 通过 `SessionCommitted` 提交事件与 domain projection intent；SessionStore 事务内先写 event 再写物化行、提交后广播，Agent 按 sequence 发布 UI（ADR 0336） |
| 2026-09-25 | §2.5 Tools：由 crate-private `ToolRuntimeCoordinator` 持有 tool runtime composition、PlatformRuntime/config 更新顺序、MCP discovery index 与 builtin catalog rebuild；Tauri config gate、MCP 连接和应用 shutdown 仍留在原 owner（ADR 0333） |
| 2026-09-25 | §2.5 Tools：background/scheduled 终态持久化修复共用纯 `ActionPersistenceRetryPolicy`；无 deadline/预算、1 秒起步与 30 秒封顶保持，CAS/outbox/timer rollback 和执行行为仍归各路径（ADR 0334） |
| 2026-09-25 | §2.5 Tools / §2.3 Memory：background outbox 与 scheduled fire claim 共用纯 typed `ActionLease<T>` 状态校验；30 秒/15 分钟 lease、SQLite CAS、ack 和 terminal invalidation 保持各自边界（ADR 0332） |
| 2026-09-25 | §2.5 Tools：ToolsManager 唯一构造 typed `ToolCapabilitySnapshot` 并供媒体 catalog、prompt、TTS/STT 与录音 gate 共用；每次从当前 runtime/router/MCP 状态重建，暂缓无共同失效时钟的缓存（ADR 0331） |
| 2026-09-25 | §2.2 LLM：将聚合 stream 的首次 `on_chunk` 交付前重试、guidance retry、总 timeout 与结果交接收口到 `AggregatedStreamExecutor`；Router 继续持路由、permit、规则和 health/cooldown 状态（ADR 0328） |
| 2026-09-25 | §2.2 LLM：将显式 `RequestDescriptor` 从 Router route preparation 传入 complete、embedding、raw stream 与 aggregated stream 执行边界；route key 和 usage owner 保持原边界（ADR 0329） |
| 2026-09-25 | §2.6 LLM：`ModelDirectory` 接管 provider client map、primary routes、client/model 选择及 capability/endpoint metadata 查询；Router 继续拥有单一 config snapshot 与执行状态（ADR 0316） |
| 2026-09-25 | §2.6 UI：聊天页 handler map 组合与 listener 生命周期由 typed `chatEventController` 持有；`events.ts` 保持唯一 wire mapper 和注册 primitive（ADR 0315） |
| 2026-09-25 | §2.6 UI：将 `SessionReducer` 按 lifecycle、transcript、interaction、usage、Agent stream 拆为内部模块；原 facade、公共导出路径和单一 store 订阅保持不变（ADR 0314） |
| 2026-09-25 | §2.6 UI：将会话切换、rollback、continue、end/interrupt、resume reload 与提交编排提取到 typed `ChatController`；页面继续拥有 ask/input、事件、model sync、恢复入口和视图状态（ADR 0313） |
| 2026-09-25 | §2.3 Memory：summary cursor 与共享 extraction throttle 经 MemoryFactExtractionStore；生产 MemoryWorker 不再使用 raw Database，MemoryService 私有句柄只用于 typed store/index 实现（ADR 0312） |
| 2026-09-25 | §2.3 Memory：LLM 矛盾候选/demote 与 predicate count/逐项 rewrite 经 MemoryMaintenanceStore；Agent 保留配置、prompt、解析、候选与安全 gate、日志和降级（ADR 0311） |
| 2026-09-25 | §2.3 Memory：facts 确定性维护、孤儿 embedding/cursor 和 source ref 清理由 MemoryMaintenanceStore 按原顺序逐项调度；Worker 保留 best-effort 继续、计数/日志与聚合错误，LLM 维护和 summary cursor/throttle 留在原边界（ADR 0310） |
| 2026-09-25 | §2.3 Memory：事实批量存在性检查、upsert 与 source ref 持久化经 MemoryFactStore 在一个 blocking closure/事务内完成；Agent 保留候选清洗与置信度策略，失败回滚整批（ADR 0309） |
| 2026-09-25 | §2.3 Memory：普通 session 事实抽取的 transcript、节流戳和游标通过 MemoryFactExtractionStore 持久化；Worker 仍保留事实批量写入、维护和摘要抽取相关 MemoryDatabase 路径，索引 catch-up 沿用 MemoryEmbeddingStore（ADR 0308） |
| 2026-09-25 | §2.3 Memory / §2.5 Agent：MemoryWorker 的已知事实 prompt 读取经 MemoryService 共享的 MemoryFactStore 有界端口；过滤、顺序和 limit 收口在 Memory，prompt 格式与字段清洗保持不变（ADR 0307） |
| 2026-09-25 | §2.5 Tools：admin capability 通过 SessionStore / MemoryFactStore typed handles 注入；诊断行为与 provider wire contract 保持不变（ADR 0306） |
| 2026-09-25 | §2.3 Memory / §2.5 Tools：ActionService 所有 action 持久化改经窄异步 ActionStore；保留 SQLite CAS/outbox 事务、内存生命周期和 headless 行为（ADR 0305） |
| 2026-09-23 | §2.5 Agent：热 transcript 的主人定为 actor 内的 `SessionState`；一次 run 在 actor 任务内执行，只在 yield 点借用状态。usage、stream id、token estimate 是函数调用，不是 mailbox 命令。当前循环仍在 actor 外，迁移必须一次跨过这条边界（ADR 0214） |
| 2026-09-23 | §1 Tools：`OperationSpec` 成为运行时策略与 manifest 的唯一来源；`OperationContract.read_only` 不再豁免确认，manifest 向运行时收紧（ADR 0213） |
| 2026-09-23 | §1 Tools：进程服务改为 `ToolServices`，不再从 `ToolsManager` 取 MCP/skills/action；`OperationSpec` 只覆盖 builtin view，IPC 形状与交互式确认边界保持不变（ADR 0212） |
| 2026-09-23 | §2.5 Tools：`ToolsManager` 收成执行 facade；operation 由 `OperationSpec` 投影 manifest，平台客户端按 `PlatformRuntime` 整份替换（ADR 0211） |
| 2026-09-23 | §2.5 Agent / §2.6 UI：durable UI 事件在 `session_events` 提交成功后由 `CommittedUiPublisher` 按 sequence 发布；Thought 携带 `event_seq`，并行卡片按 `(eventSeq, identity)` 去重。不升 schema（ADR 0210） |
| 2026-09-22 | §2.5 Tools：按组合 wiring、catalog/session discovery 与 execution 入口拆分 `ToolsManager` 实现，并将 manager 回归测试移出 crate root；公共工具、授权、IPC 与持久化契约不变（ADR 0205） |
| 2026-09-22 | §2.3：删除 `sessions.react_state`，schema v28；旧库删除重建，不迁移（ADR 0209） |
| 2026-09-22 | §2.3/§2.5：删除 `react_checkpoints`，生产恢复只 replay `session_events`；turn 终态和工具批次 durable 提交都经 `EffectBatch`；inbox 轮询状态进入 `SessionState`（ADR 0196/0208/0209），schema v27 |
| 2026-09-21 | §2.3 Memory / §2.5 Agent / Common / UI：首轮 system prompt 不再等待 embedding，记忆改为有界后台预取并通过 MEMORY fence 补入；收紧默认上下文、输出、观察、工具与 reasoning 回显预算（ADR 0191） |
| 2026-09-20 | §2.5 Agent / §2.6 UI：工具实时预览移出 SessionReducer，避免输出 tick 重算整条时间线；运行中的停止/结束立即返回，终端清理延迟到 run-exit 边界，删除/清空仍保留 destructive cleanup fence（ADR 0184） |
| 2026-09-19 | §2.5 Agent / §2.6 UI：Action board 刷新加入状态版本校验；损坏 waiting scheduled row 增加可取消的指数退避隔离重试；scheduled fire 改为服务级 claim/lease，阻止多 receiver 重复执行（ADR 0174） |
| 2026-09-19 | §2.3 Memory / §2.5 Agent / Tools / App / UI：将 session 与 action 状态下沉为 Common typed lifecycle；删除 `actions.fired` 与 `scheduled` 状态，统一定时任务取消/触发终态和 IPC 投影（ADR 0172） |
| 2026-09-19 | §2.2 Common / LLM / App / UI：将固定 five-slot 模型配置和 STT/vision 布尔开关改为命名模型、`Capability` 与 `RequestPolicy` 路由；旧 `llm.roles` 在加载时一次性转换，provider adapter wire 契约保持不变（ADR 0170） |
| 2026-09-19 | §2.5 Agent / §2.6 UI：以精确 active-run admission、单 dispatcher、生命周期闸门和 quiesce-then-mutate 收口并发会话；UI 提交改为按 session 分 lane，草稿 lane 保持串行接管（ADR 0171） |
| 2026-09-19 | §2.2 LLM：将 OpenAI Chat、Responses、Anthropic 与 Gemini provider adapter 按 wire、request、response、stream、mapping、features 与 provider-local golden fixtures 拆分；保持外部 wire、共享 transport/framing 与 LlmClient 边界不变（ADR 0169） |
| 2026-09-15 | §2.5 Security / Tools / Agent / App：由 `AuthorizationEngine` 统一承载 typed `AuthorizationRequest`、`AuthorizationDecision` 与 `CapabilityScope`；scheduled、MCP、skill、Tauri/UI confirmation 共用同一请求与 receipt 校验路径（ADR 0163） |
| 2026-09-15 | §2.6 App：引入 `ApplicationRuntime` 统一服务句柄、后台任务 owner、根取消 token、退出 shutdown/teardown；输入、session、action、MCP 和 bootstrap worker 按依赖顺序停止，pending scheduled action 保留恢复语义（ADR 0161） |
| 2026-09-15 | §2.6 UI：以 typed `SessionReducer` 统一 live event、resume、rollback/reconnect replay、Interaction、usage 与 optimistic 状态；旧消息/用量 store 降为兼容投影（ADR 0160） |
| 2026-09-15 | §2.3 Memory / §2.5 Agent：新增版本化 `session_events` append-only 事件流与 `SessionEventStore`；resume、rollback、transcript 投影与 live replay 共用 durable sequence（ADR 0159，后由 ADR 0196/0207 收口） |
| 2026-09-12 | §2.5 Tools / Agent / App：删除 `MediaGateway`、coverage、intent 与 ingress eager preprocessing；由单一共享 `MediaTool` 统一 OCR、STT fallback、文档抽取和显式媒体生成，并同步 UI 媒体结果契约（ADR 0130） |
| 2026-09-12 | §2.5 Tools / UI / Security：删除独立 `audio` 模型工具，将录音、播放、TTS、音量和静音纳入 `media` operation 分支；旧 audio 配置/权限按测试版策略重置（ADR 0133） |
| 2026-09-12 | §2.5 Tools：按公共契约、媒体引用、内容派生、生成/资产登记和测试职责拆分 `media` 内部模块；模型入口与运行时行为不变（ADR 0134） |
| 2026-09-12 | §2.5 Common / LLM / Tools / Agent / UI：统一媒体探测、视频 raw 表示、MediaResult 外壳和 STT MediaPlan 投影；区分 voice input 与 recorded media asset，并拆出文件分类/handoff 与路径资产模块（ADR 0136） |
| 2026-09-12 | §2.5 Agent / Tools / LLM：统一 producer→asset_id→media consumer；files rich path、window OCR、录音均收敛到同一资产链，并在模型请求中说明 MediaPlan 表示（ADR 0129） |
| 2026-09-12 | §2.5 Tools / Agent / Common：按实时路由能力裁剪媒体与录音 operation；structured-first observation 保留恢复字段；仓库会话默认工作区路径；区分只读重试安全性并补充 runtime capability snapshot（ADR 0128） |
| 2026-09-12 | §2.5 Tools / Agent：补充 model-facing schema 压缩、可恢复文件读取与 `files.outline`、能力过滤及显式 memory-empty 语义；保持聚合工具公共名称不变（ADR 0127） |
| 2026-09-12 | §2.5 Tools / LLM / Agent / UI：完成 P1 operation view、搜索/outline 结构化模型视图、文档页游标、原生视频 ContentPart、session-scoped 偏好/清单和 memory 空结果诊断；按测试版 reset 边界删除 FollowUp/confirmation/ask/rollback/provider-style 内部兼容层（ADR 0131） |
| 2026-09-13 | §2.5 Tools / Agent / UI / Security：模型与 UI 统一使用 `root.operation` 点号 view；files/system/haven/media 及其它 operation-based builtin 不再以聚合根注册，启用 Skill 直接注册，删除 `load_skill`（ADR 0137） |
| 2026-09-14 | §2.5 Tools / Agent：将 provider-facing 工具定义改为核心常驻 + builtin/Skill/MCP 按 session 分层加载；新增 `load_skill`，保留 `load_mcp`，完整 schema 只进入当前 session（ADR 0145） |
| 2026-09-14 | §2.5 Tools / Agent：提示词只保留 `system` / `agent` / `haven` 等第一层 family/root 摘要；新增 `tool_catalog` 提供分页的 family/root/operation 发现与精确 schema 查询（ADR 0148） |
| 2026-09-14 | §2.5 Tools / UI / MCP：工具页按 `ToolManifest.identity` 实现 family/root/operation 三级树；复核并统一 MCP 渐进连接的目录版本监听，保证 `tools/list_changed` 使分页 cursor 失效（ADR 0148） |
| 2026-09-21 | §2.5 Tools / Agent：稳定核心 provider surface 固定保留目录/加载、Skill/MCP loader 与少量高频读取工具；`tool_catalog` 的 `load` action 收口 builtin 发现与加载，删除独立 `load_builtin`（ADR 0198） |
| 2026-09-13 | §2.6 UI：工具页将同一 operation root 收束为一张可展开卡片，保留每个 operation 的独立 Schema、风险和启用状态（ADR 0139） |
| 2026-09-10 | §2.5 Tools：将 PDF/DOCX/XLSX/PPTX 的受限本地抽取收口到 `document.rs`，经受管 `files` read 返回有 provenance 的不可信派生表示（ADR 0114） |
| 2026-09-14 | §2.6 UI：聊天页会话列表、选择和错误恢复通过 `SessionReducer` 单一迁移入口；事件适配层仅保留消息/流式清理与其它副作用，ModelSettings 补齐拆分前组件测试（ADR 0154） |
| 2026-09-02 | §2.5 Tools：将 Tool contract、registry/catalog 与 AuthorizationEngine 拆分为 `tool_contract.rs`、`registry.rs`、`security.rs`，直接迁移 workspace 调用点并保持安全/执行契约不变（阶段 D） |
| 2026-09-15 | §2.5 Tools：五个受限 Admin surface 全部迁移到 TypedToolOperation；native Tauri 请求改为 typed request，删除旧 broad dispatcher 及其参数/操作类型（ADR 0071） |
| 2026-09-02 | §2.5 Tools：haven_config 完成首条 TypedToolOperation 切片，typed metadata 与 provider JSON adapter 分层（ADR 0071） |
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
| 2026-09-22 | §2.5 Agent / §2.6 UI：保留持久化 `Paused`，通过 `waiting_reason` 派生等待原因；直接 UI confirm 也复用 `InteractionRequest` 和统一 renderer projection（ADR 0200） |
| 2026-09-14 | §2.5 Agent / §2.6 UI：ask、confirm、scheduled confirm 统一为 `InteractionRequest`；session 只保留通用 `Paused`，Tauri 使用 `interaction:requested`，前端统一由 `interactionStore` 投影与恢复（ADR 0156） |
| 2026-09-14 | §2.5 Agent/Tools：`MessagingService` 接入 `SessionActor` mailbox；同进程优先 actor、跨进程 fallback JSONL；统一 request/reply/receipt/ack/retry/expiry，并以 typed `MessagingRuntime` 取代 spawn/lifecycle callback（ADR 0158） |
| 2026-08-30 | §2.6 UI：将 session 生命周期事件的状态投影与终态清理收口到 `ui/src/lib/chatSessionEventHandlers.ts`，路由页保留响应式状态回调（ADR 0047） |
| 2026-08-30 | §2.6 UI：将欢迎态、消息列表、后台等待提示与错误继续按钮收口到 `ui/src/lib/ChatMessageTimeline.svelte`，路由页保留滚动容器与业务回调（ADR 0048） |
| 2026-08-30 | §2.6 UI：将 ask 选项选择、批量回答、忽略、恢复清理与重复提交防护收口到 `ui/src/lib/chatAskInteraction.ts`，路由页保留输入编排（ADR 0049） |
| 2026-08-30 | §2.2 LLM：将 endpoint 健康、熔断状态机、role 索引与健康槽位初始化收口到 `crates/llm/src/endpoint_health.rs`，router 保留并发存储与请求时机（ADR 0051） |
| 2026-08-30 | §2.6 UI / 持久化：删除已到期的 `stores.ts` 消息/用量兼容 re-export 与未压缩 ReAct snapshot 读取回退；旧数据按发布说明重置（ADR 0052） |
| 2026-08-30 | §2.2 LLM：将流式上下文估算、idle scaling、规则门禁、chunk 聚合与首 chunk 前重试收口到 `crates/llm/src/streaming.rs`，router 保留 endpoint 编排（ADR 0053） |
| 2026-08-30 | §2.4 Agent：将增量事实抽取窗口、transcript 构造、来源解析与提案安全门禁收口到 `crates/agent/src/fact_inference.rs`，inference 保留调度与写入编排（ADR 0054） |
| 2026-09-22 | §2.3/§2.5：统一事实存储表为 `facts`、后台编排入口为 `MemoryWorker`，删除 `InferenceEngine` 兼容别名；旧数据库按 schema v26 重置（ADR 0201） |
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
| 2026-09-02 | §2.5 Tools：将模型可见的 `haven` 管理入口收窄为五个 capability-scoped admin tools；删除任意 dotted `config_set`，诊断结果增加脱敏与内容边界（ADR 0070） |
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
| 2026-09-22 | §2.3 Memory / §2.6 UI：删除只在创建时写入的 `sessions.transcript` 快照列；会话正文搜索改由 `messages` 投影，恢复继续使用 `session_events`，数据库升至 v25，按发布说明重置（ADR 0199） |
| 2026-09-21 | §2.3 Memory：消息表将旧 `attachments` 兼容列改名为 `ui_metadata`；canonical 媒体、表示和恢复只使用 `media_inputs`，UI 元数据仅保留展示/资产保留字段；数据库升至 v24，按发布说明重置（ADR 0197） |
