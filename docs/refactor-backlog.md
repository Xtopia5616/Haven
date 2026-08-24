# Haven 重构 Backlog（Memory + ReAct）

> 状态：`[待办]` / `[可选]` / `[完成归档]`  
> 原则：**可大改、不向下兼容**（记忆与 ReAct 两边可一起重构；旧 dual-array snapshot / 远古 schema 可删库重建）。  
> 更新日期：2026-08-24  
> 取代：`docs/memory-architecture.md`、`docs/react-architecture-improvements.md`（已删除，内容并入本文）。

---

## 0. 范围与读法

本文是 **剩余重构清单 + 必要现状锚点**，不是历史变更日记。

| 段 | 内容 |
|---|---|
| §1 | 当前实现锚点（删旧文档后仍能定位代码） |
| §2 | 已完成归档（不再当待办推进） |
| §3 | **全部剩余项**（含原「明确不做」） |
| §4 | 建议分期 |
| §5 | 验证基线 |

编号前缀：

- `M-*` — 记忆 / Facts / Episodes / 协作
- `R-*` — ReAct / session / transcript / hooks
- `X-*` — 跨切面（原「不做」里跨两边的大爆炸项）

---

## 1. 当前实现锚点

### 1.1 记忆通道（`haven.db`）

| 通道 | 表 | 要点 |
|---|---|---|
| 长期事实 | `facts` | SPO 三元组；confidence / durability / tags / source / `source_ref` |
| 情景 | `memory_episodes` | compaction 摘要 + `topics`/`entities`；与压缩气泡共享 `msg-*` |
| 向量 | `memory_embeddings` | `fact`/`episode`；查询 **必** `WHERE model=?` |
| 全文 | `facts_fts` / `episodes_fts` | FTS5 `trigram`；仅短 term（&lt;3）LIKE 回退 |
| 游标 | `kv_store` | `fact_extraction.*` 等 |

Schema：`haven_memory::schema::init_schema`，`PRAGMA user_version` + `MIGRATIONS`（以代码 `SCHEMA_VERSION` 为准）。缺必需列的远古库拒绝打开（删库重建）。

**读写契约（已落地）**

- **canonical** = 本会话 LLM 真源（含压缩摘要气泡）
- **facts / episodes** = **跨会话**检索；同会话不进 Past excerpts（`exclude_session_id`）
- **DB messages** = 持久化 + 抽取源；Additional context 不与首条 user 重复
- 记忆注入：开场写入 `canonical[0]` 的 `--- MEMORY ---` fence；**resume 入环前** 全量重建 system（X2 / `rebuild_canonical_system`）；步间 dirty 仅 patch MEMORY fence（M2）；`infer_session` **不**改 system
- 抽取：ReAct 只 `enqueue_infer(session_id)` → outbox worker；维护走调度器（启动 + ~6h）
- Prompt：`build_memory_sections` + 字符预算；`get_facts_limited` / `search_facts_any`

入口：`crates/memory/`、`crates/agent/src/{inference,prompt,layer,compactor}.rs`、`crates/tools/src/builtin/memory.rs`（工具名 `memory`，含 recall）

### 1.2 ReAct（对照 PI：薄循环 + 厚 hook；产品能力留宿主）

```
User/STT → AgentLayer (ingress/resume)
        → SessionExecutor (dispatcher/queues/status/tool_runner)
        → ReActEngine::run_react_loop (react/: loop/stream/tools/inject/hooks/…)
        → AgentEvent → UI
```

**已定权威（Phase 8）**

- Snapshot 唯一权威：`ReActSnapshot.events: Vec<TranscriptRecord>`
- Runtime `canonical` = 投影缓存；`BranchPoint` 只存 `event_cursor`
- 队列：steering + follow_up（answer = follow_up + `reply_to`）；action_results 独立
- Pause = 写 snapshot 后 **退出 run**；仅 dispatcher 再 claim
- Ask / Confirm：显式 `awaiting_answer` / `PausedAwaitingConfirm`（DB 区分状态）
- 工具 schema 权威 = 每步 API `tools[]`；prompt 短索引 **按 run 冻结**（G7），resume 全量重建（X2）
- 步数：`max_steps` = per-run；可选 `session_max_steps` 截断绝对步号

