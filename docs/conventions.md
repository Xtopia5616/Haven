# Haven 通知 / 日志 / 错误处理规范

> 版本: v1.3 | 日期: 2026-08-20

本文档统一 Haven 项目中**通知（Notification）**、**日志（Logging）**、**错误处理（Error Handling）** 三套规范，覆盖 Rust 后端（Tauri 2）与 Svelte 5 前端。

**事实来源（以代码为准）**：

| 域 | 路径 |
|---|---|
| 后端 tracing 初始化 / 命令错误日志 | `crates/app-binary/src/logging.rs`（`init_tracing`、`log_err`） |
| 事件发射（channel / payload） | `crates/app-binary/src/lib.rs`（`TauriEmitter`） |
| Windows 桌面通知 | `crates/app-binary/src/notification.rs`（`DesktopNotifications`） |
| 通知配置 | `crates/common/src/config/misc.rs`（`NotificationConfig` / `NotifyChannels`） |
| 前端日志 | `ui/src/lib/logger.ts` |
| 错误文案归一 | `ui/src/lib/formatError.ts`（`formatError`） |
| 应用内 toast | `ui/src/lib/stores.ts`（`addNotification`）+ `ui/src/lib/NotificationToast.svelte` |
| 事件监听注册 | `ui/src/lib/events.ts`（`registerListeners` / `registerOne`） |
| 事件 → toast 映射 | `ui/src/routes/+layout.svelte` |
| invoke 错误日志 | `ui/src/lib/tauri.ts` |

新代码必须遵守；存量代码若与本规范冲突，按 §6 待对齐项逐步迁移。

---

## 目录

