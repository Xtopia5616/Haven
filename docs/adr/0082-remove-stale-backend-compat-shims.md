# ADR 0082：删除后端过时兼容层

## 背景

后端已经完成若干职责拆分，但仍保留了没有 workspace 生产调用的旧入口：

- `haven_tools::bg` 只转 re-export，真实实现已经位于 `background_actions`、`shell_runtime`、`output` 和 `process`；
- `ReActSnapshot.upgrade_tool_rounds` 只被测试 fixture 填充，快照解析和恢复流程不读取它；
- `Database::search_episodes_by_keywords` 与 `_excluding` 只返回文本，新的 typed 查询已经返回 `entity_id + text`，而生产召回只使用 typed 结果。

这些入口制造了多个看起来都像权威实现的路径，也会让后续代码继续依赖已经过时的命名或丢失记忆实体身份。

## 决定

- 删除 `crates/tools/src/bg.rs` 及 `pub mod bg`，调用方统一使用拆分后的模块或 crate-root 导出。
- 删除 `ReActSnapshot.upgrade_tool_rounds` 及所有测试 fixture 字段。
- 删除 memory 的 text-only keyword facade，将唯一实现命名为 `search_episodes_by_keywords`。
- 将 `EpisodeKeywordHit` 作为关键词召回的唯一内部结果形状，保留 `entity_id` 以支持跨关键词/向量候选去重。

## 替代方案

- 继续保留 `bg` re-export：只会延长已完成拆分的迁移窗口，且当前没有 workspace 调用方，不采用。
- 保留 text-only 查询作为便捷 API：会继续鼓励调用方丢失实体身份，不采用。
- 为 `upgrade_tool_rounds` 保留 `serde(skip)` 空字段：生产不需要它，且测试 fixture 不应定义快照模型，不采用。

## 影响与回滚

本次只删除 workspace 内部 Rust API 和进程内测试字段，不改变数据库 schema、配置文件、IPC/wire 格式或用户数据，不需要重置。外部独立 crate 若依赖这些未承诺稳定的内部路径，需要迁移到当前模块或 typed 查询。

回滚本提交即可恢复这些入口，但不应重新引入新的调用方。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
