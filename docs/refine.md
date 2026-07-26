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

## 3. 工具定义与 API 参数冗余

Pi 只将工具 schema 通过 API `tools` 参数传递，提示词中仅列名称行。Haven 同时在提示词和 API 中传递完整 JSON schema。

**分析：** 双重传递在模型不支持 `tools` 参数时有价值，但主流模型均已支持，目前策略可以优化。还需关注：
- `build_system_prompt` 每次调用都重建所有工具 schema → 可缓存 schema JSON 字符串，仅工具注册表变化时重建
- 工具描述可进一步精简（去掉 `type`、`required` 等冗余字段）

## 4. 上下文压缩触发策略

Pi 的 `pi-compactor` 在接近 token 限制时压缩。Haven 的 `ContextCompactor` 类似但：
- 仅在 LLM 返回 `ContextLengthExceeded` 错误时触发被动压缩（`run_task` 第 1019-1120 行），而非在调用前主动预防
- `maybe_compact` 虽在每次 LLM 调用前检查，但阈值保守（`context_window - reserve_tokens`）
- 建议：主动压缩阈值应更激进，预留更多 buffer 避免昂贵的重试

## 5. 熔断机制未覆盖工具执行

`LlmRouter` 的 `CircuitBreaker` 仅保护 LLM API 调用。工具执行层（`ToolsManager::execute_tool`）虽支持重试，但**没有熔断**——某个工具持续失败时不会快速拒绝。

**实现建议：** 在 `ToolRegistry` 或 `ToolsManager` 增加 per-tool 熔断器，连续失败 N 次后快速失败，冷却后再试。

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

## 9. AgentLayer 职责过重 — 待重构

### 9.1 现状

Pi 的 `pi-agent-core`（418 行）仅关注 ReAct loop。Haven 的 `AgentLayer`（1865 行，不含测试）**混合了 8 种不同职责**，且全部在 `crates/agent/src/lib.rs` 单个文件中：

| 职责 | 方法/代码段 | 行数 | 占比 |
|------|-----------|------|------|
| ReAct loop | `run_react_loop`, `run_task`, `run_task_resumed`, `run_task_from_id` | ~460 | 25% |
| Session 管理 | `ensure_session`, `start_new_session`, `persist_message` | ~60 | 3% |
| System prompt 构建 | `build_system_prompt`, `build_tool_definitions` | ~155 | 8% |
| 事件发射 | `emit_thought`, `emit_action`, `emit_observation`, `emit_*` × 10 | ~135 | 7% |
| 响应解析 | `parse_reasoner_response` | ~50 | 3% |
| Snapshot 持久化 | `save_snapshot_with_branches`, `save_branch_point` | ~35 | 2% |
| 上下文压缩 | `maybe_compact` + `ContextCompactor`（单独文件） | ~20 + 248 | 1%+13% |
| 推理编排 | `run_fact_inference`, `run_preference_inference` | ~30 | 1.5% |
| 用户输入入口 | `process_input`, `supplement_task`, `reopen_task` | ~110 | 6% |
| 分支/回滚 | `get_branch_points`, `rollback_task`, `fork_task` | ~65 | 3.5% |
| 测试代码 | `#[cfg(test)] mod tests` | ~425 | 23% |

**问题：**
1. **耦合度高** — 所有方法共享 `self` 对 `AgentLayer` 全部 13 个字段的访问权，哪怕只用到其中 2-3 个
2. **难以独立测试** — 测试 ReAct loop 必须构造完整的 `AgentLayer`（含 DB、TaskExecutor、LlmRouter、emitter）
3. **修改风险大** — 修改 session 管理可能意外影响 ReAct loop 的字段访问（如 `session_id` Mutex 锁范围）
4. **职责粒度不一致** — 事件发射既有纯转发（`emit_action` 仅发事件）又有逻辑混合（`emit_thought` 同时写 DB）
5. **静态方法缺失** — `parse_reasoner_response` 不需要 `&self` 却定义在 `AgentLayer` 上

