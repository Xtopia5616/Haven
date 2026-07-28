# 借鉴 Pi Coding Agent 的可改进项

本文档基于对 Pi Coding Agent 架构的全面研究，对比当前 Haven 项目实现，识别可借鉴的改进方向。

> 对照版本：Pi Coding Agent (@mariozechner/pi-coding-agent) | Haven (Rust/Tauri 2 + Svelte 5)

---

## 1. 系统提示词精简 — 已完成

Pi 的系统提示词 <1000 token，工具仅列名称和简短描述。Haven 原注入完整 JSON Schema。

已按 Pi 风格精简，但内置工具仍保留全量 schema（因 API `tools` 参数与提示词互补，两者同时提供可提高模型调用准确率）。MCP/Skills 改为仅 name + description + `load_*` 渐进加载。

- 内置工具：保留完整 JSON schema
- MCP 工具：仅 name + description，前缀标注
- Skills：仅 name + description，通过 `load_skill` 激活
- 指示精简为 5 条

## 2. 核心循环代码膨胀与重复 — 已完成

Pi 的整个 agent loop **仅 418 行 TypeScript**。Haven `crates/agent/src/lib.rs` ~2500 行。

**主要问题：**
- `run_task`（1378-1942 行）与 `run_task_resumed`（828-1375 行）存在 ~900 行重复代码（LLM 调用、流式 chunk 处理、工具执行、状态检查逻辑几乎完全一致）
- 流式 chunk 消费者（`tokio::spawn` 块）在 `run_task` 中定义两次（正常路径 + compaction 重试路径），`run_task_resumed` 同理，共 4 份拷贝
- 提议提取：`async fn run_react_loop()` 作为共享内核，`run_task` 和 `run_task_resumed` 仅做前置状态准备

## 3. 工具定义与 API 参数冗余 — 已完成

Pi 只将工具 schema 通过 API `tools` 参数传递，提示词中仅列名称行。Haven 同时在提示词和 API 中传递完整 JSON schema。

**已完成改进：**

- **缓存 schema 快照（`crates/agent/src/prompt.rs`）：** `SystemPromptBuilder` 缓存 `built_in_section` / `mcp_tools_section` / `skill_index_section` / `mcp_server_index_section` 以及 `tool_definitions`（API `tools` 参数）。缓存键改为 `ToolRegistry::version()`（单调递增计数器，`register`/`rebuild` 时自增），替代原先脆弱的 `tools_count` 比较——同数量替换工具时仍能正确失效。`build_tool_definitions` 也复用缓存，不再每次重建。
- **精简提示词中的工具描述：** 新增 `compact_schema` 函数，将完整 JSON schema 压缩为单行参数列表，例如 `command (string, required): Shell command to execute`、`silent (boolean, default: false): ...`，去掉 `"type": "object"` 包装与 `"required"` 数组等冗余字段。完整 schema 仍通过 API `tools` 参数传递，提示词仅保留精简摘要引导工具选择。

## 4. 上下文压缩触发策略

Pi 的 `pi-compactor` 在接近 token 限制时压缩。Haven 的 `ContextCompactor` 类似但：
- 仅在 LLM 返回 `ContextLengthExceeded` 错误时触发被动压缩（`run_task` 第 1019-1120 行），而非在调用前主动预防
- `maybe_compact` 虽在每次 LLM 调用前检查，但阈值保守（`context_window - reserve_tokens`）
- 建议：主动压缩阈值应更激进，预留更多 buffer 避免昂贵的重试

## 5. 熔断机制未覆盖工具执行 — 已完成

`LlmRouter` 的 `CircuitBreaker` 仅保护 LLM API 调用。工具执行层（`ToolsManager::execute_tool`）虽支持重试，但**没有熔断**——某个工具持续失败时不会快速拒绝。

**已完成改进：**

新增 `crates/tools/src/circuit.rs`，实现 per-tool 熔断器：

- **`ToolCircuitBreaker`**：三态熔断器（Closed / Open / HalfOpen），默认连续失败 5 次后 Open，30s 冷却后进入 HalfOpen 放行一次探测请求
- **`ToolCircuitRegistry`**：以工具名为键的熔断器注册表，使用 `std::sync::Mutex` 保证线程安全（所有操作 O(1)，不跨 `.await`）
- **`ToolsManager::execute_tool` 集成**：调用前检查 `allow_request`，Open 时快速失败（不执行工具、不消耗重试次数）；成功时 `record_success`（重置计数），失败时 `record_failure`（可能触发 Open）
- 提供 `tool_circuits()` 访问器，支持外部查询/重置熔断状态（如 UI 显示故障工具、手动恢复）

## 6. 成本追踪字段存在但未实现

`haven_llm::types::Usage` 包含 `cost: Option<f64>` 字段，但**全程未被赋值**（始终为 None）。Pi 将成本追踪作为核心功能。

