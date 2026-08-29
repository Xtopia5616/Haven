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

CI 以 `scripts/check-crate-dependencies.ps1` 对此表执行内部 crate 依赖方向检查；新增或调整
跨 crate 依赖时，必须先更新本表与该检查，并记录 ADR。

---

## 2. 各 crate 职责

### 2.1 `haven-common` —— 共享叶子（数据与工具，无任何内部依赖）

- `config/`：TOML 配置 schema（`AppConfig` / `Settings` / 各子配置）+ `ConfigLoader`。
- `types.rs`：跨 crate 的规范类型 —— 实体 ID（`new_id` / newtype）、`CanonicalMessage` /
  `ContentPart` / `CanonicalToolCall`、`MessageAttachment`、`Supplement`、`RiskLevel`、
  `HotkeyMode` / `ShellChoice` 等。
- `prompts.rs`：系统提示词与各专用 prompt 常量（含 `STT_SYSTEM_PROMPT`）。
- `encoding.rs` / `text.rs`：编码解码（UTF-8 → GBK 回退）、文本工具。

**判定标准**：凡被 ≥2 个 crate 共享、且不依赖任何业务逻辑的纯数据/纯函数，放这里。

### 2.2 `haven-llm` —— 模型与媒体能力的唯一实现方

- `adapters/`：按 **`api_style`（线协议）** 分发的 provider 适配与统一 `LlmClient` +
  `with_retry`。能力矩阵见 `adapters/capabilities.rs`：
  - `openai-chat` / `llama.cpp` → OpenAI Chat Completions；embedding 走 `/embeddings`
  - `openai-responses`（含 DeepSeek Responses 别名 / thinking echo + `web_search`）；embedding 仍走 `/v1/embeddings`
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
- `router.rs`：`LlmRouter`，按 `EndpointRole`（small / default / balanced /
  image / audio / embedding）把请求路由到对应适配器。
- `request_pipeline.rs`：provider-neutral 的 `RequestPolicy`/`RetryPolicy`；
  为普通聊天、工具聊天、embedding 和流式端点尝试提供同一份重试预算快照与
  总超时执行语义。router 仍拥有熔断、限流、fallback 和流式聚合，adapter
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
- `media/`：**媒体网关**（原 `haven-gateway` 并入，历史归属 input crate，现已在此）——
  附件 → 模态/意图判定 → 专用 provider + 置信度门槛 + 主模型兜底；TTS 生图等 generate 请求。
- `registry.rs` / `stream_rules.rs`：模型注册表、流式规则（生产 router 默认启用 `code_block_abort`）。

**判定标准**：一切「与模型 / 云端 provider 打交道的实现」都在这里；其它 crate 只通过
`LlmRouter` / `*Client` trait 消费，不实现。

### 2.3 `haven-memory` —— 持久化与记忆存储

- `schema.rs`：当前幂等 SQLite schema、FTS/embedding 维护对象、必需列检查和
  初始化编排。
- `migrations.rs`：历史 schema/data migration、PRAGMA user_version 版本戳和
  迁移顺序；只处理数据库转换，不承担 Agent 推理或 UI 展示。
- `repositories/`：会话、消息、步骤、图谱、用量和任务的持久化读写；其中
  `fact_graph.rs` 集中负责 `memory_edges` 写入与图谱不变量，`fact_query.rs`
  负责事实读取、搜索/排序，`fact_maintenance.rs` 负责事实清理、衰减与矛盾
  扫描，`facts.rs` 负责事实类型、谓词策略和稳定 `Database` 外观；不拥有
  schema 升级策略。
- `embeddings.rs`：向量编码、相似度/ANN 查询和 embedding 存储操作。

`schema.rs` 与 `migrations.rs` 的边界不改变 X12：`messages` /
`session_steps` 仍是投影，`ReActSnapshot.events` 仍是恢复唯一权威；迁移只
维护持久化结构，不成为新的业务真源。

**判定标准**：只负责 SQLite 生命周期与记忆数据持久化；Agent 编排、LLM
provider 协议和 UI 展示逻辑不得进入本 crate。

Agent 的 `memory_index.rs` 是嵌入编排边界：它负责 embedding provider 调用、
有界 catch-up、模型切换清理、向量召回和 LSH 重建；`InferenceEngine` 只编排事实
抽取/维护并使用该组件，不把 provider 网络调用或跨 await 的 SQLite 连接下沉到
Memory。事实维护的 SQL 清理与矛盾候选读取由 `fact_maintenance.rs` 负责；维护
调度、LLM 仲裁、提案门禁与并发控制仍属于 Agent，二者通过既有 `Database` 外观
连接（ADR 0022）。

### 2.4 `haven-input` —— 输入采集与语音生命周期

- `capture/`：CPAL 采集线程 + 环形缓冲 + 重采样。
- `vad.rs`：tract ONNX 语音活动检测（含常驻 worker 线程）。
- `lib.rs` 的 `InputPipeline`：录音状态机（start / stop / cancel）、VAD 判定 →
  自动停止、`transcribe()` 把 WAV 交给 `SttClient`。
