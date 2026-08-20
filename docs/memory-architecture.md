# 记忆与 Facts 架构

> 状态标记：`[完成]` = 代码已实现；`[待办]` = 按优先级推进。
> 更新日期：2026-08-20

本文描述 **当前实现**、引擎侧 backlog，以及 **Facts / Episodes / 对话历史** 的协作改进计划（短期 / 长期）。早期六步计划（向量召回融合 / 迁移层 / trigram / 谓词规范化 / 抽取调度 / 移除规则兜底）与 Memory P0（CJK 分词 / 抽取维护解耦 / embed 有界）均已落地，不再以旧待办清单形式保留正文。

---

## 一、现状全景

记忆系统分两条通道，统一落库 SQLite（`haven.db`，WAL + 有界连接池）：

| 通道 | 表 | 说明 |
|---|---|---|
| 长期事实 | `facts` | subject/predicate/object 三元组，带 confidence / durability / tags / source(`user`\|`inferred`) / mention_count / last_seen_at / source_ref |
| 情景记忆 | `memory_episodes` | 压缩摘要（compaction）；用户消息与摘要共用 `msg-{uuid32}` ID 空间，一并进入 episode 向量域 |
| 向量索引 | `memory_embeddings` | entity_type 区分 `fact`/`episode`，f32 小端 blob + 表面文本；facts 变更由触发器失效 |
| 全文索引 | `facts_fts` | FTS5 外部内容表，**`tokenize='trigram'`**（CJK 子串），BM25 排序；短查询 / 空结果回退 LIKE |
| 游标 / 元数据 | `kv_store` | `fact_extraction.{session_id}` 增量抽取游标；`fact_extraction_last_run.{session_id}` 节流时间戳；`facts_fts_tokenizer` |

Schema 由 `haven_memory::schema::init_schema` 管理：`PRAGMA user_version` + `MIGRATIONS`，当前 **`SCHEMA_VERSION = 2`**（v2：谓词别名回填 + 去重）。缺 `REQUIRED_COLUMNS` 的远古库仍拒绝打开（需删库重建）；版本高于二进制的库也会拒绝。

### 已实现能力

1. **抽取**（`crates/agent/src/inference.rs`）：LLM-only（BalancedModel），敏感词过滤、置信度下限、谓词归一化、标签白名单；失败非致命——warning 后跳过窗口，游标照常推进。时间节流（`fact_extraction_min_interval_secs`，默认 60s）与步骤门控（`fact_infer_interval_steps`）互补。规则引擎兜底已移除。
2. **写入与冲突**（`crates/memory/src/repositories/facts.rs`）：upsert 三态（reinforce / correct / skip）+ Inserted；单值谓词旧值降权；用户声明权威；likes/dislikes 极性冲突；`normalize_predicate` 别名表集中在 memory 层。
3. **衰减与维护**：`fact_effective_confidence`（volatile 90 天 / 其他 365 天 × durability；身份谓词与 `source=user` 不衰减）。热路径 `infer_session` 只抽取 + 有界 embed；全表 `run_memory_maintenance`（dedup / 敏感清除 / 低置信度剪枝 / 孤儿 embedding / 孤儿游标 / embed 补齐）由 app 调度器在**启动时跑一次**，之后约每 6h 再跑；设置页命令走同一路径。
4. **回答路径注入** `[完成]`：`SystemPromptBuilder`（`crates/agent/src/prompt.rs`）在会话启动时构建；`memory_recall_terms` 关键词召回（含 CJK trigram）+ 跨 subject `search_facts`，与 **向量融合**（`embedding_model` 已配置且索引未因换模失效时）合并，按「有效置信度 + 关键词加分 + 向量命中加分」取 top-15 facts、top-5 episodes 注入 system prompt。
5. **记忆工具** `[完成]`：`facts` 工具（search / list / remember / forget，`crates/tools/src/builtin/facts.rs`）；Tauri `recall_memory`；读写均过滤敏感事实。
6. **连接层** `[完成]`：WAL + 有界池（`crates/memory/src/db.rs`），`run_blocking` 移出 async runtime；facts / embeddings 查询缓存（约 60s TTL）。

### 已完成的历史步骤（归档）

