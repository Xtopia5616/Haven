# ADR 0479：SessionStore 历史查询测试归属

## 状态

已采纳并实施（2026-10-05）。

## 背景

ADR 0466 已把 `SessionStore` 的只读历史查询 API 与 DTO 移到私有 `session_history` 模块，但其行为测试仍与 event append、projection、rollback 等事务测试混在 `session_events.rs` 的 `tests` 模块中。六项测试只验证历史查询 façade 的 ordering、过滤、限制、媒体读取和返回值，不触碰事务私有状态；将它们留在事务测试组会让实现与测试职责分散。

## 决定

1. 将 `latest_session_record`、`title_generation_context`、`conversation_window`、`session_resume_media` 与历史 list/search façade 的六项既有测试移入 `session_events::session_history::tests`。
2. 在测试子模块内使用内存 `Database` 和本地媒体 fixture；不公开或跨模块导出 test helper。
3. 标题读取/写入与历史缓存失效测试继续留在 `session_events::tests`，因为它们覆盖仍由父模块拥有的写路径或缓存副作用；`session_resume_projection`、event append、rollback 与原子性测试也继续留在原 owner。
4. 保持生产 API、查询语义、数据库、事件、IPC 与运行时行为不变。

## 替代方案

- 保持现状：拒绝。历史 façade 已有稳定模块边界，测试只因共享一个大 `mod tests` 而与事务职责混放。
- 将整个 `session_events` 测试套件迁入多个子模块：拒绝。本次只移动六项纯读取 façade 测试，不根据行数拆解事务回归组。
- 公开共享测试 fixture：拒绝。新测试可用内存数据库和小型本地 helper，不需要扩大 crate 可见性。

## 影响与验证

该切片只调整测试模块位置，并移除父测试组中不再使用的 attachment helper；六项原测试及其断言保持不变。无 schema、持久数据、wire 或安全语义变更，无需数据库 reset。

验证：

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-memory repositories::session_events::session_history::tests`（6 passed）
- `cargo test --locked -p haven-memory`（389 passed，2 ignored）
- `cargo check --locked -p haven-memory`
- `cargo clippy --locked -p haven-memory -- -D warnings`
- `git diff --check`

## 回滚

将这六项测试和仅供它们使用的 fixture 移回 `session_events.rs` 的测试模块，并删除 `session_history.rs` 的 `#[cfg(test)] mod tests;` 声明及对应测试文件。无需迁移或重置数据。
