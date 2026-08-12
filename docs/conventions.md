# Haven 通知 / 日志 / 错误处理规范

> 版本: v1.0 | 日期: 2026-08-09

本文档统一 Haven 项目中**通知（Notification）**、**日志（Logging）**、**错误处理（Error Handling）** 三套规范，覆盖 Rust 后端（Tauri 2）与 Svelte 5 前端。规范以现有代码中的事实模式为基础（`crates/app-binary/src/lib.rs`、`crates/app-binary/src/commands.rs`、`ui/src/lib/stores.js`、`ui/src/lib/logger.js`），新代码必须遵守；存量代码若与本规范冲突，逐步迁移对齐。

---

## 目录

1. [日志规范](#1-日志规范)
2. [通知规范](#2-通知规范)
3. [错误处理规范](#3-错误处理规范)
4. [跨端映射表](#4-跨端映射表)
5. [新增代码检查清单](#5-新增代码检查清单)

---

## 1. 日志规范

### 1.1 前端（`ui/src/lib/logger.js`）

**唯一入口**：`logger.debug / logger.info / logger.warn / logger.error(context, msg, ...args)`。

- **禁止直接调用 `console.*`**（`logger.js` 内部除外）。现有违规点已迁移：`stores.js`、`tauri.js` 中曾出现的裸 `console.warn` 一律改为 `logger.warn(...)`。
- **`context` 使用模块短名**，小驼峰：`stores`、`events`、`invoke`、`markdownRenderer`、`notification`、`tauri`。
- **级别门控**：`currentLevel` 在 DEV 为 `debug`，生产为 `info`，即 `debug` 级别日志只在开发环境输出。
- `addNotification` 的 `error` 类型自动调用 `logger.error('notification', msg)`，调用方无需重复记录。

**示例**：

```js
import logger from '$lib/logger.js';

logger.warn('stores', 'refreshSkills failed', e);
logger.error('invoke', `'${cmd}' failed`, e);
```

### 1.2 Rust 后端（`tracing`）

**框架**：`tracing` + `tracing_subscriber`，初始化集中在 `crates/app-binary/src/lib.rs::init_tracing`（console + 可选按日滚动文件，共用同一个 reloadable `EnvFilter("haven={level}")`）。target 必须落在 `haven` 前缀下（各 crate 名天然满足，如 `haven_agent`、`haven_tools`），否则会被 EnvFilter 过滤掉。

**级别语义**：

| 级别 | 用途 | 示例 |
|---|---|---|
| `error` | 不可恢复失败、用户可见失败、panic | 命令失败、任务出错、`PANIC at ...` |
| `warn` | 可恢复异常、降级、重试 | 热键冲突、录音启动失败、任务暂停 |
| `info` | 生命周期事件 | 应用初始化/退出、任务创建/完成、通知发出 |
| `debug` | 每步细节（ReAct 循环、流式 chunk） | thought/action/observation 摘要 |
| `trace` | 预留，底层数据包级 | — |

**格式约定**：事件消息统一为 `模块::方法: 描述`，上下文用 `key=value` 内联（与 `TauriEmitter::trace_event` 一致），描述含上下文时使用 `tracing` 结构化字段 `tracing::debug!(task_id, step_number, ...)`。禁止 `println!` / `eprintln!` / `dbg!` 输出日志。

**命令错误日志**：Tauri 命令失败必须走 `log_err(ctx, e)`（`commands.rs`），该函数固定输出两行：

```text
command `{ctx}` failed
command error: {e}
```

保留 `command error:` 前缀行是为了让日志采集/仪表盘可以稳定 grep。禁止手写 `map_err(|e| e.to_string())` 而不记录日志。

**Panic**：全局 panic hook 已在 `lib.rs` 设置，自动输出 `PANIC at {file}:{line}: {msg}` + backtrace，业务代码无需自行处理 panic。

### 1.3 ID 上下文（多会话 / 多任务并行可区分）

凡是会话、任务（后台任务与定时任务）等**并发实体**的日志，必须携带对应 ID（`session_id` / `task_id`），使并行日志可直观区分。两条机制配合：

**a) 结构化字段**：日志宏中直接传 ID 字段，fmt 输出形如 `session_id=ses-xxx msg`，可稳定 grep：

```rust
tracing::info!(session_id = %session_id, "dispatcher spawning handler");
tracing::warn!(task_id = %id, "failed to write output log {}: {e}", path.display());
```

**b) Span 上下文**（推荐）：在并发边界建立 `info_span!`，span 内的所有日志**自动**携带 ID 前缀，无需每个调用点手写。已在以下位置建立（新并发边界须照此办理）：

| Span 名 | 字段 | 位置 | 覆盖范围 |
|---|---|---|---|
| `run_session` | `session_id` | `haven_session::SessionExecutor::start_dispatcher`（handler 处 `.instrument(span)`） | 整个 ReAct 循环：agent / react / compactor / title / inference 的所有嵌套日志 |
| `bg_task` | `task_id` | `haven_tools::BackgroundTasks::spawn_shell`（runner future `.instrument(span)`） | 后台任务运行 / 取消 / 输出日志写入 |
| `task_completion` | `task_id`, `session_id` | `haven_agent::AgentLayer` 任务完成 consumer | 任务结果注入 / 会话唤醒 / 通知 |
| `scheduled_task_fired` | `task_id`, `session_id` | `haven_agent::AgentLayer` 定时任务 consumer | 定时任务执行与会话恢复 |

规则：

- 日志点所在函数**已有** ID 参数/变量时，优先结构化字段（a）；没有时依赖所在并发边界的 span（b）。
- 已有文本内联 ID（如 `session {}`）的存量日志保留，新增/修改的日志优先结构化字段。
- 禁止为日志引入新的参数传递（`session_id` 不可用时交给 span）；不要为了打日志改变函数签名。
- 全局性日志（初始化、统计、健康探测）不强制带 ID。

### 1.4 运行时调整

日志级别可通过 `AppState.log_filter_handles` 中暴露的 `reload::Handle` 在运行时修改（console 与文件输出同步生效）。代码不得绕过该句柄直接改 `EnvFilter`。

---

## 2. 通知规范

### 2.1 分层模型

通知分两个独立通道，由事件驱动，各通道受配置独立控制：

```
AgentEvent ──► emit(channel, payload) ──► 前端事件 handler
     │                                      ├─ 应用内 toast（notificationStore）
     │                                      └─ （由 notifyCfg.in_app 控制）
     └─ maybe_show_toast（后端）──► Windows 桌面通知（config.notification.*.windows）
```

- **后端只负责发事件与（按配置）弹桌面通知，绝不直接驱动前端 UI**（通过 `tauri::Emitter`）。
- 前端收到事件后按 `notifyCfg`（来自 `settings.notification`）决定是否显示 toast。

### 2.2 应用内 toast API（`ui/src/lib/stores.js`）

**唯一入口**：`addNotification(msg, type = 'info', duration = 3000)`。

- `type` 取值：`info` | `success` | `warning` | `error`（渲染在 `NotificationToast.svelte`）。
- **去重**：同 `msg` + 同 `type` 的 toast 不重复显示（store 内建）。
- **时长建议**：`info` 3s、`success` 3s、`warning` 4s、`error` 5s；阻塞性/需长时间阅读的消息可到 5s，不超过 5s。
- **消息语言**：与 UI 一致使用中文；变量用模板字符串拼接，如 `` `任务出错: ${errMsg}` ``。
- `error` 类型会自动写入 `logger.error`，调用方不再重复记录。
- 事件驱动的 toast 一律通过 `+layout.svelte` 的 `registerListeners` 事件映射集中处理，页面组件内不自行 `listen` 后弹 toast。

**示例**：

```js
import { addNotification } from '$lib/stores.js';

addNotification(`任务已完成: ${title}`, 'success');
addNotification(`MCP 已断开: ${name}`, 'warning', 4000);
```

### 2.3 后端事件流

- **事件命名**：`domain:action`（`session:created`、`agent:thought`、`recording:error`、`notification:show` …），channel 映射的唯一事实来源是 `TauriEmitter::channel`（`lib.rs`）；新增 `AgentEvent` 变体必须在 `channel` 中登记并补测试。
- **任务（task）事件**：后台任务（kind=`background`）与定时任务（kind=`scheduled`）统一为「任务」，共用同一组事件 `task:created` / `task:updated` / `task:output` / `task:finished`。事件由 `haven_tools` 直接 emit（`bg.rs` / `builtin/scheduled_task.rs` 的 `EventSink`），不走 `TauriEmitter::channel`。payload 区分：后台任务带 `task_id`，定时任务带 `id`；前端 `taskStore` 统一归一化（`id = payload.id || payload.task_id`，条目带 `kind: 'background'|'scheduled'`）。
- **wire 载荷**：统一 snake_case JSON（`{session_id, status, title}`），前端在边界转 camelCase。敏感/内部字段不外泄（见 `payload` 对 `SessionCreated` 的投影）。
- **桌面通知**：统一走 `TauriEmitter::maybe_show_toast`，每个变体先查 `config.notification.{event}.windows` 再决定是否 `notification().builder()...show()`。`notify` 工具（`AgentEvent::Notification`）是 agent 显式请求，双通道默认全开。
- **配置**：`NotificationConfig { in_app, windows }`（`haven_common::config`），事件维度 `session_created` / `session_completed` / `session_paused` / `session_error`。默认值见 `config.rs` 的 `Default` 实现；新增可通知事件必须同步扩展该结构与设置页。

### 2.4 新增通知事件流程模板

1. 在 `haven_agent::AgentEvent` 增加变体；
2. 在 `TauriEmitter::channel` 登记 channel，`payload`/`variant_payload` 确认 wire 形状，补 `lib.rs` 单测；
3. 如需桌面通知，在 `maybe_show_toast` 中按配置弹窗（并补 `trace_event` 日志）；
4. 前端在 `+layout.svelte` 的 `registerListeners` 映射中处理，按 `notifyCfg` 决定是否 `addNotification`。

---

## 3. 错误处理规范

### 3.1 Rust 后端

**分层原则**：

- **crate 内部**：用 `thiserror` 定义领域错误枚举（先例：`haven_llm`、`haven_tools`）；库代码返回 `anyhow::Result` 或领域错误。
- **Tauri 命令层**（`commands.rs`）：统一返回 `Result<T, String>`，所有失败经 `log_err(ctx, e)` 记录（自动 ERROR 日志）并转为前端可读字符串。
- **结构化错误载荷**：需要携带结构化信息（如确认请求 `requires_confirmation`）时用 `serde_json::to_string` 构造 JSON 字符串返回，仍走 `Result<T, String>`。
- **降级策略**：可恢复的初始化失败走降级启动（先例：`degraded_app_state`），记录 `tracing::error!` 后返回降级实例，绝不静默。

**禁止事项**：

- 命令 `Err` 分支不带日志直接 `e.to_string()`；
- 把完整错误细节（如 API key、内部路径）原样返回给前端——日志记全量，前端拿用户可读摘要；
- 用 `panic!`/`unwrap()` 处理预期内失败（`expect` 仅限初始化等不可恢复场景）。

### 3.2 前端

**invoke 调用统一模式**：

```js
try {
    const result = await invoke('xxx', args);
} catch (e) {
    addNotification(e?.message || '操作失败', 'error', 4000);
}
```

- `tauri.js::invoke` 已统一在抛出前记录 `logger.error('invoke', ...)`，页面 catch 中**不需要**再调 logger，只负责用户提示。
- 失败提示文案：优先取 `e.message`，无则给中文兜底文案（`操作失败`/按场景定制）。
- 页面级重复逻辑可收敛为局部 helper（先例：settings 页 `notifyFetch(key, msg, type, duration)`，带 per-key 节流去重）。
- **事件监听注册失败**：统一走 `events.js` 的 `registerListeners`/`registerOne`（内部 `logger.error` 后吞掉，不阻塞 mount），页面不得自行裸 `listen`。
- **catch 后禁止仅 `console.warn` 而无任何用户提示**；确属无需用户感知的错误，用 `logger.*` 记录即可，不弹 toast。

---

## 4. 跨端映射表

| 后端事件 / 错误 | 前端表现 |
|---|---|
| `session:created` | info toast（`新会话: ...`，受 `notifyCfg.session_created.in_app` 控制） |
| `session:completed` | success toast（`会话已完成: ...`） |
| `session:error` | error toast（`会话出错: ...`，5s） |
| `session:updated` status=`paused` / `pending` | warning / info toast（`会话已暂停/恢复`） |
| `notification:show` | info toast（title 为默认 `Haven` 时只显示 body） |
| `mcp:status_change` | Connected→success / Disconnected→warning / Offline→error |
| `hotkey:conflict` | error toast（5s） |
| `recording:error` | 录音 overlay + error 提示 |
| 命令 invoke 失败 | error toast（`e.message` 或兜底文案，4s） |

---

## 5. 新增代码检查清单

- [ ] 日志：前端只用 `logger.*`（禁止 `console.*`）；后端只用 `tracing`（禁止 `println!`），命令错误走 `log_err`。
- [ ] 通知：事件驱动，前端只经 `addNotification`；新事件按 §2.4 模板走完 channel / payload / toast / 配置四步。
- [ ] 错误：后端 `Result<T, String>` + 日志 + 用户可读摘要；前端 try/catch + `addNotification`，不重复记日志。
- [ ] 涉及桌面通知配置时同步扩展 `NotificationConfig` 与设置页。