| 步骤 | 状态 | 要点 |
|---|---|---|
| 1. 回答路径接入向量召回 | `[完成]` | `SystemPromptBuilder::with_router`；失败静默降级关键词 |
| 2. 迁移层 | `[完成]` | `SCHEMA_VERSION` + `MIGRATIONS`；当前版本 2 |
| 3. FTS trigram | `[完成]` | `ensure_facts_fts` + `facts_fts_tokenizer`；CJK ≥3 字走 FTS |
| 4. 谓词规范化 | `[完成]` | memory 层唯一 `normalize_predicate`；写入/删除入口统一 |
| 5. LLM 抽取调度 | `[完成]` | `fact_extraction_min_interval_secs`；节流内不推进游标 |
| 6. 移除规则引擎兜底 | `[完成]` | 失败跳过 + 游标推进；无 `infer_facts_from_messages` |

---

## 二、待优化点（对照当前代码）

### P0

#### 1. 回答路径 CJK 关键词分词 `[完成]`

- **落地**：`haven_common::text::memory_recall_terms`（拉丁词按字符长度 + 停用词；CJK 连续串出 trigram，长串取头尾各半）；`prompt.rs` / `recall_memory` 共用；facts / episodes 关键词各取最多 6 个 term。

#### 2. 抽取与重维护解耦 `[完成]`

- **落地**：热路径 `infer_session` = `infer_facts` + 有界 `embed_new_memory`；`spawn_infer` 改调它。全表 `run_memory_maintenance` 由调度器启动即跑 + 每 6h，以及设置页命令（经 agent 层，含 embed 补齐）。

#### 3. Episode 向量补齐积压有界化 `[完成]`

- **落地**：`missing_embedding_ids` 默认 `EPISODE_EMBED_BACKLOG_LIMIT=64` / `FACT_EMBED_BACKLOG_LIMIT=128`；episode 优先 compaction 摘要，再补最近用户消息；可 `missing_embedding_ids_limited` 调 cap。

### P1

#### 4. 向量检索规模化 `[待办]`

- **问题**：`search_embeddings` = 缓存全表 + 暴力 cosine。百级可接受，上千条 × 高维时拖慢会话启动的 prompt build 与 `recall_memory`。
- **位置**：`crates/memory/src/embeddings.rs`
- **方向**：先做域/subject 限定与更紧的 top-k；事实量达万级再评估 `sqlite-vec` / HNSW。
- **风险**：中（扩展依赖 / 重建索引）

#### 5. Prompt 召回查询形态 `[待办]`

- **问题**：先 `get_facts("user")` 拉全量 user subject（虽有 60s 缓存），再对最多 6 个 term 各打一次 `search_facts`（FTS 或 LIKE，**无 SQL LIMIT**），再在内存里 top-15。
- **位置**：`prompt.rs`；`facts.rs::search_facts` / `get_facts`
- **方向**：多 term 一次 FTS `OR` + `LIMIT`；种子集用 `ORDER BY confidence LIMIT k`，避免「先全量再截断」。
- **风险**：低

#### 6. 维护扫描下沉 SQL `[待办]`

- **问题**：`dedup_facts` / `delete_sensitive_facts` / `flush_low_confidence` 多在 Rust 侧拉全表；敏感删除可逐行；部分路径缓存失效不完整。
- **位置**：`crates/memory/src/repositories/facts.rs`
- **方向**：能下推的用 SQL（窗口去重、批量 DELETE）；缓存按 generation 全局 bump。
- **风险**：低–中

#### 7. 抽取移出 ReAct 控制流 `[待办]`

- **问题**：抽取/维护仍由循环与 pause 路径直接 spawn，与 BalancedModel 信号量争用，难独立压测与调参（见 ReAct 改进清单中的厚循环问题）。
- **位置**：`react` + `layer.rs::spawn_infer`
- **方向**：ReAct 只入队 `session_id`；独立 worker / outbox 消费。
- **风险**：中（与游标推进的时序）

#### 8. 记忆注入按 token/字符预算 `[待办]`

- **问题**：仅有条数上限（15 facts / 5×200 字 episodes）；长 object（路径等）仍可撑大 system prompt，与 tools/skills 段无预算协调。
- **位置**：`prompt.rs` facts / episodes 段
- **方向**：记忆段字符或近似 token 预算；优先高分短字段，超预算截断 object。
- **风险**：低

### P2

#### 9. `source_ref` 孤儿清理 `[待办]`

- **问题**：`source_ref` 为 JSON，非 FK；消息删除后悬空。维护任务已清孤儿 embedding，未清 source_ref。
- **位置**：`facts` 表 + `run_memory_maintenance`
- **方向**：定期将缺失 `message_id` 的 source_ref 置空，或只保留 snippet 不存 id。
- **风险**：低

#### 10. Episodes 结构化 / 检索 `[待办]`

