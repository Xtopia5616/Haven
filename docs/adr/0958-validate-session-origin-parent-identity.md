# 0958：校验 SessionOrigin 的父会话身份

## 状态

已接受并实现（2026-10-11）。

## 背景

Agent 通过 Memory `SessionStore::create_session_with_origin` 创建带 provenance 的 peer session；SQLite `sessions.parent_session_id` 保存 `SessionOrigin::AgentSpawn` 的父会话身份。Memory 写入此前只查询父 ID 是否存在，行映射读取也只检查 origin 与 parent 列的组合，没有按统一实体 ID 契约验证父 ID。因而畸形身份可能被接受为历史来源，或在坏数据已存在时继续投影为有效 `SessionOrigin`。

## 决定

- Memory 在打开写事务前用 `is_canonical_id(parent_session_id, "ses")` 校验 `AgentSpawn` 来源；格式错误立即返回错误，不分配子会话 ID，也不写入 session。
- 从 SQLite 读取 `AgentSpawn` provenance 时执行同一校验；非规范父 ID 按无效持久化 metadata 返回转换错误。
- 保留对格式正确父 ID 的存在性校验；父会话删除后，已创建子会话的历史 provenance 仍可读取，不引入外键级联。

## 替代方案

- 只依赖 Agent/调用方校验：`SessionStore` 与 Memory 数据库入口均可被其他受信任调用方使用，调用方检查不能保护持久化 owner。
- 只在 Tauri response mapper 校验：不能阻止畸形身份写入数据库，也不能保护 Memory 的内部消费者。

## 影响

有效的 peer session 创建和父会话删除语义不变。该改动只校验已有 SQLite 字段，不修改 schema、IPC 或数据布局；发现格式错误的旧行时会显式读取失败，不需要 schema 迁移或数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-memory -- repositories::sessions::tests`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

若会话身份格式正式变化，应先更新 Common 生成和校验契约，再同步 Memory 的 provenance 写入及读取边界；不要仅移除 owner 校验并允许任意字符串持久化。