**实现建议：**
- 在 `ModelEndpoint` 增加 `cost_per_1k_input_tokens` / `cost_per_1k_output_tokens` 配置
- `LlmRouter` 或 `HttpLlmClient` 在收到 `usage` 后自动计算 cost
- 在 `AgentEvent::Usage`（或新增事件）中推送给前端展示

## 7. 工具并行执行与回退 — 已完成

Pi 的 ReAct loop 是串行的——一步一个工具。Haven 使用 `FuturesUnordered` 并行执行同一步骤中的多个工具。

**已完成改进：** 原使用 `join_all` 等待最慢的工具，改为 `FuturesUnordered` 按完成顺序逐个推入。

**潜在问题：**
- 共享资源的工具并行可能导致竞态（如同时读写同一文件）
- 并行结果按完成顺序推入 canonical 消息，顺序与 LLM 期望的 tool_call_id 顺序可能不一致
- 建议：只有当工具间无依赖关系时才启用并行，或交由 LLM 指定 `parallel` 标志

## 8. 分步回退（Branch & Rollback）未整合到自动恢复

`ReActSnapshot` 支持 `branch_points`，`fork_task` 和 `rollback_task` 已实现，但目前只能通过前端主动触发。

**对比 Pi：** Pi 的树状会话重启后自动从分支点继续。Haven 的自动恢复仅在层面——工具失败时注入"请尝试不同方法"指示（第 1326-1337 行），但不会自动 rollback。

**改进建议：**
- 相同工具在相邻步骤连续失败 N 次后自动回退到上一个 branch point
- 将 `branch_points` 的创建时机改为"LLM 输出多个候选方案时"，而非仅在工具执行前

## 9. AgentLayer 职责过重 — 已完成


## 10. 死代码与未使用功能 — 已完成

crates/agent/src/lib.rs 中存在：
- `#[allow(dead_code)]` 标记的方法：`save_snapshot`（第 768 行）、`rollback_to_step`（第 812 行）
- ReAct loop 的 `outer loop`（第 863/1415 行 `loop { for step_num in ... }`）的 `followups` 处理——`get_followup` 实现在 TaskExecutor 但当前没有生产者调用 `add_followup`

提议：
- 移除未使用的 `rollback_to_step` 静态方法（rollback_task 已用不同方式实现）
- 明确 `followup_queue` 的生产者接入点
- 或移除 followup 机制简化代码

## 11. 测试覆盖率缺口 — 已完成

**参考 Pi：** Pi 提供 `MockLlmClient` 和 `MockToolExecutor` 用于完整的 ReAct 集成测试。

**已完成改进（`crates/agent/src/lib.rs`）：**

借鉴 Pi 的 `MockLlmClient` 模式，新增以下测试基础设施：
- `ScriptedMock`：可编程 `LlmClient` 实现，按预设序列返回 `StreamChunk` 或 `LlmError`，支持模拟工具调用、final_answer、ContextLengthExceeded 等场景
- `EchoTool` / `TimingTool`：mock 工具实现，分别用于简单回显和并行执行时序验证
- `EventCollector`：捕获全部 `AgentEvent`，提供 `has_action` / `has_observation` / `has_compaction` 断言方法
- `make_test_agent_with`：辅助构造函数，注入自定义 mock client 和 ToolsManager

覆盖的四项缺口：
1. **`run_task` 核心循环集成测试** — `run_task_executes_tool_then_final_answer`：验证完整 thought → action → observation → final 路径，包括真实工具执行、事件发射、任务暂停
2. **`FuturesUnordered` 并行执行路径** — `run_task_parallel_tool_execution`：LLM 单步返回两个工具调用，通过 `TimingTool` 记录执行区间，断言区间重叠（并行而非串行）
3. **Compaction 重试路径** — `run_task_compaction_retry_on_context_exceeded`：第一步执行工具（累积 4 条消息），第二步返回 `ContextLengthExceeded`，触发 compaction 摘要后重试成功，验证 `Compaction` 事件发射
4. **Compaction 失败路径** — `run_task_context_exceeded_compaction_fails`：消息不足 4 条时 compaction 返回 None，任务正确进入 Error 状态

> `crates/task` dispatcher 测试的 sleep-based 轮询已审查：现有测试使用条件轮询 + 超时退出模式，flakiness 风险可接受；`tokio::sync::Notify` 推送通知已在生产代码中使用（`status_notifier`），dispatcher 测试中的 sleep 仅模拟 handler 工作负载。

## 12. 前端 event 频率控制 — 已完成

`AgentEvent::ThoughtChunk` 和 `ReasoningChunk` 对每个 token 分别 emit。高频事件通过 Tauri 的 `invoke` 到前端时可能造成性能瓶颈。