入口：`crates/agent/src/react/`、`session/`、`ingress.rs`、`resume.rs`、`canonical.rs`、`event.rs`  
规范：`AGENTS.md`（ID / resume `saved_at` / schema）

**保留、不塞回薄循环**：SQLite resume、confirm、多 session、ask、语音 pause、`PartialStore` fencing、`sanitize_canonical`、`saved_at` resume（禁止内容去重）。

---

## 2. 已完成归档（摘要）

以下均已落地，**不要再当 backlog 开单**。细节以代码与 git 历史为准。

### 2.1 Memory — 完成

| 批次 | 项 |
|---|---|
| 早期六步 | 向量召回融合、迁移层、trigram、谓词规范化、抽取调度、移除规则兜底 |
| P0 | CJK `memory_recall_terms`；抽取/维护解耦；embed backlog 有界 |
| P1 | 域限定向量检索 + scan cap；FTS OR+LIMIT；维护 SQL 下沉；抽取 outbox；记忆段字符预算 |
| P2 | `source_ref` 清孤儿 + `source_snippet`；episodes 结构化+FTS；谓词别名；facts 多 subject；embedding 按 model 过滤；FTS→LIKE 收紧 |
| 协作 S1–S4 | 权威契约、同会话去重、resume MEMORY patch、与 G2 衔接 |
| 协作 L1–L4 / L6 | 压缩共享 `msg-*`；source_ref 消费；outbox；预算/查询形态；episodes FTS |
| P0/P1 (2026-08-22) | M1 确认轮次配对抽取；M2 步间节流 MEMORY patch；M3 摘要轻量抽 facts |
| P2 (2026-08-22) | M4 抽取视野含有界 assistant/tool；M5 LSH ANN（schema v6）；M6 维护期 LLM 谓词合并 |

### 2.2 ReAct — 完成（Phase 0–8）

| 期 | 主题 |
|---|---|
| 1 | `react/` 机械拆分；`execute_tool_batch` |
| 2 | 暂停外置；`LoopExit`；单调度 |
| 3 | `LoopHooks`；compact/infer/inbox 出 prologue |
| 4 | steering+follow_up；ask 无转队列；`awaiting_answer`；DB `paused_awaiting_answer` |
| 5 | `ResponsePolicy`；`StreamSession`；confirm→pause |
| 6 / 6.1 | `TranscriptEvent`/`apply`；`InjectSource`；`IdentityMap`；CompactSummary / Action·Observation 入 apply |
| 7 | `session/` 拆分；ingress/resume；统一 projector；exit/turn_end；队列契约；SnapshotStore；spans；sanitize 计数；G4–G7 等 |
| 8 | events 权威；`ReActRound`；wire-only inject 前缀；BP cursor；BufferedEmitter 测试；`session_max_steps`；集成测迁出 `lib.rs`；sidecars |
| P0 (2026-08-22) | R2 删除 `await_confirmation`，调度确认非阻塞；X7 删除 Steps so far / `{history}` |

基线曾绿：`cargo test -p haven-agent --lib`（2026-08-22 P2：281）；`cargo test -p haven-memory --lib`（202）。

---

## 3. 全部剩余项

> 含原文档「明确不做 / 延后 / 按需」。兼容性不再作为否决理由；产品与安全边界仍标注。

### 3.1 Memory — 显式与残留

#### M1. 抽取窗口含「用户确认」轮次 `[完成归档]` · 原 L5

- **落地**：`build_extraction_window` 按 user 游标增量组装；新 user 前若紧邻非压缩 assistant 则成对纳入；transcript 为 `[N] role: …`；`source_ref` 优先落 user 行；抽取 prompt 允许短确认对照上一问句。
- **位置**：`inference.rs` / `common::prompts::FACT_EXTRACTION_SYSTEM_PROMPT`

#### M2. 步间 / worker 回调刷新 MEMORY fence `[完成归档]` · 原 §3.2-1 残留