- `hotkey.rs`：快捷键字符串解析为中性 `KeyCombo`（与平台解耦）。

**判定标准**：管「何时/怎么采」——录音生命周期、VAD、把音频交给 STT；**不实现**任何
provider（STT 客户端来自 `haven-llm`）。

### 2.5 `haven-agent` —— ReAct 编排与会话执行

- `react/`：ReAct 循环（`loop` / `stream_step` / `tool_batch` / `context` / `inject` / `turn_end` / `snapshot_io` / `retries` / `hooks` / `hook_policy` / `transcript`），流式响应、快照/分支、压缩；`context` 只收集上下文来源，`inject` 只经 `apply_transcript` 投影，`turn_end` 负责最终事件与暂停边界，`hooks` 只定义扩展契约，`hook_policy` 装配生产副作用策略。
- **X12 持久化契约**：`apply_transcript` 是 events→投影的统一 writer；`messages`/`session_steps` 为物化投影（UI/抽取/rollback 读投影；LLM resume 读 events）。
- `session/`：`SessionExecutor` 门面 + `dispatcher` / `queues` / `status` / `tool_runner`（FIFO、信号量、steering/follow_up、confirm）。
- `layer.rs` + `ingress.rs` / `resume.rs` / `resume_support.rs`：对外入口与 resume 投影；`resume_support` 只提供确定性的候选合并、无快照投影和运行时工具选择恢复。
- `canonical.rs`：发送前 `sanitize_canonical` 闸门。
- `inference.rs` / `prompt.rs` / `compactor.rs` / `rollback.rs` / `rollback_support.rs` / `title.rs` / `event.rs` / `partial.rs`；`rollback.rs` 编排生命周期与 DB 双时钟，`rollback_support` 只操作 events 和 branch cursor。
- `fact_extraction.rs`：事实抽取 DTO、LLM 字段 coercion、标签/谓词规范化、prompt
  字段清洗和 JSON array 提取；`InferenceEngine` 负责调度与持久化（ADR 0029）。
- 调用 `LlmRouter` 与 `MediaGateway`、执行 `haven-tools` 工具、写 `haven-memory`、
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
| 工具 | `haven-tools` `builtin/messaging.rs` | 统一工具名 `agent`，`operation=` list / send / inbox / reply / profile / request / spawn |
| 总线 | `haven-tools` `inbox.rs` | `%APPDATA%/haven/inbox`：`agents.json` + 每 agent JSONL 邮箱 / archive；进程内 `InboxNotifier` |
| 编排 | `haven-agent` `layer::spawn_peer_session` | 先落库 `peer_kickoff` 并 inbox 注册 parent，再 Pending 调度；返回 `queued`（相对 `session.max_concurrent`） |
| 接线 | `haven-app-binary` `app_state` | 安装 `AgentSpawner` 回调（tools 不依赖 agent） |
| 运行时 | `react/context.rs` + `react/inject.rs` | `context` 负责每步 heartbeat、通知或每 3 步 poll inbox 与低信任格式化；`inject` 经 `apply_transcript` 注入带消毒后的 `id`/`in_reply_to`/`subject`；`InjectSource::CrossSession` |
| 生命周期 | `session/status.rs` | 终端态/`end_session` → BFS 子孙 system notice + 无嵌套 cascade 结束；`type=system` 仅运行时 |
| 信任 / 记忆 | `inference.rs` | 跳过 `peer_kickoff` 与跨会话注入文本的 fact 抽取 |
| UI | 对话页 tool card | `agent` 结构化卡片；自动同伴邮件以 `agent`/`inbox`/`auto` 卡片展示；kickoff 左侧「低信任委托」 |

协议约定：同伴消息 ≠ 用户指令；`in_reply_to` 对齐 request id；子会话默认工作目录仍为 Temp（全局约束）。

### 2.5.2 内置 `system` 工具（机器信息与系统控制）

统一入口：`haven-tools` `builtin/system.rs`（`env` / `registry` / `power` 子模块由 `scope=` 转发）。

| scope | 能力 | 风险 |
|---|---|---|
| `info`（默认） | 只读机器快照；`category=` 细分 | Safe |
| `env` | 环境变量 get/set/unset/list；list 可用 `name` 作前缀过滤 | get=Low；list/set/unset=High |
| `registry` | Windows 注册表 get/set/delete/list | 读=Medium；写/删=High |
| `power` | 电源 status / lock / sleep / hibernate | status=Safe；lock/sleep=High；hibernate=Critical |
| `display` | 监视器几何 + DPI/缩放 + 刷新率 | Safe |

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

### 2.5.3 权限 / 确认（SafetyGateway）

决策顺序（fail-closed）：

