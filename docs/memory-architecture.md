# 记忆与 Facts 架构优化计划

> 状态标记：`[完成]` = 代码已实现；`[待办]` = 本计划内按优先级实现。
> 更新日期：2026-08-14

## 一、现状全景

记忆系统分两条通道，统一落库 SQLite（`haven.db`，WAL + 16 连接池）：

| 通道 | 表 | 说明 |
|---|---|---|
| 长期事实 | `facts` | subject/predicate/object 三元组，带 confidence / durability / tags / source(user\|inferred) / mention_count / last_seen_at / source_ref |
| 情景记忆 | `memory_episodes` | 压缩摘要 + 用户消息，`msg-{uuid32}` 与消息共用 ID 空间 |
| 向量索引 | `memory_embeddings` | entity_type 区分 `fact`/`episode`，f32 小端 blob + 表面文本，facts 变更由触发器失效 |
| 全文索引 | `facts_fts` | FTS5 外部内容表（unicode61 tokenizer），BM25 排序，LIKE 兜底 |
| 游标 | `kv_store` | `fact_extraction.{session_id}` 增量抽取游标 |

### 已实现的能力

1. **抽取**（`crates/agent/src/inference.rs`）：LLM 抽取（带敏感词过滤、置信度下限、谓词归一化、标签白名单），失败时非致命——记录 warning 后跳过该窗口，游标照常推进避免重复分析，不写入任何数据（原规则引擎兜底已移除）。
2. **写入与冲突**（`crates/memory/src/repositories/facts.rs`）：upsert 三态（reinforce / correct / skip），单值谓词旧值降权，用户声明值权威，likes/dislikes 极性冲突处理。
3. **衰减**：`fact_effective_confidence` 按谓词类型半衰期（volatile 90 天 / 其他 365 天）× durability，身份谓词与 user 源不衰减；维护任务定期 dedup / 敏感清除 / 低置信度剪枝 / 孤儿 embedding 清理。
4. **回答路径注入** `[完成]`：`SystemPromptBuilder`（`crates/agent/src/prompt.rs`）按会话关键词召回 facts（含跨 subject）+ episodes，按"有效置信度 + 关键词命中加分"排序取 top-15 注入 system prompt。
5. **记忆工具** `[完成]`：`facts` 工具（search / list / remember / forget，`crates/tools/src/builtin/facts.rs`）通过 `SelfToolContext` 注入 DB，读写均过滤敏感事实。
6. **连接层** `[完成]`：WAL + 有界连接池（`crates/memory/src/db.rs`），`run_blocking` 把 SQLite 工作移出 async runtime。

## 二、待办步骤（按优先级）

### 步骤 1：回答路径接入向量召回 `[待办]`

**问题**：回答路径只做关键词召回（`prompt.rs`）。用户配置了 `embedding_model` 后，向量检索只被 Tauri 命令 `recall_memory` 使用，system prompt 里的记忆仍是纯关键词匹配——同义表达、跨语言（中英混述）都召回不到。

**改动**：
- `SystemPromptBuilder` 增加可选 `router: Option<Arc<LlmRouter>>`（Agent 构造时注入），保持 `new()` 兼容现有测试。
- `build()` 中：embedding 槽位已配置且模型未切换时，用会话关键词做 query 嵌入，向量 top-k 结果与关键词结果合并（按 fact id 去重，向量命中加分），无向量命中或失败时静默降级到现有关键词路径。
- episodes 召回同样融合向量路径。

**验收**：配置 embedding 后，与已知事实语义相关但无共享关键词的会话描述能召回该事实；未配置 embedding 时行为与现状完全一致（现有测试不改动仍通过）。

### 步骤 2：迁移层 `[待办]`

**问题**：`schema.rs` 明确"无迁移层"，旧库缺列直接报错要求删库。开发期可接受，对已有用户数据不可逆。

**改动**：
- 引入 `SCHEMA_VERSION`（当前 = 1）+ `PRAGMA user_version` 存储。
- `init_schema` 三态：空库 → 建全量 schema 并写入版本；`user_version < SCHEMA_VERSION` → 按序执行 `MIGRATIONS`；`user_version > SCHEMA_VERSION` → 报错（旧程序开新库）。
- 保留现有 `REQUIRED_COLUMNS` 检查作为 v0 旧库防线（版本 0 且列不齐 → 删库提示；版本 0 且列齐 → 视为 v1）。
- 后续任何 schema 演进（新表/新列）必须写为迁移并 bump 版本。

**验收**：空库初始化后 `PRAGMA user_version == 1`；模拟旧库缺列仍报清晰错误；迁移函数幂等。

### 步骤 3：FTS 中文分词（trigram）`[待办]`

**问题**：`facts_fts` 用默认 unicode61 tokenizer，不切分中文连续串，CJK 检索实际退化到 LIKE 全表扫。rusqlite 0.32 bundled SQLite ≥ 3.46，支持 `tokenize='trigram'`（子串匹配，对 CJK 有效）。