### 9.2 目标架构

提取为 5 个独立模块，依赖关系单向：

```
┌──────────────────────────────────────────────────────────────────┐
│  AgentLayer (orchestrator, ~200 lines after extraction)          │
│  - 持有拆分后模块的引用                                          │
│  - process_input() 作为唯一外部入口                               │
│  - run_task_from_id() 编排 ReActEngine 调用周期                    │
│  - 协调模块间通信                                                │
└────────────────┬───────────────────────┬─────────────────────────┘
                 │                       │
    ┌────────────┼───────┬───────────────┼──────────┐
    ▼            ▼       ▼               ▼          ▼
┌────────┐ ┌──────────┐ ┌──────────┐ ┌────────┐ ┌──────────────┐
│Session │ │SystemPrompt│ │ReActEngine│ │Inference│ │EventEmitter  │
│Manager │ │Builder    │ │          │ │Engine  │ │(trait, 已提取)│
└────┬───┘ └──────────┘ └─────┬────┘ └───┬────┘ └──────────────┘
     │                        │          │
     ▼                        ▼          ▼
  ┌──────────────────────────────────────────┐
  │  Database (haven-memory, 外部依赖)         │
  │  TaskExecutor (haven-task, 外部依赖)        │
  │  LlmRouter (haven-llm, 外部依赖)           │
  └──────────────────────────────────────────┘
```

### 9.3 模块拆分详情

#### 9.3.1 `SessionManager` — session 生命周期管理

**提取自** `AgentLayer` 的 `session_id: Mutex<String>`、`ensure_session`、`start_new_session`、`persist_message`、`session_window_size`。

```rust
pub struct SessionManager {
    db: Arc<Database>,
    session_id: Mutex<String>,
    session_window_size: usize,
}

impl SessionManager {
    pub fn new(db: Arc<Database>, session_window_size: usize) -> Self;

    /// 返回当前活跃 session ID，如为 "default" 占位则从 DB 加载
    pub fn ensure_session(&self) -> String;

    /// 关闭当前 session，创建新 session，切换激活
    pub fn start_new_session(&self) -> anyhow::Result<String>;

    /// 持久化消息到当前 session，附带窗口大小裁剪
    pub fn persist_message(&self, role: &str, content: &str, message_type: &str);

    /// 获取当前 session 消息（供 SystemPromptBuilder 和 ReActEngine 使用）
    pub fn get_conversation_history(&self) -> Vec<SessionMessage>;

    /// 获取当前 session ID（不加锁的简单 getter）
    pub fn current_session_id(&self) -> String;

    /// 切换到指定 session（用于 supplement_task 等需要跨 session 操作的场景）
    pub fn switch_to_session(&self, session_id: &str);
}
```

#### 9.3.2 `SystemPromptBuilder` — 提示词构建 + 工具 schema 缓存

**提取自** `AgentLayer` 的 `build_system_prompt`、`build_tool_definitions`。消除每次 LLM 调用时重建工具 schema 的重复开销。

```rust
pub struct SystemPromptBuilder {
    tools: Arc<ToolsManager>,        // 用于构建工具定义
    db: Arc<Database>,               // 用于事实/偏好查询
    schema_cache: RwLock<HashMap<String, String>>,  // 工具 schema JSON 缓存
    cache_version: AtomicU64,        // 工具注册表变更时递增
}

impl SystemPromptBuilder {
    pub fn new(tools: Arc<ToolsManager>, db: Arc<Database>) -> Self;

    /// 构建完整 system prompt（工具定义 + 事实偏好 + 指令）
    pub async fn build(
        &self,
        task_description: &str,
        history: &[ReActStep],
        conversation_history: &[SessionMessage],
    ) -> String;

    /// 仅刷新工具定义缓存（当工具注册表变化时主动调用）
    pub fn invalidate_cache(&self);
}
```

**关键改进：**
- `schema_cache` 缓存 `Vec<ToolDefinition>` 的 JSON 表示，工具注册表未变化时直接返回
- `invalidate_cache` 在 ToolsManager 注册/注销工具时调用
- 与 `ReActEngine` 解耦：prompt 构建不依赖 loop 状态