1. `tool_settings.disabled_operations` / `allowed_paths` → **Blocked**
2. 永久拒绝（`SecurityConfig.permissions`，Always）→ **Blocked**
3. 会话拒绝 → **Blocked**
4. 永久允许 / 会话允许 → **AutoApproved**
5. `ConfirmationMode`：`Ask`（`risk >= min_risk_level`）/ `Paranoid`（非 Safe）/ `Autopilot`（不弹窗，但仍对 Critical 弹窗；永久/会话拒绝始终生效）
6. 否则 → `RequiresConfirmation`（事件带 `params` + `permission_key`）

决策细化：永久拒绝 → 会话拒绝 → 永久允许 → 会话允许。拒绝授权写工具根键（覆盖同工具全部子操作）；允许写精确键。

权限键：`permission_key(tool, params)` → `tool` / `tool:op` / `system:power:lock`；授予父键可覆盖子操作。

确认 UI：拒绝 / 仅本次 / 本对话允许 / 始终允许；拒绝菜单含本对话拒绝、始终拒绝。永久授权写入 `config.toml`，设置页可撤销。

### 2.6 `haven-app-binary` —— 组合根 + 宿主边界（Tauri）

- `app_state.rs`：装配 `AppState`（db / router / tools / executor / agent / pipeline / shell /
  config_loader / gateway / stt_client）。
- `lib.rs`：`AgentEvent` → 前端 channel 映射（`TauriEmitter`）、`ShellHandler` /
  `InputHandler` 钩子接线、托盘 / 全局快捷键 / 单实例 / 通知 / 自启 / 日志初始化。
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
| 复用点 | `InputPipeline::transcribe`（用户麦克风录音） | `MediaGateway::process_attachment`（agent 的 `audio` 工具附件） |

同一个 `SttClient` 被两处复用是**有意的共享**，不是职责重复：input 走「用户录音」路径，
llm 的 `media/` 走「agent 附件」路径。云端 STT（Whisper / Groq / Gemini / Deepgram /
AssemblyAI）与 chat 共用 `adapter_for` 分发；`provider = "llm"` 走
`LlmRouter::transcribe_audio`（原生 `transcribe`，否则 multimodal chat 回退）。
MCP STT 仍走独立 `McpSttClient`（依赖 `McpToolCaller`）。

### 3.2 媒体网关的历史归属

`haven-llm::media` 来源是原 `haven-gateway` crate（早期挂在 input 下）。实现已在
`llm/src/media/`，读代码时以 `llm/media/mod.rs` 的模块注释为准；input 只负责采集与转写。

### 3.3 agent 对 input 的依赖（2026-08-18 清理）

- **改前**：`agent → input` 的唯一理由是重导出 `Supplement`（`session.rs`），agent 不调用任何
  input 能力，属于不必要的耦合。
- **改后**：`Supplement` 下沉到 `haven_common::types`，`agent/src/session.rs` 改为
  `pub use haven_common::types::Supplement`，删除 `haven-input` 依赖与 `input/src/message.rs`。
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
| 2026-08-22 | §2.4.1 多 Agent（Plan A）：`agent` 工具、InboxBus、spawn/cascade、低信任与 UI 展示 |
| 2026-08-18 | 初版；`Supplement` 从 `haven-input` 下沉 `haven-common::types`，去除 `agent → input` 依赖 |
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
| 2026-08-30 | §2.4 Agent：将事实抽取 DTO、字段 coercion、标签/谓词规范化、prompt 清洗与 JSON array 提取收口到 `fact_extraction.rs`，保持抽取与持久化语义不变（ADR 0029） |
| 2026-08-30 | §2.6 UI：将会话消息 map、草稿/会话迁移、rollback 截断与流式 sequence 去重收口到 `ui/src/lib/sessionMessages.ts`，由 `stores.ts` 兼容 re-export（ADR 0030） |
| 2026-08-30 | §2.6 UI：将会话 token usage、LLM 调用明细、恢复/清理与用量格式化收口到 `ui/src/lib/sessionUsage.ts`，由 `stores.ts` 兼容 re-export（ADR 0031） |
| 2026-08-30 | §2.6 UI：将流式 chunk 排队、按帧归并、sequence 去重、step block 关联与同步 flush 收口到 `ui/src/lib/streamAggregator.ts`（ADR 0032） |
| 2026-08-30 | §2.6 UI：将 shell、notify、generic/raw 工具结果 body 按 kind 注册到独立 renderer 组件，`ToolResultCard` 保留公共卡片壳与复杂工具分支（ADR 0033） |
| 2026-08-30 | §2.6 UI：将 `file` 工具的文件操作、目录和读取结果收口到 `ToolFileResult.svelte`，并由 renderer registry 按工具名选择（ADR 0034） |
| 2026-08-30 | §2.6 UI：将 `system` 工具的机器指标、显示器、环境变量筛选/复制与电源状态收口到 `ToolSystemResult.svelte`，并由 renderer registry 按工具名选择（ADR 0035） |
| 2026-08-30 | §2.6 UI：将 `process` 工具的筛选、显示上限、CPU/内存指标和状态表格收口到 `ToolProcessResult.svelte`，并由 renderer registry 按工具名选择（ADR 0036） |