1. [日志规范](#1-日志规范)
2. [通知规范](#2-通知规范)
3. [错误处理规范](#3-错误处理规范)
4. [跨端映射表](#4-跨端映射表)
5. [新增代码检查清单](#5-新增代码检查清单)
6. [待对齐 / 可优化](#6-待对齐--可优化)
7. [「Haven」/「haven」大小写](#7-havenhaven-大小写规范)

---

## 1. 日志规范

### 1.1 分层职责

| 层 | 用途 | 入口 | 禁止 |
|---|---|---|---|
| 前端开发日志 | 调试 / 排障，用户不可见 | `logger.*` | 裸 `console.*`（`logger.ts` 内部除外） |
| 后端运行日志 | 生命周期、错误、并发上下文 | `tracing::{error,warn,info,debug,trace}!` | `println!` / `eprintln!` / `dbg!`（`init_tracing` 创建日志目录失败的一次 `eprintln!` 除外） |
| 用户可见反馈 | toast / Windows 通知 | `addNotification` / `maybe_show_toast` | 用日志代替用户提示，或用 toast 代替调试日志 |

**原则**：日志给开发者看；通知给用户看。二者可同时发生（例如命令失败：`log_err` 记全量 + 前端 `addNotification` 摘要），但职责不互换。

### 1.2 前端（`ui/src/lib/logger.ts`）

**唯一入口**：`logger.debug / logger.info / logger.warn / logger.error(context, msg, ...args)`。

- **`context`**：模块短名，小驼峰。常用：`stores`、`events`、`invoke`、`notification`、`tauri`、`+layout`、页面/组件短名。
- **级别门控**：`currentLevel` 在 DEV 为 `debug`，生产为 `info`；`debug` 只在开发环境输出。
- **与 toast 的关系**：`addNotification(..., 'error')` 会自动 `logger.error('notification', msg)`；调用方**不要**再对同一条消息打一遍 error 日志。
- **invoke 失败**：`tauri.ts::invoke` 已在抛出前 `logger.error('invoke', ...)`；页面 `catch` 只负责用户提示，不再记日志。

```ts
import logger from '$lib/logger.ts';

logger.warn('stores', 'refreshSkills failed', e);
logger.error('+layout', 'get_settings error', e);
```

### 1.3 Rust 后端（`tracing`）

**框架**：`tracing` + `tracing_subscriber`，初始化集中在 `crates/app-binary/src/logging.rs::init_tracing`（console + 可选按日滚动文件，共用同一个 reloadable `EnvFilter("haven={level}")`）。target 必须落在 `haven` 前缀下（各 crate 名天然满足），否则会被 EnvFilter 过滤。

**级别语义**：

| 级别 | 用途 | 示例 |
|---|---|---|
| `error` | 不可恢复失败、用户可见失败、panic、命令失败 | `log_err`、任务出错、`PANIC at ...` |
| `warn` | 可恢复异常、降级、重试、冲突 | 热键冲突、录音启动失败、任务暂停 |
| `info` | 生命周期事件 | 应用初始化/退出、会话创建/完成、通知发出 |
| `debug` | 每步细节（ReAct 循环、流式 chunk） | thought/action/observation 摘要 |
| `trace` | 预留，底层数据包级 | — |

**格式约定**：

- 事件消息优先 `模块::方法: 描述`（与 `TauriEmitter::trace_event` 一致）。
- 上下文优先用 **tracing 结构化字段**（`session_id = %id`），便于 grep；存量文本内联 ID（`session {}`）保留，新增/修改优先结构化字段。
- 禁止为打日志而改变函数签名；拿不到 ID 时依赖所在并发边界的 span。

**命令错误日志**：Tauri 命令失败必须走 `log_err(ctx, e)`（`logging.rs`，经 `commands` 再导出），固定输出两行：

```text
command `{ctx}` failed
command error: {e}
```

保留 `command error:` 前缀行是为了让日志采集可稳定 grep。禁止手写 `map_err(|e| e.to_string())` 而不记录日志。

**Panic**：全局 panic hook 已设置，自动输出 `PANIC at {file}:{line}: {msg}` + backtrace；业务代码无需自行处理 panic。

### 1.4 ID 上下文（多会话 / 多任务并行可区分）

并发实体日志必须能区分 `session_id` / `action_id`。两条机制配合：

**a) 结构化字段**：

```rust
tracing::info!(session_id = %session_id, "dispatcher spawning handler");
tracing::warn!(action_id = %id, "failed to write output log {}: {e}", path.display());
```

**b) Span 上下文**（推荐）：在并发边界建立 `info_span!`，span 内日志自动携带字段。当前已建立：

| Span 名 | 字段 | 位置 | 覆盖范围 |
|---|---|---|---|
| `run_session` | `session_id` | `haven_agent::session`（handler `.instrument(span)`） | 整个 ReAct 循环嵌套日志 |
| `bg_action` | `action_id` | `haven_tools::bg`（runner `.instrument(span)`） | 后台任务运行 / 取消 / 输出写入 |
| `action_completion` | `action_id`, `session_id` | `haven_agent::layer` 任务完成 consumer | 任务结果注入 / 会话唤醒 / 通知 |
| `scheduled_action_fired` | `action_id`, `session_id` | `haven_agent::layer` 定时任务 consumer | 定时任务触发与会话恢复 |

规则：

- 函数已有 ID 参数/变量 → 优先结构化字段（a）；否则依赖 span（b）。
- 全局性日志（初始化、统计、健康探测）不强制带 ID。

### 1.5 运行时调整

日志级别通过 `AppState.log_filter_handles` 中的 `reload::Handle` 在运行时修改（console 与文件同步）。代码不得绕过该句柄直接改 `EnvFilter`。设置页「Logging」区块写入后走同一通道。

---

## 2. 通知规范

### 2.1 分层模型（双通道）

```
AgentEvent / 其它后端事件
        │
        ├─ emit(channel, payload) ──► +layout registerListeners
        │                                 └─ notifyCfg.*.in_app ? addNotification : 忽略
        │
        └─ maybe_show_toast ─────────► Windows 桌面通知
                                          └─ config.notification.*.windows ? show : 忽略
                                             （AgentEvent::Notification 例外：默认总是弹 Windows）
```

- **后端**只发事件 +（按配置）弹桌面通知，**绝不**直接操作前端 toast store。
- **前端**事件驱动 toast 集中在 `+layout.svelte` 的 `registerListeners`；页面组件**禁止**自行 `listen` 后再弹系统级事件 toast。
- **页面内用户操作反馈**（保存失败、复制成功等）可在页面直接 `addNotification`，不走事件总线。

### 2.2 谁可以弹 toast

| 场景 | 入口 | 是否受 `notifyCfg` 控制 |
|---|---|---|
| 会话生命周期（创建/完成/暂停/恢复/出错） | `+layout` 事件 handler | 是（`in_app`） |
| Agent 显式 `notify` / 定时任务通知 | `notification:show` | 否（始终 toast；Windows 亦默认开） |
| 录音 / 转写 / 静音 / 热键冲突 / MCP 状态 | `+layout` 事件 handler | 否（操作反馈，无独立配置项） |
| 后台任务完成（非当前会话） | `+layout` `action:finished` | 否；当前会话内完成不弹（对话里已有结果） |
| 用户点击触发的命令结果 | 页面 / helper 直接 `addNotification` | 否 |

### 2.3 应用内 toast API（`ui/src/lib/stores.ts`）

**唯一入口**：`addNotification(msg, type = 'info', duration = 3000)`。

- `type`：`info` | `success` | `warning` | `error`（由 `NotificationToast.svelte` 渲染）。
- **去重**：同 `msg` + 同 `type` 已在队列中则不再插入。
- **时长建议**：

| type | 建议 duration | 说明 |
|---|---|---|
| `info` | 1500–3000 | 轻量确认（已复制、已切换）用短；状态提示用 3s |
| `success` | 2500–3000 | |
| `warning` | 3000–4000 | |
| `error` | 4000–5000 | 上限 5s；需长时间阅读也不超过 5s |

- **文案语言**：与 UI 一致使用**中文**；变量用模板字符串拼接。专有名词（`Balanced Model`、`MCP`、产品名 `Haven`）可保留英文。
- `error` 类型自动 `logger.error('notification', msg)`，调用方不再重复记日志。

```ts
import { addNotification } from '$lib/stores.ts';

addNotification(`会话已完成: ${title}`, 'success');
addNotification(`MCP 已断开: ${name}`, 'warning', 4000);
addNotification(e?.message || '操作失败', 'error', 4000);
```

### 2.4 后端事件与桌面通知

- **事件命名**：`domain:action`（`session:created`、`agent:thought`、`recording:error`、`notification:show` …）。`AgentEvent` → channel 的唯一事实来源是 `TauriEmitter::channel`；新增变体必须登记并补单测。
- **任务（action）事件**：后台任务与定时任务共用 `action:created` / `action:updated` / `action:output` / `action:finished`，由 `haven_tools` 直接 emit，不走 `TauriEmitter::channel`。payload：后台带 `action_id`，定时带 `id`；前端 `actionStore` 归一化（`id = payload.id || payload.action_id`，`kind: 'background'|'scheduled'`）。
- **wire 载荷**：统一 snake_case JSON；前端边界转 camelCase。敏感/内部字段不外泄（见 `payload` 对 `SessionCreated` 的投影）。
- **桌面通知**：统一走 `DesktopNotifications::maybe_show_toast`（`notification.rs`，由 `TauriEmitter` 委托）。文案与应用内 toast 对齐（中文）：

| 事件 | 读配置键 | 默认 windows | 文案 |
|---|---|---|---|
| `SessionCreated` | `session_created.windows` | `false` | `新会话: {title\|\|id}`（禁止用 `input`） |
| `SessionCompleted` | `session_completed.windows` | `true` | `会话已完成: …` |
| `SessionError` | `session_error.windows` | `true` | `会话出错: …` |
| `SessionUpdated` status=`paused` | `session_paused.windows` | `false` | `会话已暂停: …` |
| `SessionUpdated` status=`pending`（且上一状态为 paused/error） | `session_resumed.windows` | `false` | `会话已恢复: …` |
| `AgentEvent::Notification` | **不读配置**，总是弹 | — | title/body 原样（设置页注明始终开启） |

标题统一产品名 `Haven`。

### 2.5 配置（`NotificationConfig`）

定义于 `crates/common/src/config/misc.rs`：

```text
session_created / session_completed / session_paused / session_resumed / session_error
  └─ NotifyChannels { in_app: bool, windows: bool }
```

默认值：`in_app` 全部 `true`；`windows` 仅 `session_completed` / `session_error` 为 `true`，其余 `false`。

- 设置页「Notifications」网格与此五键一一对应。
- 新增可配置通知事件时：**结构体 Default + 设置页 + `maybe_show_toast` + `+layout` in_app 判断** 四步同步。

### 2.6 新增通知事件流程模板

1. 若属 `AgentEvent`：加变体 → `TauriEmitter::channel` / `payload` /（可选）`maybe_show_toast` / `trace_event` → 补 `lib.rs` 单测。
2. 若属任务事件：在 `haven_tools` emit，前端 `actionStore` 归一化。
3. 前端在 `+layout.svelte` 的 `registerListeners` 增加 handler；需要用户开关则接 `notifyCfg`。
4. 需要设置项时扩展 `NotificationConfig` + Settings 网格。
5. 更新本文 §4 映射表。

---

## 3. 错误处理规范

### 3.1 Rust 后端

- **crate 内部**：`thiserror` 领域错误，或 `anyhow::Result`。
- **Tauri 命令层**：统一 `Result<T, String>`；失败经 `log_err(ctx, e)` 记 ERROR 后再把 `e.to_string()` 返回前端。
- **结构化错误**：需携带结构（如 `requires_confirmation`）时用 JSON 字符串，仍走 `Result<T, String>`。
- **降级**：可恢复的初始化失败走降级启动（`degraded_app_state`），`tracing::error!` 后返回降级实例，绝不静默。

禁止：

- 命令 `Err` 不经 `log_err` 直接 `e.to_string()`；
- 把 API key、内部路径等细节原样返回前端（日志记全量，前端拿摘要）；
- 用 `panic!` / `unwrap()` 处理预期内失败（`expect` 仅限初始化等不可恢复场景）。

### 3.2 前端

```ts
try {
	const result = await invoke('xxx', args);
} catch (e) {
	addNotification(e?.message || '操作失败', 'error', 4000);
}
```

- `invoke` 已记日志 → catch 只做用户提示。
- 失败文案统一 `formatError(e)`（`ui/src/lib/formatError.ts`），拼进 toast：`` `操作失败: ${formatError(e)}` ``；无上下文时用中文兜底（`操作失败`）。
- 页面级重复逻辑可收敛为局部 helper（先例：settings 的 `notifyFetch`，带 per-key 节流）。
- 事件监听注册失败：只走 `events.ts`（内部 `logger.error` 后吞掉，不阻塞 mount）；页面不得裸 `listen`。
- catch 后禁止仅打日志而无用户提示（除非确认无需用户感知，则只用 `logger.*`，不弹 toast）。
- 禁止 `` `${e}` `` 直接拼 toast（对象会变成 `[object Object]`）。

---

## 4. 跨端映射表

### 4.1 受 `notifyCfg` 控制

| 后端事件 | 前端 toast | 配置键 |
|---|---|---|
| `session:created` | info：`新会话: …`（4s） | `session_created.in_app` |
| `session:completed` | success：`会话已完成: …` | `session_completed.in_app` |
| `session:error` | error：`会话出错: …`（5s） | `session_error.in_app` |
| `session:updated` status=`paused` | warning：`会话已暂停: …`（3s） | `session_paused.in_app` |
| `session:updated` status=`pending`（仅当上一状态为 paused/error） | info：`会话已恢复: …`（3s） | `session_resumed.in_app` |

### 4.2 不受配置控制（操作 / 系统反馈）

| 后端事件 | 前端表现 |
|---|---|
| `notification:show` | info toast（title 为默认 `Haven` 时只显示 body；5s）+ Windows 桌面通知 |
| `mcp:status_change` | Connected→success（冷启动跳过）/ Disconnected→warning / Offline→error |
| `hotkey:conflict` | error toast：`热键冲突: …`（5s） |
| `agent:balanced_model` | warning toast + 状态芯片（仅当前会话） |
| `recording:error` / `transcription:*` / `mute:changed` | 对应中文提示 + overlay |
| `action:finished`（后台，非当前会话） | success/error：`后台任务完成/失败: {action_id}`（4s） |
| 命令 invoke 失败 | error toast（`e.message` 或兜底，4s） |

### 4.3 只更新状态、不弹 toast

| 事件 | 行为 |
|---|---|
| `session:updated` completed/error（副发） | 更新 busySessions / modelState；toast 由主通道负责 |
| `session:deleted` | 清理 busySessions |
| `action:created` / `action:updated` / `action:output` | 更新 `actionStore` |
| `agent:stream_stalled` | `updateModelState('stalled')` |
| `llm:config_changed` | 重新探测 LLM 连通性 |
| `skills:status_change` | 由工具页刷新，不弹 toast |

---

## 5. 新增代码检查清单

- [ ] 日志：前端只用 `logger.*`；后端只用 `tracing`；命令错误走 `log_err`；并发路径带 `session_id`/`action_id` 或落在已有 span 内。
- [ ] 通知：系统事件进 `+layout`；用户操作可页面直调 `addNotification`；新可配置事件按 §2.6 走完四步。
- [ ] 错误：后端 `Result<T, String>` + 日志 + 用户可读摘要；前端 try/catch + toast，不重复记日志。
- [ ] 文案：应用内中文；产品名 `Haven`；专有模型角色名按命名约定。
- [ ] 桌面通知：走 `maybe_show_toast`，读对应 `*.windows`（`AgentEvent::Notification` 除外）。
- [ ] 更新本文 §4 映射表。

---

## 6. 待对齐 / 可优化

### 已完成（v1.2）

| ID | 变更 |
|---|---|
| N1 | Windows 桌面通知文案改为中文，与应用内 toast 对齐 |
| N2 | `session_paused.windows` 已接线（`SessionUpdated`/`paused`） |
| N3 | 设置页注明：Agent `notify` 通知始终开启（双通道） |
| N4 | `hotkey:conflict` toast 改为 `热键冲突: …` |
| N5 | 抽出 `windows_enabled` / `show_windows_toast` / `session_display_title` |
| L1 | span 重命名为 `scheduled_action_fired`，字段 `action_id` |
| L3 | 设置页 Logging 注明级别仅作用于后端 tracing |
| E1 | 新增 `ui/src/lib/formatError.ts`，toast 错误文案统一经 `formatError(e)` |

### 仍待渐进

| ID | 问题 | 建议 | 优先级 |
|---|---|---|---|
| L2 | `TauriEmitter::trace_event` 仍多用文本内联 ID | 新增/改动时改为结构化字段 `session_id = %id`，存量渐进 | P3 |

落地时：改代码须同步更新 §2 / §4。

---

## 7. 「Haven」/「haven」大小写规范

产品名统一大写 **Haven**，仅用于**用户可见的展示字符串**；其余**标识 / 路径 / 协议字段一律小写 `haven`**。禁止同一语义在不同地方混用大小写。

| 域 | 大小写 | 示例 |
|---|---|---|
| 窗口标题 / 托盘 tooltip / 系统通知标题 | `Haven` | `tauri.conf.json` `productName`、`app-binary` 通知与托盘文案 |
| 通知默认标题（`notify` / `scheduled_action` 工具） | `Haven` | `tool.rs` / `notify.rs` / `scheduled_action.rs` 默认 title |
| 前端 UI 文案（欢迎页、气泡标签、设置页） | `Haven` | `+page.svelte`、`ChatBubble.svelte`、`Logo.svelte` |
| Windows 计划任务名（Action Scheduler 中展示） | `Haven` | `app-binary/src/autostart.rs` `ACTION_NAME` |
| 数据目录 / 临时工作目录 / 日志文件名 | `haven` | `ConfigLoader::data_dir()`、`default_work_dir()`、`haven.log` |
| 进程名 / crate / 包名 / Tauri identifier | `haven` | `haven-app-binary`、`haven-ui`、`com.haven.app` |
| localStorage / kv 键 | `haven` | `haven.theme`、`haven.accent`、`haven.no_auto_restore` |
| MCP `clientInfo.name` 等协议标识 | `haven` | `crates/tools/src/mcp/mod.rs` |
| 测试 fixture 中的实体数据（subject 等） | `haven` | `inference.rs` / `prompt.rs` 测试 |

判别方法：**会显示给用户看 → `Haven`；会被机器比较 / 拼接成路径 / 写入存储 → `haven`。**