#### 9.3.3 `ReActEngine` — ReAct 循环核心

**提取自** `AgentLayer` 的 `run_react_loop`、`run_task`、`run_task_resumed`、`parse_reasoner_response`、`maybe_compact`、snapshot 持久化（`save_snapshot_with_branches`、`save_branch_point`）、chunk consumer 逻辑。

```rust
pub struct ReActEngine {
    router: Arc<RwLock<Arc<LlmRouter>>>,
    executor: Arc<TaskExecutor>,
    db: Arc<Database>,
    compactor: ContextCompactor,
    max_steps: Mutex<u32>,
    max_observation_chars: usize,
    fallback_notified: Mutex<HashSet<String>>,
    run_counter: AtomicU64,
    current_run_id: AtomicU64,
}

// ReActEngine 从 AgentLayer 移入但不公开的辅助类型
pub(super) struct ChunkConsumer {
    task_id: String,
    run_id: u64,
    step_number: u32,
}

impl ReActEngine {
    pub fn new(
        router: Arc<RwLock<Arc<LlmRouter>>>,
        executor: Arc<TaskExecutor>,
        db: Arc<Database>,
        compactor: ContextCompactor,
        max_steps: u32,
        max_observation_chars: usize,
    ) -> Self;

    /// 完整 ReAct 循环（从 run_react_loop 迁移）
    pub async fn run_react_loop(
        &self,
        task_id: &str,
        canonical: &mut Vec<ChatMessage>,
        history: &mut Vec<ReActStep>,
        start_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &dyn AgentEventEmitter,
    ) -> anyhow::Result<Vec<ReActStep>>;

    /// 解析 LLM 响应为 thought + actions
    pub fn parse_reasoner_response(
        response: &LlmResponse,
        step_number: u32,
    ) -> (Option<String>, Vec<Action>);

    /// 触发上下文压缩（在每次 LLM 调用前调用）
    pub async fn maybe_compact(
        &self,
        task_id: &str,
        canonical: &mut Vec<ChatMessage>,
        emitter: &dyn AgentEventEmitter,
    );

    // Snapshot 持久化（含 branch points）
    pub fn save_snapshot_with_branches(
        &self,
        task_id: &str,
        canonical: &[ChatMessage],
        history: &[ReActStep],
        branch_points: &HashMap<u32, BranchPoint>,
    );
}
```

**关键设计：**
- `ReActEngine` **不持有 `AgentEventEmitter` 引用**，每个需要发射事件的方法通过参数传入 `&dyn AgentEventEmitter`——这使事件源清晰，也便于测试时传 mock
- `parse_reasoner_response` 改为关联函数（不依赖 `&self`），纯函数更易测试
- `run_react_loop` 的 canonical/history/branch_points 由调用者（AgentLayer）传入 `&mut` 引用，状态归 AgentLayer 管理
- `ReActEngine` **不知道 session、不知道 prompt 构建**，只负责循环逻辑

#### 9.3.4 `InferenceEngine` — 事实/偏好推断编排

**提取自** `AgentLayer` 的 `run_fact_inference`、`run_preference_inference`。

```rust
pub struct InferenceEngine {
    db: Arc<Database>,
    session_manager: Arc<SessionManager>,  // 用于获取会话消息
}

impl InferenceEngine {
    pub fn new(db: Arc<Database>, session_manager: Arc<SessionManager>) -> Self;

    /// 执行用户事实推断（从当前 session 用户消息中提取事实）
    pub fn infer_facts(&self);

    /// 执行用户偏好推断（从当前 session 消息中提取偏好）
    pub fn infer_preferences(&self);

    /// 在一次调用中同时执行两者（当前 ReAct loop 退出点的调用方式）
    pub fn infer_all(&self);
}
```