- **落地**：fact 写入成功 → `mark_memory_dirty`；下一 `before_step` 经 `take_memory_dirty_throttled` 调用 `SystemPromptBuilder::patch_canonical_memory_fence`（仅 MEMORY fence）；**禁止**全量重建 tools/skills。
- **位置**：`inference.rs` / `react/hooks.rs` / `prompt.rs` / `layer.rs`

#### M3. Compaction 摘要轻量抽 facts `[完成归档]` · 原 L1 未做支线

- **落地**：`persist_compaction_summary` 成功后 `enqueue_summary_extract`；独立游标 `fact_extraction_episode.{session}`；共享时间节流；不推进 user 游标；写入后置 dirty（接 M2）。
- **位置**：`react/snapshot_io.rs` / `inference.rs` / `ReActEngine::with_inference`

#### M4. 抽取视野对齐 canonical（适度） `[完成归档]` · 原 §3.2-6

- **落地**：每新 user 纳入同轮最多 2 条非压缩 assistant（跳过 reasoning）+ 最多 3 条 tool 观察（`role=tool` 优先，否则从 `session_steps` 合成 `tool(name): …`，单条截断 300 字）；游标仍只推进 user id；prompt 声明 tool/assistant 仅作 grounding。
- **位置**：`inference.rs` / `common::prompts::FACT_EXTRACTION_SYSTEM_PROMPT`

#### M5. 万级向量索引 `[完成归档]` · 原 P1-4 尾巴

- **落地**：纯 Rust 随机投影 LSH（无 sqlite-vec / 无新 native 依赖）；`embedding_lsh` 侧表 + schema v6；分区 ≥ `ANN_ACTIVATE_MIN`(4096) 时 Hamming-1 probe + 精确 cosine 重排；以下仍 newest scan cap；换模 fail-closed；维护期 `rebuild_embedding_lsh`。
- **位置**：`embeddings.rs` / `schema.rs`

#### M6. 谓词冲突 LLM 辅助合并 `[完成归档]` · 原 P2-11 方向支线

- **落地**：维护期 BalancedModel 提出 merge；仅当静态别名已映射或 confidence≥0.85 且目标为已知 canonical；禁止 likes↔dislikes；LLM 在 DB 锁外，`rewrite_predicate` + `dedup_facts` 落库。
- **位置**：`inference.rs` / `facts.rs` / `PREDICATE_MERGE_SYSTEM_PROMPT`

---

### 3.2 ReAct — 残留与产品未决

#### R1. `CancelToolsOnSteer` `[完成归档]` · 原 D3 产品支线

- **落地（P3 / 产品默认）**：保持「steer 不打断在途工具批」；`queues::add_steering` 文档诚实写明无 `CancelToolsOnSteer`。可选旋钮仍未实现。
- **位置**：`session/queues.rs`

#### R2. 清除调度路径遗留 `await_confirmation` `[完成归档]` · Phase 5 尾巴

- **落地**：删除 `await_confirmation` / `confirm_waits`；`execute_gated` 缺 `pre_confirmed` 时 fail-closed；`ScheduleMode::Tool` 经 `request_scheduled_confirm` 非阻塞排队，`resolve_confirmation` / `SCHEDULED_CONFIRM_TIMEOUT` 后续执行或跳过。
- **位置**：`session/{mod,tool_runner}.rs` / `layer.rs`

#### R3. Skill/MCP 加载后刷新 prompt 工具短索引 `[完成归档·已被 X2 重订]` · 原 G7 反向选择

- **落地（P3 / freeze+declare）**：开场短索引在 **run 内**冻结；`TOOL_USAGE_NOTES` 声明 API `tools[]` 为唯一 schema 权威；`load_skill` / `load_mcp` 仅在下一步 `tools[]` 可见，不 patch prompt。
- **X2 重订（2026-08-24）**：冻结范围从「整段 session」收窄为「当前 run」；resume 全量重建短索引（见 X2），步间仍不 patch。
- **位置**：`common::prompts::TOOL_USAGE_NOTES` / `prompt.rs` / `react/mod.rs` / `resume.rs`

#### R4. `run_budget` 写入 snapshot（可观测） `[完成归档]` · 原 J1 未落地字段