**改动**：
- `ensure_facts_fts` 改用 trigram tokenizer；用 `kv_store` 键 `facts_fts_tokenizer` 记录已应用的 tokenizer，与预期不符才重建（避免每次启动 DROP 重建）。
- 保留 LIKE 兜底（trigram 对 1-2 字短词不命中）。
- 补一个中文事实检索测试（≥3 字命中，2 字回退 LIKE 命中）。

**验收**：中文 3 字以上子串检索走 FTS 命中；现有英文检索测试（"Rust"）仍通过；短词回退 LIKE 行为不变。

### 步骤 4：谓词规范化 `[完成]`

**问题**：谓词是自由字符串。规则引擎产出 `project_path`，LLM 可能产出 `workspace`/`project location`，`is_single_valued_predicate` 里 `workspace` 与 `project_path` 并存，同类事实分裂成多行、单值约束失效。

**改动**：
- `facts.rs` 增加 `normalize_predicate(&str) -> String`：trim + 小写 + 别名表映射（`workspace`/`workspace_path`/`project_location`/`working_directory` → `project_path`；`employer`/`company_name` → `works_at`；`favorite_language` → `language`；`preferred_verbosity` → `verbosity`；`preferred_shell` → `shell`；`os_name`/`operating_system` → `os`）。
- 写入入口统一归一化：`insert_fact_with_source_ref`（最底层，所有路径覆盖）、`set_user_fact`、`upsert_fact_with_durability`、`delete_facts_by_triple`（查询侧一致）。
- agent 层 `inference.rs` 的本地 `normalize_predicate` 改为委托 memory 层实现，消除两处漂移。
- 补测试：别名合并、单值替换、别名删除、upsert 路径归一化。

**验收**：`remember workspace D:\x` 后按 `project_path` 检索可命中；单值约束对 `workspace` 与 `project_path` 生效；别名表集中在文件顶部一处。

### 步骤 5：LLM 抽取调度 `[完成]`

**问题**：每轮新用户消息都立即触发一次 LLM 抽取（带全部已知 facts 当上下文），对话频繁时 token 成本高、响应延迟受抽取影响。

**改动**：
- `ContextLimitsConfig` 新增 `fact_extraction_min_interval_secs`（默认 60，`#[serde(default)]` 对旧配置自动生效）。
- `InferenceEngine` 加字段：间隔内的第二次调用直接跳过——**不推进抽取游标**，待处理消息在下次允许的运行（或维护任务）中继续处理，不丢失。
- 每次实际调用模型前打 `fact_extraction.last_run.{session_id}` 时间戳（失败也算一次运行，防止持久故障下每轮重试）。
- 与既有 `fact_infer_interval_steps`（步骤级门控）互补。
- 补测试：节流内跳过（游标不动、时间戳不重打、消息未丢失）。

**验收**：同一会话 60 秒内多条消息只触发一次 LLM 抽取；配置项可调。

### 步骤 6：移除规则引擎兜底 `[完成]`

**问题**：LLM 抽取失败时回退到 `infer_facts_from_messages` 规则引擎（if-else 触发词匹配），两条抽取通道行为差异大、维护两套逻辑成本高。

**改动**：
- `inference.rs`：LLM 抽取失败 → 非致命 warning，跳过该窗口，游标照常推进（避免持久性故障下反复分析同一批消息）。
- 删除 `facts.rs` 的 `InferredFact`、`tags_for_predicate`、`extract_object`、`extract_path_object`、`infer_facts_from_messages` 及全部规则测试。
- 测试 `infer_facts_rule_fallback_persists_and_indexes` 改写为 `infer_facts_llm_failure_skips_and_advances_cursor`（失败不落库 + 游标推进）。

**验收**：`cargo test -p haven-memory -p haven-agent` 全绿；代码库无 `infer_facts_from_messages` 引用。

### 远期（记录不实施）

- **ANN 检索**：当前向量召回为全表 cosine（`list_embeddings` + 逐条算）。事实量达万级后可引 `sqlite-vec`（HNSW，纯扩展）或换独立向量库；在此之前缓存 + 域限定（只召回用户事实）足够。
- **episodes 结构化**：episode 目前只有 summary 文本，可扩展主题/实体字段辅助聚类与关联。
- **source_ref 孤儿清理**：消息删除后 `source_ref` 悬空（JSON 列无法 JOIN），可改为外键或定期清理。

## 三、实施顺序与依赖

```
步骤 1（向量召回融合）   [完成] ← 功能收益最大，无 schema 依赖
步骤 2（迁移层）         [完成] ← 为后续 schema 演进打基础
步骤 3（trigram）        [完成] ← 中文全文检索
步骤 4（谓词规范化）     [完成] ← 纯数据层，无依赖
步骤 5（抽取调度）       [完成] ← 独立
步骤 6（移除规则兜底）   [完成] ← 随步骤 5 一起收敛抽取管线
```

全部步骤已完成。验证：`cargo test --workspace` + `cargo clippy -- -D warnings` 全绿。