**关键改进：**
- `InferenceEngine` 获取会话消息通过 `SessionManager` 而非直接操作 DB——统一会话管理入口
- 当前推断逻辑是同步的规则匹配，未来可切换为 LLM 驱动的推断而不影响调用者
- `infer_all` 方法封装当前 ReAct 退出点调用的模式，AgentLayer 只需一行：`self.inference.infer_all()`

#### 9.3.5 `EventEmitter`（已有 trait，保持现状）

`AgentEventEmitter` trait 和 `AgentEvent` enum 已足够抽象，无需大改。但 `emit_*` 方法应从 `AgentLayer` 移出为一个 **helper 模块**，专注于事件构造 + 发射：

```rust
// crates/agent/src/event.rs
pub struct EventDispatcher {
    emitter: Arc<Mutex<Option<Arc<dyn AgentEventEmitter>>>>,
}

impl EventDispatcher {
    pub fn new() -> Self;
    pub fn set_emitter(&self, emitter: Arc<dyn AgentEventEmitter>);

    pub async fn emit_thought(&self, task_id: &str, thought: &str, step_number: u32, run_id: u64,
        db: &Database);  // emit_thought 同时写 DB
    pub async fn emit_action(&self, task_id: &str, ...);  // 纯事件转发
    pub async fn emit_observation(&self, task_id: &str, ...);  // 纯事件转发
    pub async fn emit_task_created(&self, task: &TaskInfo);
    pub async fn emit_task_completed(&self, task_id: &str, title: &str,
        fallback_notified: &mut HashSet<String>);
    pub async fn emit_task_error(&self, task_id: &str, error: &str,
        fallback_notified: &mut HashSet<String>);
    pub async fn emit_supplement(&self, ...);
    pub async fn emit_fallback_activated(&self, task_id: &str, reason: &str,
        fallback_notified: &mut HashSet<String>);
    pub async fn emit_compaction(&self, ...);
}
```

**注意：** `emit_thought` 同时写 DB，这是与 db 的耦合。提取后通过参数传入 `&Database` 而非 `&self.db`，使副作用显式化。`fallback_notified` 去重逻辑也从 `AgentLayer` 的 Mutex 字段改为由调用者传入 `&mut HashSet`。

### 9.4 提取后的 `AgentLayer`（~200 行 orchestrator）

```rust
pub struct AgentLayer {
    // 持有提取后的模块
    sessions: Arc<SessionManager>,
    prompt_builder: Arc<SystemPromptBuilder>,
    react_engine: Arc<ReActEngine>,
    inference: Arc<InferenceEngine>,
    events: Arc<EventDispatcher>,

    // 外部依赖引用（原有，保持暴露）
    db: Arc<Database>,
    executor: Arc<TaskExecutor>,
}

impl AgentLayer {
    // 原有公共 API 不变（向后兼容）

    // 新构造函数：在内部构造各子模块
    pub fn new(
        db: Arc<Database>,
        executor: Arc<TaskExecutor>,
        router: Arc<RwLock<Arc<LlmRouter>>>,
        max_steps: u32,
        session_window_size: usize,
        max_observation_chars: usize,
    ) -> Self;

    // process_input：现有逻辑保持不变，但内部调用 sessions.process_input(...)
    pub async fn process_input(&self, transcript: &str, active_task_id: Option<&str>)
        -> ProcessResult;

    // run_task_from_id：协调 SessionManager + SystemPromptBuilder + ReActEngine
    pub async fn run_task_from_id(&self, task_id: &str)
        -> anyhow::Result<Vec<ReActStep>>;

    // start：与现有一致，wiring TaskExecutor + self 的 RunHandler
    pub fn start(self: Arc<Self>);
}
```

### 9.5 抽取后的字段分布

当前 `AgentLayer` 的 13 个字段在提取后分布：

