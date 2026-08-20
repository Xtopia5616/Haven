# Haven ReAct 架构改进计划（对照 PI Agent Core）

> 状态标记：`[待办]` / `[进行中]` / `[完成]` / `[不做]`  
> 对照基线：[@earendil-works/pi-agent-core](https://github.com/earendil-works/pi/tree/main/packages/agent)（`agent.ts` ≈528 行 + `agent-loop.ts` ≈718 行；早期常说的「≈418 行」指 Agent 门面量级）  
> Haven 现状：`crates/agent` — `react/`（Phase 1 已拆）· `session.rs` ≈2.6k · `layer.rs` ≈1.6k · `event.rs` ≈1.1k  
> 更新日期：2026-08-20  
> 原则：**逐步改、行为先冻结再重构**；产品能力（SQLite resume、confirm、多 session、ask、语音 pause）保留在宿主层，不塞回薄循环。

---

## 一、对照结论

PI Agent Core 的精髓不是行数少，而是：

| PI 做法 | Haven 现状 | 后果 |
|---|---|---|
| 薄循环：`stream → tools → inject → continue\|stop` | `run_react_loop` 单函数 ≈1.5k 行，内嵌压缩/记忆/inbox/ask/snapshot/重试/web-search | 认知负载高，改一处易伤多路径 |
| 厚 hook：`transformContext` / `beforeToolCall` / `afterToolCall` / `prepareNextTurn` / `shouldStopAfterTurn` / steering·followUp | 新能力直接 splice 进 `react.rs` | 无法在不改循环的情况下扩展 |
| 单一 transcript `messages[]` | `canonical` + `history` + DB `messages`/`session_steps` 四套 | resume / sanitize / UI 对齐成本高 |
| 队列两档：steering vs followUp（带 `all`/`one-at-a-time`） | steering / supplement / answer / action_results 四通道 | ask TOCTOU、语义交叉 |
| 暂停 = 退出 run + 宿主 `continue()` | 循环内 `status_rx` 挂起等 resume | 与 dispatcher 双调度、permit 语义难推 |
| 事件驱动状态归约 | emit 与 DB 双写散落在循环各出口 | 出口路径重复、易漏 snapshot |

**不要照搬**：PI 不做持久化、多 session、confirm、语音 pause。这些留在 `session` / `layer` / hooks，薄循环只编排。

**已做得好、保留**：`PartialStore` 代次 fencing；`sanitize_canonical` 发送前闸门；`saved_at` 时间戳 resume（禁止内容去重）；`set_status_and_emit`；`SnapshotView` 借序列化；工具定义 catalog 缓存；dispatcher panic 隔离。

---

## 二、规模与热点地图

```
User / STT
  → AgentLayer::process_input          (layer.rs)     入口路由 + resume
  → SessionExecutor dispatcher         (session.rs)   FIFO + semaphore
  → ReActEngine::run_react_loop        (react.rs)     ★ 最大热点
       ├ inject / inbox / compact / infer
       ├ stream_llm_step + retries
       ├ tool batch + ask/confirm
       └ pause / snapshot / budget
  → AgentEvent → UI                    (event.rs)
```

| 文件 | 角色 | 主要问题 |
|---|---|---|
| `react.rs` | 循环本体 | 上帝对象 + 内嵌领域逻辑 |
| `session.rs` | 会话 OS | dispatcher / 队列 / confirm / tool 一体 |
| `layer.rs` | 对外门面 | ingress + resume + media + title 混杂 |
| `types.rs` | Snapshot / Step | 双 transcript（canonical+history） |
| `event.rs` | 事件总线 | emit 兼持久化 |
| `lib.rs` | 公共 API + 巨型集成测试 | 测试与生产混在 crate root |

---

## 三、改进清单（按主题）

每项含：问题、位置、方向、风险、优先级（P0/P1/P2）、状态。

### A. 模块边界与文件体量

#### A1. 拆分 `run_react_loop` 巨函数 `[完成]` · P0

- **问题**：`react.rs` ≈L844–2321 单函数混杂 pause 等待、inject、inbox、compact、infer、stream、empty/cut-off 重试、web-search、parse/repair、final/ask pause、并行工具、failure nudge、budget。
- **方向**：机械拆出（行为不变）：
  - `loop.rs` — 薄驱动：`prepare → stream → handle → tools → inject → continue|stop`
  - `stream_step.rs` — LLM 流式 + StreamForwarder
  - `tool_batch.rs` — 并行/串行工具批
  - `inject.rs` — steering/supplement/action 注入
  - `snapshot_io.rs` — snapshot / branch / exit 写盘
  - `retries.rs` — empty / cut-off 策略调用点
- **风险**：取消/快照不变量多；先拆模块 + 现有集成测试全绿，再改行为。
- **验收**：`cargo test -p haven-agent` 全过；`run_react_loop` 本体可读（目标 &lt;300 行驱动逻辑）。

#### A2. `ReActEngine` 去上帝化 `[进行中]` · P0

- **问题**：Messaging、token cache、snapshot 节流、tool-def cache、usage、msg-id mint、cut-off 启发式、schema repair 全挂在一个类型上（`react.rs` `ReActEngine`）。
- **方向**：Engine 变门面；侧车服务：`MessagingPoller`、`SnapshotStore`、`UsageTracker`、`MsgIdRegistry`、`LoopHooks`。
- **风险**：调用点多；先引入类型再挪字段。
- **验收**：Engine 字段只剩协作依赖；侧车可单测。

#### A3. 拆分 `SessionExecutor` `[待办]` · P1

- **问题**：`session.rs` ≈2.6k：FIFO dispatcher、状态机、三队列、confirm、partials、DB status、`execute_step` 同文件。
- **方向**：`SessionDispatcher` / `SessionQueues` / `ToolRunner`（含 confirm）+ 薄 `SessionExecutor` 门面。
- **风险**：锁序与 permit 释放易破。
- **验收**：队列与 dispatcher 可独立单测；permit 在 pause/error/cancel 路径仍正确释放。

#### A4. 继续拆 `AgentLayer` `[待办]` · P1

- **问题**：`layer.rs` 同时做 ingress、resume 恢复、title、peer spawn、media。
- **方向**：仿 `rollback.rs` 已拆模式：`ingress.rs`、`resume.rs`；Layer 只接线。
- **风险**：`process_input` 分支多。
- **验收**：`process_input` 路由表可一眼读完。

#### A5. `lib.rs` 瘦身 `[待办]` · P2

- **问题**：crate root 混公共 API、`sanitize_canonical`、数千行集成测试。
- **方向**：`canonical.rs` 承载 sanitize/repair；集成测试迁 `crates/agent/tests/` 或按主题 `#[cfg(test)]` 模块。
- **风险**：低。
- **验收**：`lib.rs` 以 `mod` + re-export 为主。

---

### B. 状态权威（transcript）

#### B1. 收敛多权威源 `[待办]` · P0（长期，分阶段）

- **问题**：模型上下文 `canonical`、`history: Vec<ReActStep>`、DB `messages`、`session_steps` 四处手写对齐；ID 规范（`msg-*`/`step-*` 共用）加剧复杂度。
- **方向（分阶段）**：
  1. 短期：规定 **canonical 为 LLM 唯一权威**；history 只作派生/调试，禁止独立业务分支依赖 history 语义。
  2. 中期：引入 append-only `TranscriptEvent`（Thought / ToolCall / ToolResult / UserInject / CompactSummary），投影到 canonical 与 UI。
  3. 长期：snapshot = transcript cursor，而非并行拷贝两份数组。
- **风险**：高；受 `AGENTS.md` ID 规范与前端气泡关联约束。禁止一次大改。
- **验收**：新代码路径不再「改 canonical 又改 history 两套逻辑」；resume 只从一条投影重建。

#### B2. `ReActStep` 模型不适配并行工具 `[待办]` · P1

- **问题**：一步多工具 → 多条 `ReActStep`；branch / restore_tools 需小心遍历（`types.rs` + `react.rs` 工具批）。
- **方向**：事件日志替代 step 三元组；UI 卡片按 `tool_call_id` / `step-*` 关联。
- **风险**：snapshot serde 与 rollback。
- **验收**：并行工具一轮对应一组事件，无「假 step_number 膨胀」。

#### B3. 注入前缀字符串 → 结构化来源 `[待办]` · P1

- **问题**：canonical 用 `"Steering:"` / `"Additional context…"` / `"Answer…"` 前缀；DB 存原文；靠 `saved_at` 恢复（`inject_pending_context` / `push_user_context`）。
- **方向**：消息元数据 `source: steering | follow_up | answer | action_result`；对 LLM 的渲染层再加前缀。
- **风险**：provider 可见格式变化需回归。
- **验收**：resume/去重只按 `message_id`/`saved_at`，从不比字符串内容。

#### B4. 统一 resume 投影路径 `[待办]` · P1

- **问题**：有 snapshot vs `rebuild_tool_chain_from_steps`（合成 `resumed_{id}`）两套世界（`layer.rs` `run_session_from_id`）。
- **方向**：缺失 snapshot 时用**同一** projector；或明确硬失败 + 用户可见提示。禁止静默分叉语义。
- **风险**：崩溃恢复 UX。
- **验收**：两种入口产出的 canonical tool 链形状一致（或明确不可恢复）。

---

### C. 循环控制流

#### C1. 暂停外置：去掉循环内 `status_rx` 挂起 `[完成]` · P0

- **问题**：`run_react_loop` ≈L908–983 在 `Paused`/`PausedAwaitingAnswer` 时 `select!` 等待；任务不退出，与外层 dispatcher 形成双调度。
- **方向**：pause 时写 snapshot、设状态、**return**；仅 dispatcher 在 `Pending` 时再次 `run`。对齐 PI 的 `prompt` / `continue`。
- **风险**：须保持「同 session 不双 claim」（`try_claim_pending`）；resume 延迟可能多一次 permit 获取。
- **验收**：pause 后无残留 Running 任务；supplement/answer 只经 dispatcher 唤醒；现有 ask/resume 测试全绿。

#### C2. 显式 `LoopExit` `[完成]` · P1

- **问题**：大量 `return Ok(())` 表示 cancel / pause / session 消失 / 完成，语义靠注释。
- **方向**：

```rust
enum LoopExit {
    Paused { reason: PauseReason }, // TurnEnd | Ask | Budget
    Cancelled,
    Completed,
    Error(String),
}
```

宿主映射到 `SessionStatus` / 事件。  
- **风险**：UI 假设「回答后 = Paused」。
- **验收**：循环出口无一裸 `Ok(())` 歧义；测试断言 `LoopExit` 变体。

#### C3. Ask 路径去掉 steering→answer 特判 `[完成]` · P1

- **问题**：工具批 ask 处理里把 mid-batch steering 转成 answer（≈L2203–2288），TOCTOU 与附件边界脆弱。
- **方向**：配合 C1：ask → 退出 run；用户回复以 typed follow-up（`reply_to`）再入队。单一收件箱。
- **风险**：后台任务 auto-wake 与 `PausedAwaitingAnswer` 门闩。
- **验收**：ask 期间用户输入、带附件、并发 action 完成，行为有单测覆盖且无「转队列」特判。

#### C4. 取消出口去重 `[待办]` · P2

- **问题**：`save_exit_snapshot` + `return` 在循环内重复 ≈10 处。
- **方向**：`exit_cancelled(...)` 或 cancel guard；与 `StepCallOutcome` 风格统一。
- **风险**：低（机械）。
- **验收**：取消路径只有一处写 snapshot 的实现。

#### C5. Ask 状态用显式标记，禁 JSON 子串扫描 `[完成]` · P1

- **问题**：`canonical_has_pending_ask` / `extract_pending_ask_question` 扫 `\"ask\":true`（≈L2520+）；压缩后可能失效。
- **方向**：snapshot / 运行时 flag `awaiting_answer: Option<AskPending>`；ask 工具成功时设置。
- **风险**：resume 必须恢复 flag；DB status 今日把 `PausedAwaitingAnswer` 塌缩为 `"paused"`（见 F2）。
- **验收**：压缩后仍能正确识别待答；无字符串启发式。

#### C6. `final_answer` 与「无 tool call」双轨收敛 `[待办]` · P2

- **问题**：空 actions → `pause_turn`；`is_final` → 另一套 pause；budget 又一套。
- **方向**：主路径「无 tool calls = turn end」；`final_answer` 可保留为显式 UX/兼容，共用同一 `PauseReason::TurnEnd`。
- **风险**：模型习惯依赖 `final_answer` tool。
- **验收**：两条路径进入同一 pause 实现；无重复 persist/emit 代码。

---

### D. 队列语义

#### D1. 收敛为 steering + followUp `[完成]` · P0

- **问题**：Running→steering、Paused→supplement、ask→answer、action_completions 分离；心智模型 ≠ PI。
- **方向**：
  - **steering**：run 进行中、下一 LLM 前注入（可配 `all` / `one_at_a_time`）
  - **follow_up**：本会将停 / 已 pause 后注入；answer = follow_up + `reply_to`
  - **action_results**：系统/工具注入，不进用户队列（独立 `system_inject` 或 hook）
- **位置**：`session.rs` 队列 API；`layer.rs` `process_input` 路由。
- **风险**：resume 恢复、auto-wake、现有测试大量依赖旧名。
- **验收**：路由表文档化；旧 API 可先 type alias 过渡一版。

#### D2. 队列持久化与 RAM 缓存关系理清 `[待办]` · P1

- **问题**：队列在内存；正确性依赖消息落库 + `saved_at` + undelivered 扫描（`run_session_resumed`）。
- **方向**：提交时即写入 transcript/消息（已部分如此）；RAM 队列仅缓存；resume 按 `message_id` 幂等重放。
- **风险**：双注入。
- **验收**：崩溃后未送达消息只注入一次（按 id）。

#### D3. 诚实命名「steering」能力 `[待办]` · P2

- **问题**：注释暗示可打断；实际仅在 step 边界注入，工具批默认跑完（除非 cancel）。
- **方向**：文档写清；可选策略 `CancelToolsOnSteer`（产品决定后再做）。
- **风险**：UX 预期。
- **验收**：`docs/` + 代码注释一致。

---

### E. 流式与工具执行

#### E1. 抽出流式管线 `[待办]` · P1

- **问题**：`StreamForwarder`、`stream_llm_step`、`stream_retry_step`、`call_step_llm`、`partial` 与循环体耦合。
- **方向**：`StreamSession::run(...) -> StepResponse`；循环只消费结果；partial 经 hook 写盘。
- **风险**：重试必须复用 msg-id（已有约定）。
- **验收**：流式/卡死 watchdog/partial 有针对模块测试。

#### E2. 抽出 `execute_tool_batch` `[完成]` · P0

- **问题**：工具批 ≈L1861–2288 内联 FuturesUnordered、cancel 修复、ask/notify、history、failure nudge、ask pause。
- **方向**：返回 `ToolBatchResult { results, asks, notifies, failures }`；循环只决定是否 pause。配合 `before_tool` / `after_tool` hooks。
- **风险**：cancel interrupt 语义必须字节级保持。
- **验收**：工具批单测覆盖并行、取消、ask、失败 nudge。

#### E3. Confirm 改为可暂停，而非工具内阻塞 `[待办]` · P1

- **问题**：`await_confirmation` 最长 120s 堵在工具 future 内；并行批被一个 gated 工具拖死（`session.rs`）。
- **方向**：`before_tool` → `NeedConfirm` → 会话 pause（类 ask），用户确认后 `continue` 再执行该工具。
- **风险**：并行工具与对话框 UX 大改。
- **验收**：一 gated + 一普通并行时，普通可完成或整体有序 pause；无 120s 隐式挂死。

#### E4. `execute_step` 禁止强制改 Running `[待办]` · P2

- **问题**：Pending/Paused 时 warn 并强制 Running，掩盖调度 bug。
- **方向**：严格：仅 Running 可执行工具；修调用方。
- **风险**：暴露潜伏 bug。
- **验收**：非法状态调用返回错误而非改状态。

#### E5. 参数补全/校验移到工具边界 `[待办]` · P2

- **问题**：`supplement_missing_required_fields` 与 schema fallback 在循环内。
- **方向**：`ToolRunner` / `before_tool` 校验与补全。
- **风险**：低。
- **验收**：循环内无 schema 修补代码。

---

### F. Session / Dispatcher / 状态持久化

#### F1. 单一调度器（与 C1 绑定）`[完成]` · P0

- **问题**：外层 FIFO + 内层 status watch 同任务 resume；permit 何时释放靠约定。
- **方向**：handler 在 pause **必须退出**；只有 dispatcher 启动 run。
- **风险**：resume 延迟。
- **验收**：无「Paused 仍占用 handler」；并发 session 压测 permit 不泄漏。

#### F2. 持久化 `PausedAwaitingAnswer` `[完成]` · P1

- **问题**：`SessionStatus::as_str` 把 awaiting 塌成 `"paused"`；重启丢 ask 门闩，靠 canonical 启发式补。
- **方向**：DB/wire 区分状态，或 snapshot 内 `awaiting_answer` 字段（与 C5 一致）。
- **风险**：前端/旧库兼容（新状态需 migration bump `SCHEMA_VERSION`；缺必需列的远古库仍要求删库重建）。
- **验收**：杀进程重启后仍阻止 bg auto-wake，直到用户回答。

#### F3. Snapshot 策略事件化 `[待办]` · P1

- **问题**：节流 mid-run snapshot 与 pause 必写混在 Engine（`last_snapshot_step` 等）。
- **方向**：`on_step_boundary` / `on_pause` / `on_cancel` 持久化 hook。
- **风险**：中。
- **验收**：策略可单测「第 N 步是否写盘」。

#### F4. BranchPoint 降成本 `[待办]` · P2

- **问题**：每个分支克隆完整 canonical/history。
- **方向**：存 transcript 索引 / 外部 blob；或 COW。
- **风险**：rollback 正确性。
- **验收**：长对话多次工具前分支，内存不线性翻倍（或可配置上限）。

#### F5. Rollback 等待循环退出更可靠 `[待办]` · P1

- **问题**：`rollback.rs` 轮询 running actions 最多 ≈5s，晚到工具写可能竞态。
- **方向**：join run handle 或 generation token（与 PartialStore gen 对齐）。
- **风险**：中。
- **验收**：rollback 与在途工具结束顺序确定；无超时侥幸。

---

### G. Hook 化领域逻辑

#### G1. 引入 `LoopHooks` `[完成]` · P0

- **问题**：无扩展缝；compact / infer / inbox / usage / title 全 splice 进循环。
- **方向**：

```rust
#[async_trait]
trait LoopHooks: Send + Sync {
    async fn before_step(&self, ctx: &mut StepCtx, canonical: &mut Vec<CanonicalMessage>);
    async fn after_llm(&self, ctx: &StepCtx, response: &StepResponse) -> AfterLlmAction;
    async fn before_tool(&self, ctx: &ToolCtx) -> BeforeToolAction;
    async fn after_tool(&self, ctx: &ToolCtx, result: &ToolResult) -> AfterToolAction;
    async fn on_pause(&self, ctx: &StepCtx, reason: PauseReason);
    async fn on_error(&self, ctx: &StepCtx, err: &str);
}
```

默认 hooks：compact、infer、inbox、usage emit、snapshot。测试用 noop / 录制 hook。  
- **风险**：hook 顺序契约需文档化。
- **验收**：禁用 inbox/infer 的单元测试不启动相关依赖；主循环源文件不再直接调用 `maybe_compact` / `maybe_poll_inbox`。

#### G2. Compaction / fact infer / inbox 迁出 prologue `[完成]` · P0

- **问题**：每步 prologue 可能 LLM 压缩、抽事实、poll inbox（≈L999–1025）。
- **方向**：全部经 `before_step` / `after_step`；核心循环保持纯编排。热路径抽取/维护解耦已先落地（`infer_session` vs 调度器 `run_memory_maintenance`）；与 `docs/memory-architecture.md` §三协作计划对齐——短期 S4（记忆 patch 挂 pause/resume）、长期 L3（抽取 outbox）。本项仍要把 compact/infer/inbox 迁出 prologue。
- **风险**：与 `sanitize_canonical` 顺序。
- **验收**：顺序固定为 inject → hooks.before_step → sanitize → LLM；有注释契约；禁用 infer 的单测不触达 SQLite maintenance。

#### G3. 响应策略（empty / cut-off）外置 `[待办]` · P1

- **问题**：中英截断短语表、`is_suspect_final`、`finish_reason` 逻辑在核心循环（≈L1164–1397, L2567+）。
- **方向**：`ResponsePolicy::classify -> Accept | Retry { nudge } | Error`；`after_llm` 调用。
- **风险**：行为敏感。
- **验收**：策略单测不启 loop；循环内无短语字面量。

#### G4. Web-search 特判移出循环 `[待办]` · P2

- **问题**：provider 服务端搜索续跑写在 ReAct 分支（≈L1477–1542）。
- **方向**：流式层产出「合成 tool result」或 `ContinueWithoutTools`。
- **风险**：DeepSeek 等 provider 特异。
- **验收**：循环无 `web_search` 分支。

#### G5. Failure nudge 勿污染用户 transcript `[待办]` · P2

- **问题**：合成 User 文本注入 canonical（`build_failure_nudge`）。
- **方向**：ephemeral system/developer，或只附在 tool result；持久化可剥离。
- **风险**：模型行为变化。
- **验收**：DB messages 无「失败催促」伪用户句；或标记 `ephemeral`。

#### G6. Inference 与循环生命周期解耦 `[待办]` · P2

- **问题**：循环持 `infer: &dyn Fn`；失败静默；测 loop 易拖进真实抽取。
- **方向**：仅 hook；专用 worker + 已有 semaphore。
- **风险**：低。
- **验收**：loop 单测不构造 `InferenceEngine`。

#### G7. 系统提示与工具列表双路径整理 `[待办]` · P2

- **问题**：`SystemPromptBuilder` 嵌入工具索引；API 另发 `ToolDefinition`；skill/MCP 热更新 defs 后 prompt 可能仍是开场快照。
- **方向**：工具详情以 API schema 为准；prompt 只保留短索引或在 skill load 时经 hook 刷新 section。
- **风险**：token / 行为。
- **验收**：`load_skill` 后下一步 LLM 请求工具列表与 prompt 描述一致。

---

### H. 事件与持久化

#### H1. 统一 `apply(TranscriptEvent)` `[待办]` · P1

- **问题**：`emit_thought_from` 等「emit」内写 DB（`event.rs`）；Action 先 `begin_action_step` 再 emit——散落且失败模式不一。
- **方向**：单一 `apply`：持久化（保持「行先于卡」）→ 再投影事件。禁止各处手写双写。
- **风险**：前端时序。
- **验收**：所有 thought/action/observation 只经 `apply`；部分失败有明确错误路径。

#### H2. BufferedEmitter 溢出与 snap 和解绑测试 `[待办]` · P2

- **问题**：块事件可丢，依赖最终 snap 和解。
- **方向**：保留；强制 stream 管线测试「丢块 + snap 仍一致」。
- **风险**：低。
- **验收**：相关测试存在且稳定。

---

### I. 测试与可观测性

#### I1. 薄循环可单测 `[完成]`（模块启发式） · P0

- **问题**：绝大多数测试在 `lib.rs` 全栈（DB + executor + mock LLM）。
- **方向**：拆分后 `run_turn` + mock `Stream`/`Tools`/`Hooks`；集成测试留下少数黄金路径。
- **风险**：前置依赖 A1/G1。
- **验收**：核心状态机测试 &lt;1s 且不打开 SQLite（或只用 in-memory 且不经 Layer）。

#### I2. 阶段 span `[待办]` · P2

- **问题**：tracing 散落，缺与薄循环对齐的 phase。
- **方向**：`inject` / `compact` / `llm` / `tools` / `persist` / `pause` spans（session 已有外层 span）。
- **风险**：低。
- **验收**：一次 turn 的 trace 树可辨阶段。

#### I3. Msg-id / Identity 集中服务 `[待办]` · P1

- **问题**：`ensure_msg_id` / `block_msg_id` / action `step-*` mint 分散，易漏（`AGENTS.md` ID 规范）。
- **方向**：`IdentityMap` 供 stream、persist、events 共用。
- **风险**：前端合并回归。
- **验收**：同一 thought 流式气泡 id 与落库 id 始终一致；有单测。

---

### J. 其它产品语义澄清

#### J1. 文档化 per-run `max_steps` 再预算 `[待办]` · P2

- **问题**：`effective_max` 每次 resume 再给满额（≈L855–862），会话可很长。
- **方向**：写入 `RunBudget` 到 snapshot；产品决定是否加 session 生命周期上限。
- **风险**：产品决策。
- **验收**：行为有文档 + 可选配置。

#### J2. Debug 断言 `sanitize_canonical` 为 no-op `[待办]` · P2

- **问题**：发送前修理掩盖上游 interrupt/compaction bug。
- **方向**：保留闸门；`debug_assert` 或 metrics 计数「修理发生次数」。
- **风险**：低。
- **验收**：正常路径 metrics ≈0；集成测试可注入损坏链验证闸门。

#### J3. 补齐缺失的 PI 对照文档引用 `[完成]` · P2

- **问题**：曾引用不存在的 `docs/Pi Coding Agent架构.md` / 已删除的 `performance-review.md`。
- **方向**：对照表以本文第一节 + `architecture.md` 为准；工作流不再依赖已删文档。
- **风险**：无。
- **验收**：死链清除。

---

## 四、落地分期（建议执行顺序）

行为冻结优先：每一期结束必须 `cargo test -p haven-agent` + 相关 UI 冒烟。

| 期 | 主题 | 包含项 | 优先级 |
|---|---|---|---|
| **0** | 文档与基线 | 本文；修死链（J3）；列出现有黄金测试清单 | — |
| **1** | 机械拆分 | A1, A2（骨架）, E2, I1（先能测拆后模块） | P0 |
| **2** | 暂停外置 + 单调度 | C1, F1, C2（可同做） | P0 |
| **3** | Hook 化 | G1, G2, 侧车迁 compact/infer/inbox | P0 |
| **4** | 队列收敛 | D1, C3, C5, F2 | P0–P1 |
| **5** | 工具与确认 | E3, E1, G3 | P1 |
| **6** | Transcript 收敛 | B1 阶段1–2, B3, H1, I3 | P1 |
| **7** | 清理与加固 | A3–A5, B4, F3–F5, G4–G7, C4, C6, D2–D3, E4–E5, J1–J2, I2 | P1–P2 |

每期建议工作流：

1. 基线：记下相关测试名与手动场景（ask / steering / resume / rollback / confirm）
2. 独立分支改动
3. `cargo test -p haven-agent`；必要时 `/test-ui --run`
4. 对照场景手测
5. 在本文件将对应项标为 `[完成]` 并记提交哈希

---

## 五、明确不做（或延后）

| 项 | 原因 |
|---|---|
| 把 Haven 循环重写成 TypeScript / 直接依赖 pi-agent-core | 技术栈与产品边界不同（Tauri/Rust、SQLite、语音） |
| 去掉 SQLite snapshot / branch rollback | 桌面助手崩溃恢复刚需 |
| 去掉 confirm / 风险门闩 | 安全产品要求 |
| 为追行数而删空/截断重试 | 中文模型截断实测有用；应外置为 policy，而非删除 |
| 一次合并 canonical+DB 大爆炸重构 | 必须分期（B1） |

---

## 六、参考

- 上游：https://github.com/earendil-works/pi/tree/main/packages/agent （`agent.ts` / `agent-loop.ts` / `types.ts`）
- 本仓库：`docs/architecture.md`、`AGENTS.md`（ID / resume 规范）、`crates/agent/src/react/`
- 相关：`docs/memory-architecture.md`（记忆 / Facts 现状、引擎 backlog、§三 Facts/Episodes/对话历史协作计划；S4/L3 与 G2 互补）

---

## 变更记录

| 日期 | 内容 |
|---|---|
| 2026-08-20 | 初版：对照 PI Agent Core 列出 A–J 共 40+ 改进点与分期落地顺序 |
| 2026-08-20 | **Phase 1**：`react.rs` → `react/`（`mod`/`loop`/`stream_step`/`tool_batch`/`inject`/`snapshot_io`/`retries`）；抽出 `execute_tool_batch`→`ToolBatchOutcome`；retries/tool_batch 模块单测。`cargo test -p haven-agent --lib` 221 全绿。`run_react_loop` 仍约 950 行（pause 等待/empty·cut-off 重试仍内联，Phase 2/G3 再外置），未达 &lt;300 驱动目标。A2 侧车字段迁出留待后续；骨架 ZST 已去掉避免死代码。 |
| 2026-08-20 | **Phase 2**：C1 去掉循环内 `status_rx` 挂起（Paused* 写 snapshot 后 `return`）；C2 引入 `LoopExit`/`PauseReason`，`run_react_loop`→`Result<LoopExit>`，`ToolBatchOutcome::Done(LoopExit)`；F1 注释/claim 语义改为 exit-based 单调度。`cargo test -p haven-agent --lib` 全绿。 |
| 2026-08-20 | **Phase 3**：G1 `LoopHooks`（`hooks.rs`：`DefaultHooks`/`NoopHooks`）；G2 prologue 改为 inject → `before_step` → sanitize → LLM；pause infer 经 `on_pause`。主循环不再直接调 `maybe_compact`/`maybe_poll_inbox`。`cargo test -p haven-agent --lib` 223 全绿。 |
| 2026-08-20 | F2 风险表述改为迁移层现状；参考链去掉缺失的 `performance-review.md`；G2 交叉链到 memory backlog |
| 2026-08-20 | G2 / 参考链对齐 `memory-architecture.md` §三协作短期 S4 / 长期 L3 |
| 2026-08-20 | **Phase 4**：D1 队列收敛为 steering + follow_up（`FollowUp` alias、`follow_up_queue`、旧 `add_supplement*` 保留）；C3 去掉 steering→answer 转队列，改为原地 `mark_user_queues_as_answer`；C5 snapshot/`SessionExecutor` 显式 `awaiting_answer: AskPending`；F2 DB/wire `paused_awaiting_answer`（SCHEMA_VERSION=3）+ UI `isPausedStatus`。 |
