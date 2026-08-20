# Haven 架构与 crate 职责

> 版本: v1.0 | 日期: 2026-08-18
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
| `haven-memory` | common | 持久化（SQLite schema、仓库） |
| `haven-skills` | common | 技能目录解析 |
| `haven-mcp` | common | MCP 客户端 / 传输 |
| `haven-tools` | common, skills, mcp, llm | 工具注册表 + 各内置工具 |
| `haven-input` | common, llm | 录音 / VAD / STT 编排（**不实现 provider**） |
| `haven-agent` | common, llm, memory, tools, input | ReAct 循环 + 会话执行 |
| `haven-app-binary` | 以上全部 + tauri | 装配 + Tauri 命令 + 事件桥 |

> 依据 `crates/*/Cargo.toml` 实际 workspace 依赖整理。`haven-agent` 与 `haven-app-binary` 是最上层，
> 其余全部是它们的底层依赖。`haven-llm` 不允许被业务 crate 反向依赖。

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

- `adapters/`：chat / stream / tools 的 provider 适配（OpenAI / Anthropic / Gemini）与统一
  `LlmClient` trait + `with_retry`。
- `router.rs`：`LlmRouter`，按 `EndpointRole`（small / default / balanced / reasoning /
  image 等）把请求路由到对应适配器。
- `stt.rs` / `ocr.rs` / `tts.rs` / `image_gen.rs`：各专用客户端实现 + 统一分发入口
  （`build_stt_client` 等）。
- `media/`：**媒体网关**（原 `haven-gateway` 并入，历史归属 input crate，现已在此）——
  附件 → 模态/意图判定 → 专用 provider + 置信度门槛 + 主模型兜底；TTS 生图等 generate 请求。
- `registry.rs` / `stream_rules.rs`：模型注册表、流式规则（生产 router 默认启用 `code_block_abort`）。

**判定标准**：一切「与模型 / 云端 provider 打交道的实现」都在这里；其它 crate 只通过
`LlmRouter` / `*Client` trait 消费，不实现。

### 2.3 `haven-input` —— 输入采集与语音生命周期

- `capture/`：CPAL 采集线程 + 环形缓冲 + 重采样。
- `vad.rs`：tract ONNX 语音活动检测（含常驻 worker 线程）。
- `lib.rs` 的 `InputPipeline`：录音状态机（start / stop / cancel）、VAD 判定 →
  自动停止、`transcribe()` 把 WAV 交给 `SttClient`。
- `hotkey.rs`：快捷键字符串解析为中性 `KeyCombo`（与平台解耦）。

**判定标准**：管「何时/怎么采」——录音生命周期、VAD、把音频交给 STT；**不实现**任何
provider（STT 客户端来自 `haven-llm`）。

### 2.4 `haven-agent` —— ReAct 编排与会话执行

- `react/`：ReAct 循环（`loop` / `stream_step` / `tool_batch` / `inject` / `snapshot_io` / `retries`），流式响应、快照/分支、压缩。
- `session.rs`：`SessionExecutor`（会话队列、并发信号量、状态机、supplement/steering 队列）。
- `layer.rs`：`AgentLayer`（对外入口：process_input / run_session / 事件发射）。
- `inference.rs` / `prompt.rs` / `compactor.rs` / `rollback.rs` / `title.rs` / `event.rs` / `partial.rs`。
- 调用 `LlmRouter` 与 `MediaGateway`、执行 `haven-tools` 工具、写 `haven-memory`、
  通过 `AgentEvent` 对外发事件。

**判定标准**：会话的业务编排中心，不知道也不关心 provider 细节 / 录音硬件细节。

### 2.5 `haven-app-binary` —— 组合根 + 宿主边界（Tauri）

- `app_state.rs`：装配 `AppState`（db / router / tools / executor / agent / pipeline / shell /
  config_loader / gateway / stt_client）。
- `lib.rs`：`AgentEvent` → 前端 channel 映射（`TauriEmitter`）、`ShellHandler` /
  `InputHandler` 钩子接线、托盘 / 全局快捷键 / 单实例 / 通知 / 自启 / 日志初始化。
- `commands/*`：全部 Tauri IPC 命令（recording / session / action / history / model / mcp /
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
- `docs/memory-architecture.md` —— 记忆 / Facts 现状、引擎 backlog、与对话历史协作计划（短期/长期）
- `docs/react-architecture-improvements.md` —— ReAct 对照 PI Agent Core 的改进清单与分期
- `docs/architecture-refactor.md` —— 历史重构图谱（命令拆分 / 配置收敛 / 运行阻塞）

---

## 变更记录

| 日期 | 内容 |
|---|---|
| 2026-08-18 | 初版；`Supplement` 从 `haven-input` 下沉 `haven-common::types`，去除 `agent → input` 依赖 |
| 2026-08-20 | `memory-architecture.md` 改为现状 + backlog 描述 |
| 2026-08-20 | 相关文档增加 `react-architecture-improvements.md` |
| 2026-08-20 | `memory-architecture.md` 增加 Facts/Episodes/对话历史协作计划 |
