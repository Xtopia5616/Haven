# ADR 0551：Memory 多行持久查询使用 list 动词

## 状态

已采纳并实施；Rust workspace 格式、编译、严格 Clippy 与测试通过。

## 背景

`haven_memory::Database` 的多行持久查询中，session messages、steps、usage、pending inputs 与 facts 查询使用 `get_*`，返回 `Vec<_>`；同一个 facts 查询模块又已有 `list_facts`、`list_facts_by_source`。`get_fact_by_id` 则返回一个可选实体。调用点跨 Memory、Agent 与 Tools，导致相同的集合读取角色有两套动词。

进程内 `QueryResultCache` 的 `cache_get_*` 也会返回集合，但它按稳定 cache key 读取单个缓存槽，不是数据库领域集合查询，因此保持原名。

## 决定

1. Memory 数据库多行读取统一使用 `list_*`：
   - `get_session_messages` → `list_session_messages`
   - `get_session_messages_limit` → `list_recent_session_messages`
   - `get_pending_session_inputs` → `list_pending_session_inputs`
   - `get_session_steps` → `list_session_steps`
   - `get_session_llm_usage` → `list_session_llm_usage`
   - `get_facts` → `list_facts_by_subject`
   - `get_facts_limited` → `list_facts_by_subject_limited`
   - `get_facts_by_ids` / `get_facts_by_tag` → `list_facts_by_ids` / `list_facts_by_tag`
2. 同步更新 Memory、Agent、Tools 中的定义、消费者和测试引用，不保留旧 Rust API 别名。
3. 按 ID 取单条记录的 `get_*` 与按缓存 key 取缓存槽的 `cache_get_*` 保留。
4. 只改方法名与相关注释/测试名；SQL、过滤、排序、缓存、ID、持久 schema、IPC 和恢复行为不变。
5. 将“按条件筛选但返回多实体时使用 `list_*`”写入命名规范，并明确缓存槽 getter 的例外。

## 替代方案

- 保留 `get_*` 并要求调用者查看返回类型：拒绝。方法动词应表达读取基数，避免调用链内混用。
- 将 cache getter 也改为 `list_*`：拒绝。它们读取由 cache key 唯一定位的槽，不执行领域列表查询。
- 保留兼容方法别名：拒绝。该 Rust API 仅供 workspace 内部调用，项目测试版本不增加无到期日的过渡层。

## 影响与验证

- 改动触及 Memory、Agent 和 Tools crate 的 Rust API 调用点，不改变 IPC/数据库契约或运行行为。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1` 与 `git diff --check`。

## 回滚

恢复旧 Rust 方法名和调用点，并移除本 ADR、命名规则与路线图条目；无需重置数据库或用户配置。