- **问题**：`memory_episodes` 仅 summary 文本；关键词检索是 Rust 侧对 messages∪episodes 的 LIKE（约 1000 上限），无 FTS；无主题/实体字段。
- **位置**：`episodes.rs`；`embeddings.rs::search_episodes_by_keywords`
- **方向**：可选 topic/entity 列（需 migration v3）；episode FTS；或减少「每条用户消息都进向量域」。
- **风险**：中

#### 11. 谓词 / 冲突覆盖面 `[待办]`

- **问题**：极性冲突目前 mainly likes↔dislikes；别名表有限（如 `company` 未并入 `works_at`），LLM 自由谓词仍易分裂行；normalize 后部分旧 volatile/single-valued 列表项成死条目。
- **位置**：`normalize_predicate` / `is_single_valued_predicate` / upsert 冲突分支
- **方向**：扩展别名与单值集；可选维护期 LLM 辅助合并。
- **风险**：低–中（误合并）

#### 12. `facts` 工具多 subject `[待办]`

- **问题**：list / remember / forget 写死 `subject="user"`；抽取会写实体 subject，工具侧难按 subject 列出/遗忘。
- **位置**：`crates/tools/src/builtin/facts.rs`
- **方向**：可选 `subject` 参数；list 支持跨 subject 最近 N 条。
- **风险**：低

#### 13. Embedding 按当前 model 过滤 `[待办]`

- **问题**：`search_embeddings` / `get_embedding` 依赖换模时 `clear_embeddings` 成功；若清理失败，混模向量会污染 cosine。
- **位置**：`embeddings.rs`；`inference.rs::embedding_model_changed`
- **方向**：查询始终 `WHERE model = ?`；混模时 fail-closed。
- **风险**：低

#### 14. FTS 空结果 → LIKE 策略收紧 `[待办]`

- **问题**：trigram 下空 FTS 结果多为真未命中；长查询仍回退全表 LIKE 成本高。注释仍写「default tokenizer 不切 CJK」（过时）。
- **位置**：`facts.rs::search_facts`
- **方向**：仅短查询（如 &lt; 3 字符 / &lt; 3 个 CJK 字）走 LIKE；更新注释。
- **风险**：低

---

## 三、Facts / Episodes / 对话历史协作改进计划

> 目标：三套存储继续独立落库，但在 **读写策略上合成一套协作记忆**——本会话靠 canonical，跨会话靠 facts/episodes，彼此不重复、不冻结、不抢权威。
> 与 §二引擎 backlog 互补：§二偏检索/维护性能；本节偏「和对话历史怎么一起工作」。

### 3.1 协作现状（简图）

| 层 | 存储 | 写入 | 读入 LLM |
|---|---|---|---|
| 对话 / ReAct | `canonical`（+ DB `messages` / `session_steps`） | 循环、inject、compaction | **每步**完整消息数组 |
| Facts | `facts` (+ FTS / embeddings) | `infer_session`（读 DB 用户消息） | system prompt「USER FACTS」——**主要在新开会话** |
| Episodes | `memory_episodes` + 用户消息作 episode 实体 | compaction → `add_episode`；用户消息就地索引 | system prompt「Past conversation excerpts」——**主要在新开会话** |

写路径已通（抽取、压缩写 episode、pause/步间 infer）；读/同步弱：resume 冻结 system 记忆块、同会话文本多重注入、`source_ref` 只写不用。

### 3.2 协作断点（对照代码）

1. **记忆注入一次、之后冻结**：新开会话 `prompt_builder.build` 写入 `canonical[0]`（`layer.rs`）；resume 整份恢复 snapshot，中途 `infer_session` 更新表但不改 system 记忆段。
2. **同会话三重叠**：DB 窗口 →「Additional context」、episode 召回（未排除当前 `session_id`）、首条 user = `session.input`，同一段话可出现多次。
3. **权威不清晰**：canonical / history / messages / steps + 两张记忆表并存；生产调用 `build(..., history=&[])`，「Steps so far」实际未用。
4. **`source_ref` 写而不读**：抽取写入消息溯源，无 prompt/矛盾/UI 消费者；消息删除后易孤儿（§二 P2-9）。
5. **compaction ↔ episode 单向**：压缩改 canonical 并 `add_episode`，但 episode id 与压缩气泡不共享；不从摘要再抽 facts。
6. **抽取读 DB 不全看 canonical**：能看见已压出窗口的用户话（利于记忆），但与模型当前视野解耦，且不含 assistant/tool 轮次。