| 字段 | 移至 | 原因 |
|------|------|------|
| `db: Arc<Database>` | 保留在 AgentLayer | 多个子模块共享 |
| `executor: Arc<TaskExecutor>` | 保留在 AgentLayer | AgentLayer 调用 executor.start_dispatcher；ReActEngine 需引用 |
| `router: Arc<RwLock<Arc<LlmRouter>>>` | `ReActEngine` | 仅 ReAct loop 使用 |
| `max_steps: Mutex<u32>` | `ReActEngine` | 仅 loop 控制 |
| `emitter: Arc<Mutex<Option<...>>>` | `EventDispatcher` | 事件发射专用 |
| `session_id: Mutex<String>` | `SessionManager` | session 管理专用 |
| `session_window_size: usize` | `SessionManager` | session 管理专用 |
| `max_observation_chars: usize` | `ReActEngine` | 仅 loop 使用 |
| `fallback_notified: Mutex<HashSet<String>>` | `ReActEngine` | 仅 loop 使用 |
| `compactor: ContextCompactor` | `ReActEngine` | 仅 loop 使用 |
| `run_counter: AtomicU64` | `ReActEngine` | 仅 loop 使用 |
| `current_run_id: AtomicU64` | `ReActEngine` | 仅 loop 使用 |

AgentLayer 自身保留：`db`、`executor`、`sessions`、`prompt_builder`、`react_engine`、`inference`、`events`。

### 9.6 文件结构重组

```
crates/agent/src/
├── lib.rs              # AgentLayer orchestrator (~200 行)
├── compactor.rs        # ContextCompactor（现有，不变）
├── session.rs          # SessionManager
├── prompt.rs           # SystemPromptBuilder
├── react.rs            # ReActEngine + parse_reasoner_response + snapshot
├── inference.rs        # InferenceEngine
├── event.rs            # EventDispatcher + AgentEvent enum + AgentEventEmitter trait
└── types.rs            # BranchPoint, ReActSnapshot, ReActStep, Action, ProcessResult
```

### 9.7 迁移策略（分 3 阶段）

#### Phase 1：纯提取，不修改逻辑（~2-3 次 PR）

1. 创建 `types.rs`：将 `BranchPoint`、`ReActSnapshot`、`ReActStep`、`Action`、`ProcessResult` 移入，在 `lib.rs` 中 `pub use`
2. 创建 `session.rs`：`SessionManager` — 直接复制 `ensure_session`、`start_new_session`、`persist_message`，AgentLayer 通过 `self.sessions.*` 调用
3. 创建 `event.rs`：`EventDispatcher` — 直接复制 10 个 `emit_*` 方法，AgentLayer 通过 `self.events.*` 调用

**风险：** 低。纯复制 + 重定向调用，不改变任何逻辑。

#### Phase 2：ReAct 循环独立（~2-3 次 PR）

4. 创建 `react.rs`：`ReActEngine` — 将 `run_task`、`run_task_resumed`、`run_react_loop`、`maybe_compact`、snapshot 持久化方法移入。AgentLayer 的 `run_task_from_id` 协调 prompt 构建 + ReActEngine 调用。

**关键：** `run_react_loop` 的签名改为接收 `&dyn AgentEventEmitter` 参数，而非通过 `self` 访问 emitter。

**风险：** 中。`run_react_loop` 内部大量使用 `self.emit_*`，需逐一改为 `emitter.emit_*`。建议先在 `AgentLayer` 内部提取局部引用，验证编译通过后再移入新文件。

5. 创建 `prompt.rs`：`SystemPromptBuilder` — 将 `build_system_prompt`、`build_tool_definitions` 移入，增加 schema 缓存。

#### Phase 3：Inference 提取（~1 次 PR）

6. 创建 `inference.rs`：`InferenceEngine` — 将 `run_fact_inference`、`run_preference_inference` 移入，通过 `SessionManager` 获取会话消息。

7. AgentLayer 缩减为 orchestrator，仅负责协调、不包含业务逻辑。

### 9.8 接口兼容性保证

