# ADR 1305：隔离 SessionStore event replay/read 实现

状态：已接受并实施（2026-10-17）。

## 背景

`SessionStore` 的 event append 与 transcript projection 已进入私有模块；durable replay state、bounded page、active timeline、branch-point 读取和 App resume aggregation 仍在 facade 文件中。读侧必须保留原 event sequence、rollback 和 compaction boundary，并从同一 SQLite read boundary 读取恢复 cursor。

## 决定

1. 将 replay state、event page、active timeline、domain/branch-point read、transcript cursor 映射与 `session_resume_projection` 移入私有 `event_replay` 模块。
2. 保留 `SessionStore` 的公开方法与返回类型路径。`event_replay` 只实现 facade，不向 Agent 或 App 暴露新模块或 backing database。
3. 保留 active replay 规则：rollback marker 只移动 active cursor；`compact_summary` 成为新的 active root；若后续 rollback 指向 root 之前则回退到完整 replay，保证正确性优先。
4. `load_replay_state` 仍在一个 SQLite read transaction 中读取 active transcript、branch points 与 projection clocks；event sequence、transcript cursor 和三表投影 cutoff 继续各自表达原契约。
5. 将数据库 event row mapper 放入 `event_model`，保持 canonical session ID 与无符号 run ID 校验。

## 替代方案

- 把 replay policy 放进 Agent 或 UI：拒绝。Memory 是 durable event log 的 owner，调用方不能通过 messages/snapshot 重建 event history。
- 把 rollback/usage mutation 一并移进 read 模块：拒绝。它们修改 append-only log 和 projection，留给下一子切片按原子事务边界整理。
- 在拆分时改变 sequence、page capacity、compaction fallback 或 branch-point 选择：拒绝。本切片只移动实现。

## 影响与恢复

event 数据、schema、sequence、rollback/compaction 规则、IPC 和 UI 行为均未变化，不需要数据库重置或迁移。回退时恢复原私有模块布局即可。

## 验收

- `cargo test --locked -p haven-memory`：477 passed，2 ignored。
- `cargo clippy --locked -p haven-memory -- -D warnings` 通过。
- `cargo check --workspace --locked` 通过。
- `cargo fmt --all -- --check` 通过。
- 既有 event page bounds/order、active replay、rollback/compaction、resume cursor、branch metadata 和 1k/10k/100k active replay benchmark 测试通过。