**改进建议：**
- 在 chunk 发射端增加 micro-batch（每 50ms 或每 10 个 chunk 聚合一次 emit）
- 或使用 `tokio::sync::watch` channel 替代 unbounded channel，支持背压（现有 unbounded channel 无背压，内存可能暴涨）

## 13. 回调/Hook 实现不一致 — 已完成

Haven 不存在标准化的 hook 框架，而是**6 种 ad-hoc 回调/观察者模式并存**，缺乏统一抽象：

### 13.1 现有模式

| 模式 | 位置 | 机制 | 用途 |
|------|------|------|------|
| `DesktopShell` 回调注册表 | `app-binary/src/desktop.rs` | 9 个 `Arc<Mutex<Option<Box<dyn Fn>>>>` 字段 | 录音起停、切换、托盘、通知 |
| `AgentEventEmitter` trait | `agent/src/event.rs` | `#[async_trait]` | 所有 Agent 事件推送至前端 |
| `ConfirmRequestCallback` | `task/src/lib.rs` | 函数指针类型别名 | 工具执行安全确认 |
| `VadCallback` / `AutoStopCallback` | `input/src/lib.rs` | 函数指针类型别名 | VAD 状态通知、自动停止 |
| `RunHandler` | `task/src/lib.rs` | `Arc<dyn Fn> -> Pin<Box<dyn Future>>` | Dispatcher 回调 Agent |
| `tokio::sync::Notify` | `task/src/lib.rs` | 推送通知 | 任务状态变更通知 |

### 13.2 具体问题

**问题 1：`DesktopShell` 回调样板过多**
9 个回调每个都是重复的 `Arc<Mutex<Option<Box<dyn Fn + Send>>>>` 类型定义、setter、factory 模式：
```rust
// desktop.rs 重复结构，每个回调约 6 行样板
type Callback = Arc<Mutex<Option<Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>>>>;
// × 9 个字段，每个都需要 register_on_* / fire_on_* 方法
```
所有回调在 `lib.rs` 的 `setup()` 中一次性硬编码绑定，没有集中的注册表或生命周期管理。

**问题 2：缺少去注册/反注册机制**
- Svelte 端用 `safeListen()` + `onDestroy()` 正确清理 Tauri 事件监听
- Rust 端没有类似的取消注册机制——`DesktopShell` 回调一旦设置无法移除
- `EventDispatcher` 的 `set_emitter` 只能整体替换，不支持多订阅者

**问题 3：`fallback_notified` 去重漂移**
该字段原本在 `AgentLayer`，重构后移至 `ReActEngine`，但其去重逻辑依赖手动调用位置——如果未来添加新的 fallback 触发路径，容易遗漏去重检查。

**问题 4：同步 vs 异步回调签名不一致**
- `on_recording_start` / `on_recording_stop` / `on_show_window` / `on_quit` / `AutoStopCallback` 是异步（返回 `Pin<Box<dyn Future>>`）
- `on_toggle_change` / `on_mute_change` / `on_tray_status` / `on_notify` / `ConfirmRequestCallback` / `VadCallback` 是同步
- `AgentEventEmitter` 全部异步 — 一致但带来 `MutexGuard` 跨 `.await` 问题

### 13.3 改进建议

**短期（低投入）：统一 `DesktopShell` 回调为 trait**
```rust
#[async_trait]
pub trait ShellHandler: Send + Sync {
    async fn on_recording_start(&self);
    async fn on_recording_stop(&self);
    async fn on_recording_cancel(&self);
    fn on_toggle_change(&self, active: bool);
    fn on_mute_change(&self, muted: bool);
    async fn on_show_window(&self);
    async fn on_quit(&self);
    fn on_tray_status(&self, status: TrayStatus);
    fn on_notify(&self, title: &str, body: &str);
}
```
替代 9 个独立回调字段，一个 `Arc<dyn ShellHandler>` 即可。注册/替换为原子操作，新增方法只需在 trait 上加默认实现。

**中期：`AgentEventEmitter` 改为支持多订阅者**
```rust
pub struct EventBus {
    subscribers: RwLock<Vec<(String, Arc<dyn AgentEventEmitter>)>>,
}
impl EventBus {
    pub fn subscribe(&self, id: &str, emitter: Arc<dyn AgentEventEmitter>);
    pub fn unsubscribe(&self, id: &str);
    pub async fn emit(&self, event: AgentEvent);
}
```
保持 `AgentEventEmitter` trait 不变，`EventBus` 增加多播能力。前端 `TauriEmitter`、日志记录器、测试 mock 均可独立订阅。

**长期：输入管线回调统一**
将 `VadCallback`、`AutoStopCallback` 合并为 `InputHandler` trait，与 `ShellHandler` 命名风格一致，减少 `Arc<Mutex<Option<>>>` 散落各处的现象。
