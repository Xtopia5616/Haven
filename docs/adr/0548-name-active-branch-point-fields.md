# ADR 0548：将 StoredBranchPoint 位置元组改为具名结果

## 状态

已采纳并实施；Rust workspace 门禁通过。

## 背景

Memory 的 `StoredBranchPoint` 是 `(SessionEvent, usize, u32, Option<String>)`。调用点通过 tuple 索引/解构推断这四项是 durable event、transcript cursor、step number 和 `last_msg_at`。它们分别参与 rollback 的 event clock、transcript cursor 和投影时间边界；把它们位置传递跨过 Memory→Agent 边界，容易互换时钟或读错身份。调用者只使用 event 的 sequence，并不需要整条 `SessionEvent`。

## 决定

1. 将类型改为 `ActiveBranchPoint` 具名结构体，公开 `event_sequence`、`event_cursor`、`step_number` 与 `last_msg_at` 字段。
2. 只保留消费者实际需要的 event sequence，不再把整条 `SessionEvent` 放进 branch point 结果。
3. Memory rollback 的查询与 Agent 恢复映射改用字段访问；事件选择、回滚 cutoff 与重放语义保持不变。
4. 在命名规范中约定：跨模块或 crate 传递且字段各有领域含义的多值结果使用具名字段。

## 替代方案

- 继续用 tuple，只增加注释：拒绝。调用点仍依赖位置，注释不能约束字段对应关系。
- 将 `last_msg_at` 改为派生 `event_cursor` 或 event sequence：拒绝。rollback 的 `BranchPoint.event_cursor` 与 event sequence 是不同游标，projection timestamp 又是第三个时钟，不能互相换算。
- 在 Agent 和 Memory 合并 BranchPoint 类型：拒绝。Memory 的 active durable view 与 Agent 的 ReAct recovery projection 有不同 owner 和用途，Agent 继续从 durable view 映射自己的运行态结构。

## 影响与验证

- Rust public API 的结果类型由 tuple 改为具名结构体，仅限 workspace 消费者源码；不改变 durable event payload、数据库、IPC 或 rollback/recovery 行为。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复 `StoredBranchPoint` tuple 类型、crate re-export、所有 tuple 解构点和原测试断言，并移除此 ADR 与路线图/命名规范记录。无需数据库或数据迁移。