- `AgentLayer` 的 `pub` 方法签名**全部保持不变**（`new`、`process_input`、`start`、`set_emitter`、`replace_router`、`set_max_steps`、`start_new_session`、`supplement_task`、`reopen_task`、`rollback_task`、`fork_task`、`get_branch_points`、`emit_task_completed`）
- 内部模块的字段均为 `pub(super)` 或 `pub(crate)`，不暴露为公共 API
- `AgentEvent`、`AgentEventEmitter`、`ReActStep`、`Action` 等公开类型保持原有导出路径

### 9.9 依赖注入测试收益

提取后各模块可独立测试：

| 模块 | 需 mock | 测试内容 |
|------|---------|---------|
| `SessionManager` | `Arc<Database>`（可用 `Database::open_in_memory()`） | session CRUD、窗口裁剪、会话切换 |
| `SystemPromptBuilder` | `Arc<ToolsManager>` + `Arc<Database>` | prompt 构建、schema 缓存刷新 |
| `ReActEngine` | `MockLlmClient` + `MockTaskExecutor` + `MockAgentEventEmitter` | 完整 ReAct 循环、熔断、压缩、分支/回滚 |
| `parse_reasoner_response` | 无（纯函数） | 各种 LLM 响应格式解析 |
| `InferenceEngine` | `Arc<Database>` | 事实/偏好推断编排 |
| `EventDispatcher` | `MockAgentEventEmitter` | 事件发射、去重、DB 写 |

当前 `AgentLayer` 存在大量 `#[cfg(test)]` 测试混合的类型定义（mock struct、辅助函数），提取后可将测试移至各模块文件，`lib.rs` 的测试仅保留 orchestrator 级别的集成测试（~50 行）。

### 9.10 增量重构检查清单

- [ ] Phase 1a: 提取 types.rs（BranchPoint, ReActSnapshot, ReActStep, Action, ProcessResult）
- [ ] Phase 1b: 提取 session.rs（SessionManager）
- [ ] Phase 1c: 提取 event.rs（EventDispatcher）
- [ ] Phase 2a: 提取 react.rs（ReActEngine + 响应解析 + snapshot）
- [ ] Phase 2b: 提取 prompt.rs（SystemPromptBuilder + schema 缓存）
- [ ] Phase 3: 提取 inference.rs（InferenceEngine）
- [ ] 最终：缩减 lib.rs，仅保留 orchestrator + 公共 re-export
- [ ] 验证：`cargo test -p haven-agent` 全部通过
- [ ] 验证：`cargo clippy -p haven-agent` 无 warning

## 10. 死代码与未使用功能 — 已完成

crates/agent/src/lib.rs 中存在：
- `#[allow(dead_code)]` 标记的方法：`save_snapshot`（第 768 行）、`rollback_to_step`（第 812 行）
- ReAct loop 的 `outer loop`（第 863/1415 行 `loop { for step_num in ... }`）的 `followups` 处理——`get_followup` 实现在 TaskExecutor 但当前没有生产者调用 `add_followup`

提议：
- 移除未使用的 `rollback_to_step` 静态方法（rollback_task 已用不同方式实现）
- 明确 `followup_queue` 的生产者接入点
- 或移除 followup 机制简化代码

## 11. 测试覆盖率缺口

- `crates/agent` 测试集中在纯逻辑（解析、构造），缺少对 `run_task` 核心循环的集成测试
- `crates/task` 的 dispatcher 测试使用 sleep-based 同步，存在 flakiness 风险
- `FuturesUnordered` 并行执行路径无测试覆盖
- Compaction 重试路径无测试

**参考 Pi：** Pi 提供 `MockLlmClient` 和 `MockToolExecutor` 用于完整的 ReAct 集成测试。

## 12. 前端 event 频率控制

`AgentEvent::ThoughtChunk` 和 `ReasoningChunk` 对每个 token 分别 emit。高频事件通过 Tauri 的 `invoke` 到前端时可能造成性能瓶颈。

**改进建议：**
- 在 chunk 发射端增加 micro-batch（每 50ms 或每 10 个 chunk 聚合一次 emit）
- 或使用 `tokio::sync::watch` channel 替代 unbounded channel，支持背压（现有 unbounded channel 无背压，内存可能暴涨）