- **落地**：`ReActSnapshot.run_budget: Option<RunBudget>`（`start_step` / `effective_max` / `max_steps` / `session_max_steps`）；loop 开场写入，pause/mid-run snapshot 镜像；rollback 清掉。
- **位置**：`types.rs` / `react/{loop,snapshot_io,mod}.rs`

#### R5. 薄循环黄金单测加厚 `[完成归档]` · 原 I1 深化

- **落地**：`lifecycle` 窗口矩阵单测；集成：`rollback_while_ask_wait_clears_awaiting_answer_gate`、`rollback_mid_tool_batch_joins_and_restores`、`pause_snapshot_includes_run_budget`；既有 ask-after-retry / cut-off / mid-batch cancel 作基线。
- **位置**：`lifecycle.rs` / `integration_tests.rs`

#### R6. 分支 / 重试跨生命周期窗口硬化 `[完成归档]` · 2026-08-24

- **落地**：`lifecycle::{LifecycleWindow,LifecycleOp,decide}` 矩阵；rollback：凡 run slot 在握（含 claim→spawn / 直跑 `begin_direct_run`）→ cancel+`await_run_finished`；始终清 ask/confirm gate + snapshot `awaiting_*`；continue 在 unwind 中 `AwaitThenAllow`；直跑与 dispatcher 共用 run slot。
- **不可用窗口（诚实）**：continue 于 `Running`/`Pending`/`Completed`；`Completed`+`pause=false` branch（状态机禁 `Completed→Pending`）；steer 不 cancel 工具（R1）。
- **位置**：`lifecycle.rs` / `rollback.rs` / `session/dispatcher.rs` / `resume.rs`

---

### 3.3 原「明确不做」——现全部入册

> 下列原为否决项。现允许大改时记为 **可选史诗**；实施前仍要过产品/安全门，但**不再以兼容性否决**。

#### X1. 记忆大表 / 知识图谱 `[可选·史诗]` · 原 memory §3.5

- 把 `facts` + `memory_episodes` + `messages` 合成统一记忆存储或图谱。
- **代价**：schema、召回、UI、迁移全面重做。

#### X2. Resume 全量重建 system prompt `[完成归档]` · 原 memory §3.5

- **落地（2026-08-24）**：`run_session_resumed` 调用 `SystemPromptBuilder::rebuild_canonical_system`（tools/skills/MCP 短索引 + MEMORY + session）；保留 Additional context 行；清 `schema_cache` 以拾取新装技能/MCP。
- **G7 重订**：freeze-per-run（步间 `load_*` 仍只改 API `tools[]`；resume 刷新短索引）。M2 步间 dirty 仍仅 patch MEMORY fence。
- **位置**：`prompt.rs` / `resume.rs` / `common::prompts::TOOL_USAGE_NOTES`

#### X3. 恢复「按内容比对」resume 去重 `[可选·不推荐]` · 原 memory §3.5 / AGENTS.md

- 用字符串内容代替 `saved_at` + `message_id`。
- **说明**：与现行 ID/resume 规范直接冲突；仅当推翻 `AGENTS.md` 契约时考虑。**默认保持禁止。**

#### X4. 压缩出窗 DB 历史灌回 canonical `[可选·不推荐]` · 原 memory §3.5

- 「保险」把已压出窗口的整段 DB 历史再注入模型。
- **说明**：应用 facts/episodes + 压缩摘要；灌回会撑爆上下文。

#### X5. `source_ref` 矛盾引擎 `[可选]` · 原 memory §3.5 / L2

- 在 snippet 展示与 upsert demote 之上，做自动矛盾检测/仲裁。
- **依赖**：先有稳定引用与展示（已有）。

#### X6. 记忆独立 UI Tab `[可选·产品]` · 原 memory §3.5 / 项目约束

- 召回现为 prompt / 工具结果形态；独立 Tab 需改产品约束 `ui.agent_tool_display` 相关约定。

#### X7. 并行启用「Steps so far」与 canonical `[完成归档·已删除]` · 原 memory §3.2-3 / §3.5

- **落地**：删除 `ReActRound`→system prompt 注入、`{history}` 占位与 Steps so far 渲染；canonical 为唯一 LLM transcript 权威。`ReActRound` 投影与 Additional context 保留。