### 3.3 短期（优先落地，无大爆炸 schema）

状态默认 `[待办]`；落地后改为 `[完成]`。

#### S1. 权威边界写进注入策略 `[待办]`

- **问题**：四套「历史」+ 两套记忆，模型「这一轮记得什么」不清晰。
- **方向**（策略，少改代码即可先固化）：
  - **canonical** = 本会话 LLM 真源（含压缩摘要气泡）
  - **facts / episodes** = **跨会话**检索层，不负责复述本会话已在 canonical 里的内容
  - **DB messages** = 持久化 + 抽取源；禁止把「已在 canonical 中的同会话窗口」再灌进 Additional context
- **位置**：`layer.rs`（fresh / continue 注入）、`prompt.rs`、文档契约
- **风险**：低
- **验收**：注释/单测固定「本会话不进 Past excerpts；Additional context 不与首条 user 重复」

#### S2. 同会话去重：episode 排除当前 session + 收敛 Additional context `[待办]`

- **问题**：新开/续跑时，当前会话消息可被当成「过去对话」召回，并与 inject 窗口重复。
- **方向**：
  - `search_episodes_by_keywords` / 向量 episode 召回支持 `exclude_session_id`（默认当前会话）
  - fresh/continue：若即将把同窗口放进 canonical，不再（或大幅缩短）Additional context
- **位置**：`embeddings.rs`、`prompt.rs`、`layer.rs`
- **风险**：低
- **验收**：同会话用户句不出现在「Past conversation excerpts」；续跑 prompt 体积不因重复历史膨胀

#### S3. resume / 抽取成功后只 patch 记忆段 `[待办]`

- **问题**：长会话 facts 表已更新，system 里仍是开场那份；resume 也从不刷新记忆。
- **方向**：
  - 抽出「只渲染 facts + episodes 段」的 builder API（不动 tools/skills 缓存）
  - 在 **resume 进入循环前**、以及 **`infer_session` 成功且有写入之后**（可节流，例如仅 pause 时）替换 `canonical[0]` 中带标记的记忆 fence，或替换独立的 Memory system/user 消息
  - **禁止**每次整份重建 system prompt（tools 索引 + 快照体积）
- **位置**：`prompt.rs`、`layer.rs`、`inference` / pause 钩子
- **风险**：中（消息形状、snapshot 体积、provider 对改写 system 的敏感度）
- **验收**：同会话 pause 后新抽出的 fact 在下一步可见；resume 后跨会话事实不落后于库超过一次维护/抽取周期

#### S4. 与 ReAct 改进对齐的轻量衔接 `[待办]`

- **问题**：compact / infer 仍在厚循环内，协作调参困难（见 `docs/react-architecture-improvements.md` G2）。
- **方向**：短期不拆完钩子，但约定：记忆 patch（S3）挂在 pause / resume，不挂每步 prologue；抽取继续 `infer_session`，全表维护仍只走调度器。
- **风险**：低
- **验收**：文档与调用点一致；步间 infer 不触发全量 system 重建

**短期建议顺序**：S1 → S2 → S3 → S4（S1/S2 可同 PR）。

### 3.4 长期（架构债与增强，按需分期）

#### L1. Compaction 与 episode 共享稳定身份 `[待办]`

- **问题**：摘要在 canonical 与 `memory_episodes` 各一份、id 无关，易双嵌、难溯源。
- **方向**：压缩时生成/复用稳定 `msg-*`；可选在压缩气泡 metadata 记下 episode id；只 embed 一次；可选对摘要跑轻量抽取（遵守现有游标/节流）。
- **风险**：中（serde / resume / 旧快照）
- **依赖**：最好在 S1 权威边界稳定后做

#### L2. `source_ref`：用起来或降级 `[待办]`

- **问题**：写了没人读；消息删除后悬空。
- **方向**（二选一，勿先上第二套矛盾引擎）：
  - **用**：维护清孤儿；工具/设置页「为何记得」展示 snippet；upsert 已有 demote 逻辑即可
  - **降**：不再存 `message_id`，只留短 snippet
- **风险**：低
- **与**：§二 P2-9 合并跟踪

#### L3. 抽取出 ReAct 控制流（outbox / worker） `[待办]`

- **问题**：与 BalancedModel、compact、inbox 争用；难单测（§二 P1-7 + ReAct G2）。
- **方向**：循环只入队 `session_id`；独立 worker 跑 `infer_session`；pause/resume 的记忆 patch（S3）由 worker 完成回调或下一轮入口触发。
- **风险**：中（游标与 pause 时序）

