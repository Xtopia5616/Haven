# ADR 1109：为提示词分段与身份查询使用具名语义

## 状态

已接受（2026-10-13）

## 背景

Rust 命名审计发现 Common 的 `split_system_prompt_cache_sections` 返回 `(stable, session, memory)` 位置元组。三个切片有稳定且不同的领域含义，provider adapter 必须依赖位置理解结果。Memory 的 `all_session_ids`、`all_session_authorization_grants` 与可取消 session ID 读取返回集合，却没有采用项目规定的 `list_*` 查询动词。另有 `list_descendants(root)` 和 `FactGraph::delete_by_id(id)` 未在参数名中明确根 session 与事实 ID 身份。

## 决定

- 提示词解析返回 `SystemPromptCacheSections`，以 `stable_prompt`、`session_context` 和 `memory_context` 字段表达各段；LLM adapters 按字段读取。
- Memory 零到多条查询统一使用 `list_*`：`list_session_ids`、`list_session_ids_cancellable`、`list_session_authorization_grants`。
- 遍历/删除参数点名其身份：Messaging 的 descendants 起点为 `root_session_id`；事实删除入口与内部 helper 使用 `fact_id`，内部动作命名为 `delete_fact_by_id`。
- 更新命名规范版本，并将这批迁移记入规范审计路线图。

## 替代方案

保留元组及 `all_*` 方法会让消费者继续依赖位置索引和不一致的集合动词。增加只做别名转发的兼容 API 会留下两套公共词汇且本次只涉及仓库内源码调用点，因此直接迁移所有发现的消费者。

## 影响

改动仅影响 Rust 源码 API 与命名文档，不改变 provider payload、持久数据、IPC 或运行行为。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- 全库搜索旧 API 与位置型 prompt section 解构，确认无生产调用点残留。

## 回滚

如需回滚，必须同步撤销所有消费者的字段访问/API 名称与 `docs/naming.md` 规则；不涉及 schema 或用户数据迁移。