#### X8. 重写为 TypeScript / 依赖 pi-agent-core `[可选·不推荐]` · 原 react §五

- 技术栈与产品边界不同（Tauri/Rust、SQLite、语音）。对照价值已吸收进 Phase 1–8。

#### X9. 去掉 SQLite snapshot / branch rollback `[可选·不推荐]` · 原 react §五

- 桌面崩溃恢复刚需；去掉需另有等价持久化。

#### X10. 去掉 confirm / 风险门闩 `[可选·不推荐]` · 原 react §五

- 安全产品要求；可改交互（R2），不宜删除门闩本身。

#### X11. 删除 empty / cut-off 重试 `[可选·不推荐]` · 原 react §五

- 中文模型截断实测有用；已外置 `ResponsePolicy`。可调参，不宜为行数删除。

#### X12. DB messages 与 events 一次大合并 `[可选·史诗]` · 原 react §五尾巴

- Phase 8：snapshot = events；DB `messages`/`session_steps` 仍独立投影。
- **方向**：单一 append-only 日志同时服务 LLM / UI / 抽取；或 DB 只存 events blob + 物化视图。
- **风险**：高；与 ID 规范、前端气泡、抽取源强耦合。

#### X13. BranchPoint 外置 blob / 完整 transcript 索引 `[可选]` · 原 F4 延后支线

- Phase 8 已用 `event_cursor`（无 Vec 拷贝）。若 events 极大，可再外置冷存储 / 分页加载。

---

### 3.4 清单速查

| ID | 状态 | 域 | 一句话 |
|---|---|---|---|
| M1 | 完成 | Memory | 抽取含确认轮次 |
| M2 | 完成 | Memory | 步间/worker 刷新 MEMORY |
| M3 | 完成 | Memory | 摘要抽 facts |
| M4 | 完成 | Memory | 抽取视野对齐 canonical |
| M5 | 完成 | Memory | 万级 LSH ANN（≥4096） |
| M6 | 完成 | Memory | 维护期 LLM 谓词合并 |
| R1 | 完成 | ReAct | 文档默认：steer 不 cancel 工具 |
| R2 | 完成 | ReAct | 去掉遗留 await_confirmation |
| R3 | 完成·X2重订 | ReAct | freeze-per-run + prompt 声明（原永久冻结） |
| R4 | 完成 | ReAct | snapshot 显式 RunBudget |
| R5 | 完成 | ReAct | 薄循环/窗口矩阵单测加厚 |
| R6 | 完成 | ReAct | 分支/重试跨生命周期窗口硬化 |
| X1 | 可选·史诗 | 跨切 | 记忆大表/图谱 |
| X2 | 完成 | Memory | resume 全量重建 system；G7→freeze-per-run |
| X3 | 不推荐 | Resume | 内容比对去重 |
| X4 | 不推荐 | Context | 出窗历史灌回 canonical |
| X5 | 可选 | Memory | source_ref 矛盾引擎 |
| X6 | 可选·产品 | UI | 记忆独立 Tab |
| X7 | 完成 | Prompt | 已删 Steps so far 死路径 |
| X8 | 不推荐 | 栈 | TS / pi-agent-core |
| X9 | 不推荐 | 持久化 | 去掉 snapshot/rollback |
| X10 | 不推荐 | 安全 | 去掉 confirm |
| X11 | 不推荐 | 策略 | 删除截断重试 |
| X12 | 可选·史诗 | 跨切 | DB↔events 统一日志 |
| X13 | 可选 | ReAct | BP/events 冷存储 |

**计数**：待办 **0** · 可选 **7**（X 史诗/产品）· 不推荐 **6** · 完成归档本轮 **14**（M1–M6、R1–R6、X2、X7）· 史诗计入可选。

---

## 4. 建议分期（不顾兼容）

