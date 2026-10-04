# Haven 架构与 crate 职责

> 版本: v1.11 | 日期: 2026-10-04
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
├── haven-memory（持久化）
├── haven-llm（模型与媒体 provider）
├── haven-common（跨层契约）
└── haven-platform（操作系统适配）

haven-platform ──► haven-common（credential store 端口）

haven-agent ──► haven-tools, haven-memory, haven-llm, haven-common
haven-tools ──► haven-input, haven-mcp, haven-memory, haven-skills, haven-llm, haven-common, haven-platform
haven-mcp   ──► haven-llm, haven-common, haven-platform
haven-input / haven-llm / haven-memory / haven-skills ──► haven-common
```

实际依赖（见各 `Cargo.toml`）：

| crate | 依赖 | 说明 |
|---|---|---|
| `haven-common` | 无内部依赖 | 纯叶子，全 workspace 共享 |
| `haven-platform` | common | CredentialStore 端口与引用校验；Windows 凭据管理器和子进程适配 |
| `haven-llm` | common | 只依赖共享层，不依赖任何业务 crate |
| `haven-memory` | common | 持久化（当前 SQLite schema、历史迁移、仓库） |
| `haven-skills` | common | 技能目录解析 |
| `haven-mcp` | common, llm, platform | MCP 客户端 / 传输（媒体能力复用 LLM 协议） |
| `haven-tools` | common, memory, skills, mcp, llm, input, platform | 工具注册表 + 各内置工具 |
| `haven-input` | common | 录音 / VAD / PCM/WAV 采集（不实现 provider 或转写） |
| `haven-agent` | common, llm, memory, tools | ReAct 循环 + 会话执行 |
| `haven-app-binary` | agent, common, input, llm, memory, tools, platform + tauri | 装配 + Tauri 命令 + 事件桥 |

> 依据 `crates/*/Cargo.toml` 实际 workspace 依赖整理。`haven-agent` 与 `haven-app-binary` 是最上层，
> 其余全部是它们的底层依赖。`haven-llm` 不允许被业务 crate 反向依赖。
> 语音转写的运行时调用路径是 app → tools → llm；`haven-input` 只产出采集结果，不直接依赖 `haven-llm`。

`haven-platform` 只依赖 common 中稳定的 `CredentialStore` 端口与 credential reference validator，
不依赖 Tools、MCP 或 Tauri。Windows adapter 用 Credential Manager 保存密钥；其他平台对带凭据配置
明确失败，不退回明文或进程内持久化。该 crate 还拥有 MCP stdio、Shell、Skill 和后台 Action
子进程共用的 `ProcessContainment`：Windows 使用 kill-on-close Job Object，其他平台保持原有 no-op
行为。进程创建和取消时机仍由各自 adapter 所有；此 crate 只封装 OS handle 生命周期。

`haven-mcp` 内部按职责分为 `protocol.rs`（MCP/JSON-RPC DTO 与内容归一化）、
`transport.rs`（stdio、Streamable HTTP、SSE 和进程边界）、`client.rs`（单服务器连接、
限流、重连与健康监控）和 `manager.rs`（多服务器 reconcile 与 LLM caller 适配）；
`lib.rs` 只保留模块声明和公共导出，`sse.rs` 保留为 SSE parser。

`haven-tools` 的工具核心按稳定边界分为 `tool_contract.rs`（Tool、ToolResult、typed
operation 与执行策略）、`registry.rs`（全局注册表、SessionCatalog、版本快照与 probe）和
`security.rs`（AuthorizationEngine、权限继承、disabled operation、路径沙箱与本机安全矩阵）。
在这组稳定模块之上，`OperationRegistry` 持有已安装、deferred 与 session operation；
`OperationCatalog` 是模型可见投影；`AuthorizedExecutor` 做熔断、启用检查、校验、执行和结果分类，
并把经幂等性与 retryability 筛选的工具失败交给 `haven-common::retry::RecoveryPolicy` 决定退避与 attempt budget（ADR 0447）。
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
coordinator 不复制 Router prepare/publish，也不为半失败状态增加 compensation/rollback。审计确认无第二份配置 owner、target mapping 或 Settings payload builder；durable edit 后不增加 compensation/rollback 或显式 retry/restart；失败保留磁盘配置并报告部分 apply failure，重启从磁盘初始化。失败至重启前普通会话和工具调用继续使用各自 runtime owner 当前状态，不等待重启，也不保证所有 consumer 处于同一配置 revision；turn catalog 保持不可变 snapshot，authorization 和执行仍由 live owner 决定（ADR 0372）。`SettingsView` 唯一的 `update_settings` payload builder 使用开放式 `SettingsPayload`，IPC script 校验 Rust `Settings` 参数和 UI 直接调用 owner，而完整字段 schema 仍由 Rust 所有（ADR 0372）。Tools admin 的 MCP/Skills/Logging/ToolSettings writers 若修改与 Settings/model 相同的运行时配置域，必须进入统一串行配置边界；不相关域仍由原 owner 负责。实现跟进由组合根创建共享 gate，并经窄 AdminContext 注入 Tools AdminServices；gate 覆盖 durable edit 与 live apply/rebuild，Tauri wrapper 不重复 catalog rebuild。MCP Tauri 管理入口将写入委托给 AdminServices；
add/update/toggle/remove 的持久化、live 连接和 catalog rebuild 均在 AdminServices 的共享 gate 内完成。renderer 的
refresh/reconnect 也先经 `AuthorizationEngine`；typed native request 不进入模型可见 MCP schema。Refresh 授权后仅在
`AdminServices` 内按共享配置 gate 复核版本与完整 diff，reconnect 复核单个已启用 server 与 live client，再执行对应
连接路径。Refresh 的 connect/disconnect status 经 manager channel 投影；直接 reconnect 成功后由 ToolsView 刷新快照，
待确认 reconnect 则由 resolver finalizer 发布当前状态。待确认 refresh 的 partial failure 由 confirmed-only finalizer
筛选本批授权 target 后，经现有 status channel 发布通用 Offline 状态；直接 refresh 继续通过原响应 DTO 返回批次结果，
不会重复发失败事件。连接及其 `catalog_version` 仍由 `McpManager` 持有。
应用退出顺序由 `ApplicationRuntime` 负责，coordinator 不增加独立 shutdown 生命周期
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
- `retry.rs`：纯恢复策略模型。调用方提供错误分类、尝试次数和当前单调时钟，获得继续/停止决策与退避时间；不执行 sleep、取消、队列、持久化或任务生命周期。
- `encoding.rs` / `text.rs`：编码解码（UTF-8 → GBK 回退）、文本工具。

**判定标准**：凡被 ≥2 个 crate 共享、且不依赖任何业务逻辑的纯数据/纯函数，放这里。
OS 句柄和进程生命周期适配不属于该共享契约面，统一归 `haven-platform`。

### 2.2 `haven-llm` —— 模型与媒体能力的唯一实现方

- `adapters/`：按 **`api_style`（线协议）** 分发的 provider 适配与统一 `LlmClient` +
  `with_retry`。LLM error 分类留在 `haven-llm`，attempt/backoff/stop 决策复用
  `haven-common::retry::RecoveryPolicy`（ADR 0447）。能力矩阵见 `adapters/capabilities.rs`：
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
  `Capability`；用途到能力的映射复用 `RequestKind::required_capability()`。`RequestKind` 同时是
  当前逻辑请求类型和 public 配置 route key；不增加一一对应的 public `CallPurpose`。Router 将同一
  descriptor 传至 complete、embedding、raw stream 与 aggregated stream 执行边界；health check
  与 native transcription 也在 Router route/permit boundary 从原 `RequestKind` 构造 descriptor。
  health adapter 调用和已选 client 的 native `transcribe` 不再推导 route capability；STT fallback
  以独立 `AudioChat` purpose 重新进入 aggregated route。usage owner 仍由调用方表达（ADR 0319、0329、0339）。
- 聊天页模型选择器与设置页共用 `llm.request_policies`：菜单展示声明了 `chat` 能力的具名
  `ModelConfig`，提交其配置 ID 并更新 `RequestKind::Chat` 的 `primary`；Provider 的服务模型
  ID 只由模型配置编辑器维护。切换因此改变全局 Chat 默认路由，而不就地改写模型配置（ADR 0437）。
- `model_directory.rs`：crate-private `ModelDirectory`，从 Router 的配置 snapshot
  构造 provider client map 与以 `RequestKind` 为原 key 的 primary route；key 唯一保存 route
  purpose，route value 保存 capability 与 model id，执行解析核对 descriptor capability 并 fail closed。它还集中 client/model
  选择、capability profile 和 endpoint/context-window metadata 查询；生产路由同时要求
  所需 `Capability` 与可用凭据，测试注入只跳过凭据过滤。metadata 借用 Router 的单一
  `RouterConfig` snapshot，不复制配置真源。配置/metadata helper 继续接收 `RequestKind` 并
  经 `RouterConfig::route` 校验 route，不调用 provider 或投影 usage/health；`capability_profile`
  只读取已选 adapter 的本地 wire profile。`connection_status` 与 `prewarm_all` 是明确的健康
  probe，会调用 health check 并按既有规则投影 outcome（ADR 0316、0329、0339）。探测遇到
  open circuit 时返回 `circuit_open` 分类并以 debug 记录，因为该路径没有访问 provider；
  用户对错误/暂停会话执行 Continue 时，仅清除所选聊天模型的连续失败熔断门槛，保留
  rate-limit cooldown 与历史调用计数（ADR 0421）。
- `router.rs`：`LlmRouter` 保留配置 snapshot 与请求执行状态，拥有 route/client 选择、
  health/circuit、rate-limit cooldown、semaphore 与 stream rules，并为执行器提供配置
  snapshot 和 health/rate-limit outcome closure。每个 request kind 仍只走唯一 primary；
  同一模型内重试耗尽后直接返回错误，不跨 provider/model 切换缓存命名空间（ADR 0192）。旧 `llm.roles` 仅在
  配置加载时转换，不进入生产路由；`CallExecutor` 执行 complete/embedding，`StreamExecutor`
  只执行 raw stream 建流与 permit 包装；`AggregatedStreamExecutor` 执行聚合流状态机。
  执行器已接收显式 descriptor。public request DTO 继续以 `RequestKind` 表达兼容 route key，且
  descriptor 的 `purpose` 仍是 `RequestKind`；审计确认这是 route purpose 的同一权威值，不需要
  额外 public 类型。health/native transcription 已在路由边界使用 descriptor，无需再传进不拥有
  route 语义的 adapter。`LlmCallKind` usage owner 由 Agent/Tools 调用方显式设置；相同 route 可以
  有不同 owner，因此 Router 不推断也不接收该字段（ADR 0339、0354）。
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
- `registry.rs` / `stream_rules.rs`：模型注册表与可显式配置的流式规则；生产 router 默认不拦截代码块。

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
批处理和维护门控（ADR 0021、0303、0304）。组合根 `AppState` 在已有 Database、Router
和 `ContextLimitsConfig` 后创建唯一 `MemoryService`，按配置中的 `embedding_chunk_size`
初始化并注入 `AgentLayer::build`；该构造返回 `AgentStartup { agent, memory_startup }`。
AgentLayer 从同一 service 派生 stores、共享的 `MemoryWorker` 与 `SystemPromptBuilder`；
`MemoryStartup` 持有唯一 `MemoryRuntime`，并由 ApplicationRuntime 接管长期所有权。Prompt-memory
cache、embedding index 与 worker memory capability 仍属于同一服务实例（ADR 0364、0367）。
`SystemPromptBuilder::with_memory_service`
是唯一公开 builder 构造入口；它不从 Database 创建额外的 MemoryService，因此不产生独立的
prompt-memory cache 或 embedding index（ADR 0365）。`MemoryService` 构造并持有共享的
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
`MemoryFactExtractionStore` 读写。App composition root 创建 typed stores 后注入 `MemoryService`；生产 `MemoryService` 不接收或保留 backing `Database`，测试的 Database-backed 构造仅在 `cfg(test)`。embedding catch-up 与
LSH lagging 检查沿用 `MemoryService` 的 `MemoryEmbeddingStore` 边界（ADR 0383）。
`memory_worker.rs` 只编排事实抽取、durable outbox、维护、提案提交和索引 catch-up；
`MemoryWorker` 是事实抽取和 maintenance pass 的后台执行编排入口。`prompt_context.rs`
在 turn 边界取得一次工具/运行时/记忆快照，`prompt_renderer.rs` 以纯函数渲染 system
message 与 MEMORY fence，不访问 DB、router 或 cache。事实抽取 outbox 以 `kv_store`
marker 持久化，不把 provider 网络调用下沉到 Memory；事实维护的 SQL 清理与矛盾候选
读取由 `fact_maintenance.rs` 负责，`MemoryMaintenanceStore` 提供确定性与 LLM 维护 persistence
操作的异步 typed 边界；maintenance pass 步骤编排、LLM 仲裁、提案门禁与并发控制仍属于 Agent（ADR 0022、0063、0169、0310、0311）。
`MemoryRuntime` 负责 startup cursor/replay、committed-event live consumer 和六小时维护 schedule policy（ADR 0263、0267）。`AgentLayer::build` 仅在组合过程中创建它，并通过 `AgentStartup` 将唯一 `MemoryStartup` 交给 ApplicationRuntime；AgentLayer 只保留同一个 `MemoryWorker` capability。AppRuntime 注册 prepare/startup、live consumer 与 maintenance tasks 并负责 cancel/join。`PreparedMemoryRuntime` 按值消费 prepared receiver；AppRuntime 只有在 live task 注册成功后才将 `MemoryReady` 交给 `AgentLayer::start_after_memory_ready`。prepare/replay 失败或取消时 dispatcher 不启动（ADR 0367）。

### 2.4 `haven-input` —— 输入采集与语音生命周期

- `capture/`：CPAL 采集线程 + 环形缓冲 + 重采样。
- `vad.rs`：tract ONNX 语音活动检测（含常驻 worker 线程）。
- `lib.rs` 的 `InputPipeline`：录音状态机（start / stop / cancel）、VAD 判定 →
  自动停止、PCM/WAV 序列化和采集侧错误。
- `hotkey.rs`：快捷键字符串解析为中性 `KeyCombo`（与平台解耦）。

**判定标准**：管「何时/怎么采」——录音生命周期、VAD 和音频产出；**不实现** provider
调用、转写或 fallback。

### 2.5 `haven-agent` —— ReAct 编排与会话执行

- `react/`：ReAct 循环（`loop` / `turn` / `effects` / `response_cycle` / `stream_step` / `tool_batch` / `tool_batch_execute` / `tool_batch_policy` / `tool_batch_plan` / `context` / `inject` / `turn_end` / `event_boundary` / `retries` / `hooks` / `hook_policy` / `committed_ui` / `transcript` / `state` / `request_context`），按 Run → Turn → ToolBatch 分层；`ReActState` 统一表示当前 run 的 events、canonical、branch points、retry nudge 和 turn cancel，所有边界共享同一运行态。`SessionActor` 为每次 run 创建局部 `ReActState`，将其保存在 actor 持有并轮询的 active-run future 内；actor loop 同时处理该 future 与 mailbox 命令，因此没有 actor 外的 ReAct 循环。`ReActState` 是单次 run 的投影 scratch，不与其它 session 共享；当前它保存在 run future 内，不是 `SessionState` 字段。`SessionState` 持有会话元数据、队列、交互与 messaging 状态。[ADR 0214](adr/0214-react-run-inside-session-actor.md) 记录该单 actor-task 边界及其 mailbox 约束。`loop` 只负责 run 预算、生命周期和按序应用 `EffectBatch`，`turn` 负责模型阶段编排并产出 effect batch，`effects` 是 transcript、branch point 和 pause 的唯一按序应用边界；turn 终态与工具批次的 durable 提交都走这里，turn-start 注入和 stream chunk 仍留在各自边界，`response_cycle` 负责不完整工具参数 JSON 的有限重试；空响应与模型非正常结束会保留部分输出并转为可继续生成的错误，`tool_batch_plan` 固化 assistant 调用顺序和跨层身份，`tool_batch_execute` 负责批次准入、并发执行、取消与每项结果的即时 durable 提交，`tool_batch_policy` 负责失败分类与重试提示，`tool_batch` 在批次完成后按 assistant 调用顺序更新 canonical transcript，并负责工具执行原语、确认生命周期与结果状态。`RequestContext` 从 durable canonical 生成不可变的 provider 请求视图，统一承载 sanitize、retry nudge 和一次性重试指令，不反写 transcript；`context` 只收集有边界的上下文项，`inject` 只经 `apply_transcript` 投影，`turn_end` 只组装最终 effect batch，`event_boundary` 负责事件流完整性与生命周期边界，`hooks` 只定义扩展契约，`hook_policy` 装配生产副作用策略。
- `react/`：ReAct 循环（`loop` / `turn` / `effects` / `response_cycle` / `stream_step` / `tool_batch` / `tool_batch_execute` / `tool_batch_policy` / `tool_batch_plan` / `context` / `inject` / `turn_end` / `event_boundary` / `retries` / `hooks` / `hook_policy` / `committed_ui` / `transcript` / `state` / `request_context`），按 Run → Turn → ToolBatch 分层；`ReActState` 统一表示当前 run 的 events、canonical、branch points、retry nudge 和 turn cancel，所有边界共享同一运行态。`SessionActor` 为每次 run 创建局部 `ReActState`，将其保存在 actor 持有并轮询的 active-run future 内；actor loop 同时处理该 future 与 mailbox 命令，因此没有 actor 外的 ReAct 循环。`ReActState` 是单次 run 的投影 scratch，不与其它 session 共享；当前它保存在 run future 内，不是 `SessionState` 字段。`SessionState` 持有会话元数据、队列、交互与 messaging 状态。[ADR 0214](adr/0214-react-run-inside-session-actor.md) 记录该单 actor-task 边界及其 mailbox 约束。`loop` 只负责 run 预算、生命周期和按序应用 `EffectBatch`，`turn` 负责模型阶段编排并产出 effect batch，`effects` 是 transcript、branch point 和 pause 的唯一按序应用边界；turn 终态与工具批次的 durable 提交都走这里，turn-start 注入和 stream chunk 仍留在各自边界，`response_cycle` 负责不完整工具参数 JSON 的有限重试；空响应与模型非正常结束会保留部分输出并转为可继续生成的错误，`tool_batch_plan` 固化 assistant 调用顺序和跨层身份，`tool_batch_execute` 负责批次准入、并发执行、取消与每项结果的即时 durable 提交，`tool_batch_policy` 负责失败分类、共享 `RecoveryPolicy` 的 Agent 重试预算与重试提示，`tool_batch` 在批次完成后按 assistant 调用顺序更新 canonical transcript，并负责工具执行原语、确认生命周期与结果状态。`RequestContext` 从 durable canonical 生成不可变的 provider 请求视图，统一承载 sanitize、retry nudge 和一次性重试指令，不反写 transcript；`context` 只收集有边界的上下文项，`inject` 只经 `apply_transcript` 投影，`turn_end` 只组装最终 effect batch，`event_boundary` 负责事件流完整性与生命周期边界，`hooks` 只定义扩展契约，`hook_policy` 装配生产副作用策略。
- 流式输出由 `stream_step` 产生，`event.rs` 用一个有序 chunk 队列归并 thought/reasoning；provider retry 通过 `agent:stream_reset` 标记新的输出代次，UI 只清理 live stream block，不修改 durable transcript。`streamAggregator` 只合并相邻且同身份的 chunk，保留交错输出顺序；最终 thought/reasoning 投影仍是丢 chunk 时的权威修复路径。
- **X12 持久化契约**：ReAct 将 live transcript 作为 `SessionCommitted` domain intent 提交给 `SessionStore`；Agent 负责 ReAct 事件 payload 与消息/步骤语义，Memory 将 intent 翻译为物化行。Store 在同一 SQLite 事务中先追加 `session_events`，再写入 intent 指定的 `messages` / `session_steps` 投影；投影失败时整笔回滚，事务提交后才使 cache 失效并广播事件。Agent 随后由 `CommittedUiPublisher` 按 `session_events.sequence` 发布 Thought、Action、Observation、Supplement、ingress MediaPlan 与 Compaction，再更新进程内 canonical。assistant Thought 消息行与 durable event 同事务提交；共享 `step-*` 的 Thought 执行步骤作为可修复的后置 Store 投影写入，失败不会撤销已提交事件或重复发布。流式分片只用 `chunk_seq`；WebSearch、Usage，以及请求准备阶段的 MediaPlan（`event_seq` 为空）不占用这条 durable 序号。同一 sequence 的并行工具卡按 `(eventSeq, stepId)` 去重。交互请求由 `SessionActor` 命令追加为 domain event，Agent 从活动 `session_events` replay 交互状态；若 Ask `tool_result` 已提交而独立 `interaction_requested` 尚未提交，replay 以稳定 `step_id` 恢复 pending Ask，后续 `UserInject(source=answer)` 或 clear event 关闭它（ADR 0440）。resume、rollback 和实时重放均从 event sequence 读取，事件流本身承载恢复游标。rollback 的 event cursor 用于 active transcript 投影，event sequence 用于 append-only timeline；`last_msg_at` 只用于截断物化消息投影，三者由 `SessionStore` 封装且不得互相推导或作为 transcript 真源。多模态输入在 ingress 接受 `MessageAttachment`，但事件/数据库 canonical 投影使用 `MediaAsset → MediaRepresentation → MediaPlan`，事件不保存 inline bytes；OCR/STT 成功追加派生表示且保留 raw asset。合法旁路限于 ingress seed、recovery partial 与终态 action-result；interaction lifecycle event 只承载请求状态和引用 ID，Ask 正文只在 canonical transcript 出现一次；turn-end 防御性 search-final 仍可在既有 ToolCall event 后直接补 message projection，单独跟踪收口。
- **工具调用身份契约**：同一 assistant tool batch 内，`action_index` 是 provider 调用数组的零基稳定位置，`step_id` 是该调用的持久执行行/卡片身份，`tool_call_id` 是 provider 调用身份；`session_steps` 与 ReAct events 同步保存三者。确认恢复必须按完整身份关联，禁止按工具名、参数或 observation 文本猜测；缺失事件流不再从步骤投影重建 ReAct transcript，旧数据按 reset 边界处理。并行工具的每项结果在完成后单独提交并按 durable sequence 发布 UI；canonical history 与 event replay 按 `step_number + action_index` 排序（ADR 0433）。
- **工具参数验证契约**：执行前只验证，不用 schema default、首个 enum 或类型占位符改写输入；无效参数以包含 `action_index`、工具名和验证明细的失败 observation 返回给模型，避免改变副作用语义。
- `session/`：`SessionSupervisor` 只负责 registry、并发 admission 和生命周期；其生产构造接收组合根创建的 `SessionStore` 与 `SessionToolPorts`，不暴露 raw `Database` 或 `ToolsManager`。`AgentLayer::build` 接收 `AgentToolPorts`；`haven-app-binary` composition root 用唯一共享的 `ToolsManager` 创建 prompt/catalog/execution/authorization/observation/overlay/asset adapters，Agent runtime owners 只持有窄 ports 与既有 AuthorizationEngine/ActionService capability（ADR 0384、0388）。session runner 通过 `ToolExecutionContext` 传递 session、tool、input、cancel 与 step identity；live authorization 仍在执行前判断。`SessionActor` 独占会话级可变状态，并在自己的 loop 中 select 外部 mailbox 命令与 `SessionState::react_run` active future。`SessionState` 持有会话元数据、交互与 ingress/action/messaging 队列；active future 独占 run-local `ReActState`，不会跨 session 共享，也不借用整份 `SessionState`。usage 由 `ReActEngine::UsageRuntime` 聚合和写入，stream identity 与 token estimate 由 run-local `ReActState` 持有。inbox 通知游标、轮询节拍和标题缓存已在 `SessionState`；进程级 heartbeat 合并仍留在 `MessagingPoller`；`TurnEngine` 只推进一次 turn 并产出 `EffectBatch`，`RunEngine` 负责应用批次和 run 边界；`dispatcher` / `queues` / `status` / `tool_runner` 只提供各层协作能力。普通用户创建与 `agent.spawn` peer 继续共用同一 Session、SessionActor 与 ReAct 流程；`SessionStore` 持久化并按 parent 查询 typed `SessionOrigin`，Messaging registry 仍负责角色、能力、mailbox 与在线状态（ADR 0442）。
- `layer.rs` + `ingress.rs` / `resume.rs` / `resume_support.rs`：对外入口与 resume 恢复；`resume_support` 只提供确定性的候选合并、悬空工具调用修复和运行时工具选择恢复。
- `canonical.rs`：发送前 `sanitize_canonical` 闸门。
- `memory_worker.rs` / `memory_service.rs` / `memory_index.rs` / `prompt_context.rs` / `prompt_renderer.rs` / `prompt.rs` / `compactor.rs` / `rollback.rs` / `rollback_support.rs` / `title.rs` / `event.rs` / `partial.rs`；`memory_service` 统一 typed memory/embedding/cache 边界，`prompt_context` 取得 bounded turn snapshot，`prompt_renderer` 纯渲染 bounded MEMORY fence；`rollback.rs` 编排生命周期与 DB 双时钟，`rollback_support` 只操作 events 和 branch cursor。
- Memory outbox retry 与 `MemoryRuntime` 恢复退避复用 common 纯策略/退避计算；worker 仍拥有 marker、cursor、等待、取消与恢复时序（ADR 0268、0447）。
- `fact_extraction.rs`：事实抽取 DTO、LLM 字段 coercion、标签/谓词规范化、prompt
  字段清洗和 JSON array 提取；`MemoryWorker` 负责调度与持久化（ADR 0029、0169）。
- 调用 `LlmRouter`、执行 `haven-tools` 工具、写 `haven-memory`、
  通过 `AgentEvent` 对外发事件。

**步数预算（Phase 7/8 / J1）**：`session.max_steps` 是**单次 run**上限。pause / ask / confirm 后再次
resume 会按 `per_run_cap = max(max_steps, start_step - 1 + max_steps)` 再给满额。可选
`session.session_max_steps: Option<u32>`（默认 `None` = 不限）在绝对 `step_number` 上截断：
`effective_max = min(per_run_cap, session_max_steps)`。该限制由 `SessionConfig` 提供，计算仍由 ReAct run budget 持有（ADR 0220）。

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
| 工具 | `haven-tools` `builtin/messaging.rs` | 统一工具名 `agent`；`operation=` list / children / history / send / inbox / ack / reply / profile / request / spawn / status / join / wait / stop / collect；通过 `haven-messaging::MessagingService` 调用 |
| 服务 | `haven-messaging` `messaging_service.rs` | 唯一应用层消息 port：校验 Envelope identity、claim/complete/retry/expiry、request/reply selective wait 与 receipt 生命周期 |
| 传输 | `haven-messaging` `inbox.rs` | JSONL file transport adapter：`%APPDATA%/haven/inbox` 的 registry / mailbox / archive / lock；不向应用暴露同步 drain 语义 |
| 编排 | `haven-agent` `layer::spawn_peer_session` | 先落库 `peer_kickoff` 并 inbox 注册 parent，再 Pending 调度；返回 `queued`（相对 `session.max_concurrent`） |
| 接线 | `haven-app-binary` `app_state` | 将 Agent 提供的 typed `MessagingRuntime` 接入 Tools runtime；消息服务与协作契约由 `haven-messaging` 提供 |
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
store 重试与终态持久化修复的 typed failure 分类由 crate-private Action retry policies 收口，
attempt/deadline/backoff/stop 决策复用 `haven-common::retry::RecoveryPolicy`（ADR 0447）：background/scheduled
worker 均无 retry deadline/预算，保留 1 秒起步、指数退避、30 秒封顶；Action store 的短 retry
保留 3 次/50 ms，malformed-row repair 在短 retry 耗尽后继续按 1 秒起步、30 秒封顶恢复。
策略不持有 clock、sleep、store 或 terminal arbitration。
ActionService 继续按 kind 执行各自 CAS/outbox、内存状态、事件发布、重试等待与生命周期。当前 background
shell 没有 action-level 执行 timeout；scheduled `due_at` 是触发时刻。AgentLayer 对 background completion
做 durable transcript 投影/入队的 100 ms 重试决策复用 common 模型，但投影、等待与 outbox ack 仍由 AgentLayer
持有；provider/LLM retry 和 Agent
ReAct tool-call retry 不属于 action persistence retry（ADR 0305、0332、0334）。
调用边界并不是一个共享的执行 owner：后台 shell 的 child process 由 `ActionService` 启动并回收；
scheduled fire 由 `ActionService` 按 `Waiting → Running` durable CAS 后交给 AgentLayer，AgentLayer/
tool runner 执行 scheduled tool 或继续会话，再调用 `complete_scheduled` / `fail_scheduled`。
scheduled trigger 的输入分类和 due-time 计算由 crate-private 纯 typed policy
`ScheduledTriggerRequest`/`ScheduledTriggerCandidate` 承担；ActionService 仍读取 horizon 配置并拥有
durable admission、board insertion、timer/watch worker、fire、terminal commit/retry 与 lifecycle event。
这只是 trigger admission 的窄边界，不是 `Immediate`/`At`/`After` 与 execution 的完整 Job 模型；
schedule tool 对 LLM 输入的前置验证仍保留在工具边界，App command/event adapter 仍只做 UI DTO 投影（ADR 0343）。
ActionService 仍是唯一状态 owner；实现按职责放在 `action_service/background.rs`（shell 启动、终态与 session 清理）、
`action_service/scheduled.rs`（timer/dependency admission、fire、终态与恢复）和
`action_service/views.rs`（task board/status typed projection 与 JSON 序列化）。这些模块只实现同一个
`ActionService`，共享 action map、terminal arbitration、completion bus 和 restore coordinator，不增加执行器或状态副本。
完整 lifecycle 审计没有发现需要迁移到另一个纯 transition policy 的重复判断：status graph 与 terminal claim 已由
`ActionStatus::can_transition_to` / `action_terminal::can_claim_terminal` 单点定义；background admission 直接进入
`running`，`waiting → running` 只属于 scheduled fire。提交前后的重复检查跨越 durable CAS 与内存投影/回滚边界，保留为竞态校验。
执行副作用、outbox、retry 与 UI finished 投影继续按 kind 分流；trigger/execution、deadline/claim identity 和 restart recovery
语义需先决策，当前不引入新的 Job 状态或自动 replay（ADR 0352）。产品已确认 background 与 scheduled 的完成记录、任务卡和 transcript 投影采用统一格式，但保留类型细节；TaskCenter 活动卡片已由 `projectActionCard` 统一投影，terminal completion record/transcript 的 scheduled outcome 映射仍待产品决策（ADR 0373）。该审计还发现 `ActionService::set` 的 scheduled
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
仍由 ActionService board 返回。`TaskCenter` 使用 `projectActionCard` 将两种 kind 映射到共同卡片结构，并在
kind-specific details 中保留 background command/output/error/exit code/preview 与 scheduled due time/title/body/mode；
现有用户文案、排序、搜索、打开会话和取消行为不变。该 mapper 只收口活动卡片，尚未统一 terminal
completion record 或 transcript 内容：scheduled finished payload 不含 execution result，continue 模式已有正常会话
输入/对话记录，tool 模式当前只把结果发到通知，具体 scheduled transcript 映射需产品决定（ADR 0373）。
Rust bridge 只为 background 提供 `action:output`，scheduled action 不持有 tail。没有 durable event identity，
因此不新增 UI event dedup 或统一 Job reducer；background 的终态工具卡和 transcript 投影仍按 ADR 0344 原路径。
Action completion 经 `notification:show` 发布带 `notification_kind=action_completion` 标记的专用事件，
并携带 `action_kind`、`action_id`，background 还携带终态 `action_status` 与真实 owner `session_id`；scheduled 或 AppCommand 通知没有会话关联时省略 `session_id`。UI 只对该标记
应用 `notification.action_completed.in_app` 开关；`DesktopNotifications` 只对同一类事件应用
`notification.action_completed.windows` 开关。两项配置由 background 与 scheduled action 共用且默认开启，
配置加载完成前 UI 暂存带标记的完成提示。通用 `AgentEvent::Notification` 不带该标记，仍保持原有通知语义与
always-on 行为。此通知设置不定义 scheduled transcript 或 execution result 的投影；其内容映射仍待产品决策
（ADR 0373）。
Action 管理写入口经 ADR 0373 审计：Tauri、`actions`/`schedule` 工具、timer worker 与 Agent completion 共用一个
`ActionService`；`ActionStore` 仍是生产持久化写边界。`schedule.set` 只创建新 action，没有 update-existing 或手动
trigger command，`ToolConcurrency` 也不是跨 Tauri/worker 的互斥机制。terminal history delete 只接受终态，但其
`spawn_gate` 不覆盖所有 terminal CAS/retry；现已由 ADR 0374 收口为 completion ack 前拒绝 history delete，并在同一 SQLite writer 边界协调 ack/delete。无 owner completion 的 ack 同时校验 action 与 outbox 均未绑定；迟到绑定会重开 outbox，带 session 的 live-output 工具则在无 step id 时也于 spawn 前绑定 owner。
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

确认 UI：拒绝 / 仅本次 / 本对话允许此操作 / 更多允许选项；更多选项按“本对话或永久”与“当前操作、功能组、工具”组合展示，永久授权和扩大范围需要二次确认。没有持久会话 owner 的界面直调与定时确认不提供 session scope；后端也会拒绝 renderer 伪造的 session 决定。拒绝菜单同样支持操作、功能组、工具层级。后端只接受当前 capability 的合法父级，不能由 renderer 发明任意权限键。永久授权写入 `config.toml`；会话授权写入 `session_authorization_grants`，由 `SessionStore` 读取并恢复，且通过 `session_id` 外键随会话删除和历史保留清理。普通会话结束只清进程内 map，授权在重新载入会话时恢复；普通 Security 配置 apply 清进程内 map 后也从数据库恢复。永久规则与会话授权可分别查看、撤销和重置；会话逐项撤销按 session+capability 匹配，永久操作不删除会话 grant。回滚 transcript 不改变授权。安全设置子视图按会话列出 allow/deny、capability、target，并显示每类 reset 的准确影响数量。确认收据绑定规范化输入 hash、权限 key、策略 revision、风险和过期时间，执行前再次验证；原始 shell、网络、文件和扩展参数不进入 renderer。普通全量设置保存不拥有权限规则，避免 stale form 清空授权。授权结果另带稳定 `AuthorizationReasonCode`，调用方不得解析错误文案。

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
- `ApplicationRuntime` 长期持有 Agent 构造结果交接的 `MemoryStartup`，并注册/join prepare、
  live consumer 与周期 maintenance task；prepare/replay 完成且 live task 注册后才获得 typed
  `MemoryReady` 并开放 dispatcher。周期策略仍归 MemoryRuntime，手动 maintenance 命令仍调用
  Agent 的单次 worker pass，shutdown 先停 worker、后按既有顺序 join app tasks（ADR 0367）。
- `app_state.rs`：装配 `AppState`（runtime / 瞬态录音状态 / bootstrap 状态 / UI
  confirmation）；命令通过 runtime 稳定句柄消费 db / router / tools / executor /
  agent / pipeline / shell / `config_service` / media clients / stt_client；在组合根创建
  supervisor 专属 `SessionStore` 与唯一 `MemoryService` 并注入 AgentLayer（ADR 0363、0364）。
  启动、保留期、上传引用和每日媒体清理 task 只捕获 `SessionStore` typed port，不把 raw
  `Database` 传入后台任务（ADR 0374）。
- `config_runtime.rs`：根据 `ConfigChanged` 生成 runtime apply plan，区分 live consumer 和
  `restart_required` consumer；运行时编排留在组合根，不下沉到 `haven-common`。
- `commands/recording.rs`：拥有录音/转写 Tauri 命令与事件状态机；transcript 附件落盘委托给
  `commands/managed_media.rs`，不在录音命令内维护文件清理规则。
- `commands/managed_media.rs`：拥有 App transcript 上传的额度、staging、原子提交、文件名/路径校验，
  以及 uploads/generated-media 两根目录和 staging 的清理。一个 App 内部写锁串行化上传与清理；
  两根媒体目录按 `SessionStore` durable refs、`ManagedAssetRegistry` lease 与 detached TTL 对账。
  `app_state.rs` 仍拥有启动/每日调度，session 命令仍在历史删除成功后触发清理；Tools registry 和媒体
  producer 不迁入此模块（ADR 0403、0469）。
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
并由 listener 层记录。可选 `waiting_reason` / `reason` 缺省映射为 `null`；已提供但不属于当前值集的
status 或等待原因会丢弃该事件（ADR 0380）。Rust DTO、channel、payload 与 reducer 语义不变（ADR 0330）。
Action board 与 lifecycle event 共用 Rust `events.rs::ActionEvent` wire DTO：
`ui/src/lib/contracts/action.ts::mapActionPayload` 是其唯一前端运行时 validator/mapper，
`actionStore.refreshActions` 的 command rows 和 `events.ts` 的 action lifecycle listeners 都调用它。
必需 `id`/`kind` 或已声明字段类型无效时丢弃整行/事件；未知附加字段忽略，未知 status 与 kind
fail closed（ADR 0380）。mapper 不接触 ActionService completion outbox；动态
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
没有额外的 settings store 或第二个 update serializer。`update_settings` 的唯一 builder 复用开放式
`SettingsPayload`，其 Rust 参数和直接调用 owner 由 IPC contract script 校验；前端不复制 Rust nested Settings schema
（ADR 0372）。`hotkey:rebind` 事件已由
`ui/src/lib/contracts/app.ts::mapAppEvent` 唯一映射 `old_binding` / `new_binding` 到 camelCase；
设置页的 `get_log_info`、`read_log_tail`、`get_performance_metrics`、`check_shell_available` 与
`get_api_key_status` 读取统一经 `ui/src/lib/diagnosticsCommands.ts`；`contracts/settings.ts` 的既有
parsers 继续校验 ADR 0007 的命名响应 DTO，并作为日志、shell 与 API-key 响应的唯一 validator/mapper；
metrics 响应保持开放以保留动态诊断字段。
`performanceMetrics.ts` 继续拥有 renderer 计数 provider（ADR 0370）。app-shell 事件的批量
`appEventListeners` 与单条 `registerAppListener` 共用 `mapAppEvent` adapter；ToolsView 的 MCP/Skills
刷新监听也经过该入口，布局只拥有 MCP 通知副作用，Skills 不再保留空 listener。Rust MCP status 使用
serde 外部标记 enum，MCP status 只接受当前 Rust DTO variants（ADR 0380）；明确投影的 hotkey/interaction 字段仍只输出
已知 camelCase DTO 字段。布局通知与 ToolsView 刷新是不同副作用，不做 event dedup。`SessionResumeResponse`
中的 interactions 仍由原 session resume normalizer 处理。Agent wire DTO 仍由 Rust `events.rs` 定义；
`contracts/agent.ts::mapAgentEvent` 是唯一 runtime validator/mapper，删除重复的 TS snake_case wire
interfaces，忽略未知附加字段；工具 outcome、retry、idempotency 与 operation scope 必须匹配当前值集，动态扩展值仍只在显式字段保留（ADR 0380）。`agentEventListeners` 对 malformed
payload 记录不含 payload 的 warning 并丢弃；聊天页与布局订阅互不重叠，共用同一 session reducer，通知、
usage fallback 与 media plan 双副作用保持原 owner（ADR 0347）。SessionCompleted/SessionError
仍经 primary channel 与 `session:updated` secondary fan-out；同一终态 occurrence 现携带共享 `occurrence_id`，
聊天页只对精确配对的 secondary 跳过重复 cleanup，独立终态 `session:updated` 仍执行清理；相同 reducer
状态投影不再广播新引用（ADR 0349、0386）。session、action、recording、settings read、app event 与 agent event contract 已完成对应 mapper/validator 或边界审计（ADR 0330、0335、0340、0341、0346、0347、0348、0350、0376）；live interaction event 与 resume snake_case DTO 保持各自 mapper。命令 request/response 的静态 TypeScript contract 由 Rust handler/Serde DTO 生成至 `generatedCommands.ts`，不在多份手写定义间同步字段（ADR 0394）；生成类型不替代运行时校验，event mappers 与动态扩展 payload 仍按各 domain 手工维护。Settings update payload 仍由 SettingsView 的单一 builder 构造。Action board 的活跃
`list_actions`/`cancel_action` 经 `actionCommands.ts`；list response 复用 `mapActionPayload`，cancel
request/result 使用命名 TS contract，`actionStore` 不直接 invoke（ADR 0348）。命令静态 request/response
统一使用 Rust 生成 contract；每个领域仍负责运行时校验、直接调用编排和安全审计。事件尚无全局 codegen，
各事件 mapper 继续按 ADR 逐域维护。
`continue_session`、`interrupt_session`、`end_session` 与 `rollback_session` 由
`ChatController` 单一编排并直接 invoke；请求在 `contracts/commands.ts` 使用命名 DTO，页面逻辑保留
原 in-flight 锁、错误处理与通知顺序。`resolve_confirmation` 留在 `+layout.svelte` 的 shell confirmation
入口，因为弹窗必须跨工作区可见；它也使用命名 request DTO 与本地 in-flight guard。没有重复 request
mapper 或绕过 owner 的 UI caller，IPC script 对照 Rust handler 参数、TS DTO 和直接调用边界（ADR 0371）。
`+page.svelte` 保留 view/scroll 与 dialog/loading/menu 状态、model sync、resume target/auto-restore、
新会话入口及非 chat-event teardown；ask/input 分流、会话启动恢复和滚动/observer 生命周期分别由
`chatAskInteraction`、`chatSessionStartup`、`chatViewController` 拥有，在 mount 时按 listener-ready 顺序
接线并在 destroy 时 dispose。旧 `sessionMessages.ts`、
`sessionUsage.ts` 仅保留兼容投影，`streamAggregator.ts` 只负责排队后 dispatch chunk action
（ADR 0160、0313、0315、0320、0322）。阶段 8 的命令 contract 生成与聊天编排范围已完成（ADR 0394）；尚未逐域审计的事件运行时校验、授权策略与页面局部状态仍由各自领域按变更和风险持续审查，不是待完成的跨域 codegen/总 controller 阶段，也不据此机械拆页。
`ModelSettings.svelte` 仍拥有命名模型和 Provider CRUD 编排；活跃 discovery command 现统一经过
`modelDiscoveryCommands.ts`（ADR 0368），页面仍负责缓存数据的 settings 投影与刷新交互。ToolsView 的
catalog、MCP/Skills 管理、连接和 refresh 命令均通过 `toolsCommands.ts`；命名 request/response DTO 保持
Rust wire snake_case，MCP config 对齐固定 `McpServerConfig`，动态 schema 仍只在 `ToolSchema = unknown` 边界。
ToolsView 继续拥有 optimistic state、通知、失败显示和 snapshot/event refresh 编排；Rust handler 继续拥有
native admin authorization、连接副作用和 status event。`refresh_mcp_servers` 与 `reconnect_mcp` 的 handler
当前未请求 AuthorizationEngine，策略审计是单独 follow-up，不由 typed helper 改变。MCP/Skill list DTO
保留扩展字段，builtin `ToolManifest` 仍由 `toolManifest.ts` 唯一投影，卡片列表复用同一批解析行（ADR 0369）。

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
- `docs/architecture-refactor-roadmap.md` —— 当前架构阶段、未完成项与验收条件
- `docs/architecture-output-contract-inventory.md` —— 跨 crate 与 Tauri 输出契约清单
- `docs/adr/README.md` —— 架构决策索引；具体 ADR 记录决定、替代方案与影响

---

## 变更历史

历史决策、替代关系和实施细节以 [ADR 索引](adr/README.md) 与具体 ADR 为准；本文件只描述当前架构。