#### L4. 记忆注入预算与召回查询形态 `[待办]`

- **问题**：仅条数上限；`get_facts("user")` 全量 + 多路无 LIMIT 搜索（§二 P1-5 / P1-8）。
- **方向**：记忆段字符/近似 token 预算；多 term 一次 FTS `OR` + SQL `LIMIT`；与 S3 patch 共用同一渲染函数以免两套截断策略。
- **风险**：低

#### L5. 抽取窗口适度包含「用户确认」轮次 `[待办]`

- **问题**：仅 user 消息时，Agent 提问后用户只回「好的/就要这个」可能抽不到偏好。
- **方向**：仅当评估显示漏抽率高时，把「成对的上一 assistant 问句 + 当前 user」纳入抽取窗口；默认仍不整段 transcript 进抽取。
- **风险**：中（噪声 / 把 agent 话当事实）

#### L6. Episodes 结构化 / FTS（可选） `[待办]`

- **问题**：episode 关键词靠 LIKE；无主题/实体（§二 P2-10）。
- **方向**：migration 增加可选 topic/entity；或 episode FTS；或减少「每条用户消息都进向量域」。
- **风险**：中；仅当跨会话情景召回质量成为瓶颈时做

### 3.5 明确不做

- 不把 `facts`、`memory_episodes`、`messages` 一次合成「记忆大表 / 图谱」。
- 不在 resume 时每次全量重建 system prompt。
- 不恢复「按内容比对」的 resume 去重（继续 `saved_at` / canonical 权威，见 `AGENTS.md`）。
- 不为了「保险」把已压缩出窗口的整段 DB 历史灌回 canonical（用 facts/episodes 跨会话，用压缩摘要管本会话）。
- 不在 `source_ref` 被消费前另建矛盾引擎。
- 不把记忆做成独立 UI Tab；召回保持 prompt / 工具结果形态（项目约束）。
- 不并行启用「Steps so far」与 canonical 双通道；生产只认 canonical。

### 3.6 与其他文档的关系

| 文档 | 关系 |
|---|---|
| 本文 §二 | 引擎/检索/维护性能项；短期协作优先于多数 P1 性能项，但 P1-5/8 可与 L4 合并实施 |
| `docs/react-architecture-improvements.md` | G2（compact/infer/inbox 出 prologue）与 L3 / S4 对齐；B1 多 transcript 收敛与 S1 同方向 |
| `AGENTS.md` | resume / id / schema 规范；协作改动不得违反 |

---

## 四、引擎侧 backlog 实施顺序

```
§二 P0-1..3                 ← [完成] 2026-08-20
§三 S1 → S2 → S3 → S4       ← 协作短期（下一阶段优先）
§二 P1-5 / P1-8 与 §三 L4   ← 可合并：预算 + 查询形态
§二 P1-4 / P1-6、§三 L1–L3  ← 规模与架构债
§二 P2-* / §三 L5–L6        ← 按需；schema 相关走 migration bump
```

验证基线：`cargo test -p haven-memory -p haven-agent -p haven-common`；涉及 schema 时补 migration 测试；协作项另测：同会话不进 Past excerpts、resume/pause 后记忆段可更新、中英会话各一条。

---

## 五、相关文档

- `docs/architecture.md` —— 总架构
- `docs/react-architecture-improvements.md` —— ReAct 厚循环拆分（抽取出环与协作短期 S4 / 长期 L3 互补）
- `AGENTS.md` —— ID 规范与 schema / migration 约定
- 实现入口：`crates/memory/`、`crates/agent/src/inference.rs`、`crates/agent/src/prompt.rs`、`crates/agent/src/layer.rs`、`crates/tools/src/builtin/facts.rs`

---

## 变更记录

| 日期 | 内容 |
|---|---|
| 2026-08-14 | 初版优化计划（步骤 1–6） |
| 2026-08-20 | 对照代码改写为「现状 + 新 backlog」；步骤 1–6 归档为已完成；FTS/迁移/向量融合等过时描述修正；新增 P0–P2 优化项 |
| 2026-08-20 | P0-1/2/3 落地：`memory_recall_terms`、热路径 `infer_session`、embedding backlog LIMIT |
| 2026-08-20 | review 修复：维护启动即跑、CJK 头尾 trigram、中文召回测试加固、episode term cap、去掉 `infer_all` |
| 2026-08-20 | 新增 §三：Facts / Episodes / 对话历史协作改进计划（短期 S1–S4 / 长期 L1–L6 / 明确不做） |