```
P0  契约清理                         ✅ 2026-08-22
    R2  清除 await_confirmation 遗留
    X7  删除「Steps so far」死路径（不做双通道）

P1  记忆协作加深                     ✅ 2026-08-22
    M1  确认轮次抽取
    M2  步间/worker MEMORY patch（节流）
    M3  摘要 → facts

P2  抽取与检索增强                     ✅ 2026-08-22
    M4  抽取视野（assistant/tool 有界上下文）
    M5  万级向量（LSH ANN，≥4096 激活）
    M6  谓词 LLM 合并（门闩 + demote/极性保留）

P3  ReAct 产品旋钮 + 窗口期硬化                     ✅ 2026-08-24
    R6  分支/重试跨生命周期窗口（lifecycle 矩阵 + cancel/join + 清 gate）
    R1  文档默认：steer 不 cancel 工具（无 CancelToolsOnSteer）
    R3  freeze+declare：短索引 run 内冻结，API tools[] 权威（后经 X2 重订）
    R4  RunBudget 入 snapshot
    R5  窗口矩阵单测 + 工具中/ask 等待 rollback 集成测

P3.1 X2 + G7 重订                                   ✅ 2026-08-24
    X2  resume 全量重建 system（短索引 + MEMORY）；M2 仍 fence-only
    G7  freeze-per-run（非整段 session）；TOOL_USAGE_NOTES 同步

P4  史诗（单独立项）
    X12 DB↔events 统一
    X1  记忆图谱/大表
    X5  矛盾引擎
    X6  记忆 UI Tab
    X13 events 冷存储

明确保持禁止（除非推翻 AGENTS.md / 安全模型）
    X3 内容比对去重 · X4 出窗灌回 · X8 TS 重写
    X9 去 snapshot · X10 去 confirm · X11 删截断重试
```

每期结束：`cargo test -p haven-memory -p haven-agent -p haven-tools -p haven-common`；涉及 schema 必 bump migration；UI 相关加 `/test-ui --run`。手测：ask / steering / resume / rollback / confirm / 中英记忆召回。

---

## 5. 验证基线

- Rust：`cargo test -p haven-memory -p haven-agent -p haven-tools -p haven-common`
- 静态：`cargo clippy -- -D warnings`；UI：`cd ui && npm run check`
- 协作回归：同会话不进 Past excerpts；resume/pause 后记忆段可更新；中英会话各一条
- ReAct 回归：pause 无残留 Running；ask/confirm 重启门闩仍在；并行工具无假 step 膨胀
- 分支/重试窗口（R6）：工具批中 rollback；流式中 empty/cut-off 重试；ask 答后 continue；claim→spawn 竞态；pause 写 snapshot 后立刻 branch
- 大改后：旧 `haven.db` / 旧 react_state **允许删库**；不必保留 dual-array / 无 fence 快照兼容，除非刻意留 `from_json` 只读迁移

---

## 6. 相关文档

- `docs/architecture.md` — crate 职责与依赖
- `AGENTS.md` — ID / resume / schema（改 X3/X12 前必须同步改本文与 AGENTS）
- `docs/conventions.md` / `docs/naming.md`
- 上游对照（只读）：https://github.com/earendil-works/pi/tree/main/packages/agent

---

## 变更记录

| 日期 | 内容 |
|---|---|
| 2026-08-24 | ReAct 热路径减负：heartbeat 不 await（per-session 合流）；`last_msg_at` 缓存（ingress 同步 + truncate 后清）；sanitize 健康快路径；thought step `run_blocking`；ToolDefCache `Arc`；去掉步头 `canonical.clone` |
| 2026-08-24 | P3 落地：R6 lifecycle 窗口矩阵 + rollback/continue 硬化；R1 文档默认；R3 freeze+declare；R4 RunBudget；R5 窗口测 |
| 2026-08-24 | 新增 R6：分支/重试在工具调用、模型流式、claim→spawn、ask/confirm、pause 等窗口期硬化 |
| 2026-08-22 | P2 落地：M4 抽取视野含有界 assistant/tool；M5 embedding_lsh + ANN≥4096；M6 维护期 LLM 谓词合并 |
| 2026-08-22 | P0+P1 落地：R2 非阻塞调度确认；X7 删除 Steps so far；M1 确认轮次抽取；M2 节流 MEMORY patch；M3 摘要→facts |
| 2026-08-21 | 初版：合并并取代 `memory-architecture.md` 与 `react-architecture-improvements.md`；完成项归档；剩余项含原「明确不做」；原则改为可大改、不向下兼容 |
