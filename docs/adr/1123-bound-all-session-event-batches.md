# ADR 1123：限制所有会话事件批次

## 背景

SessionStore 已限制包含 transcript 的批次最多 128 条、序列化后最多 4 MiB，但校验会对纯 domain event 提前返回。`interaction_requested` 等任意 JSON 事件因此可绕过行数和 payload 上限。异步 domain event 入口还会在写线程校验前复制 payload 或整批 events；确认事件的 tool input 可让这次复制先消耗无界内存。

## 决定

1. 所有 `session_events` 批次共用最多 128 条、序列化 event array 最多 4 MiB 的限制，覆盖 transcript、domain event、seed、rollback replacement 和底层 append 写入。
2. 大小按完整 `SessionEventInput` JSON 表示计算，使用有界 byte counter；达到上限即停止序列化并返回预算错误，不生成完整副本。
3. `append_domain_event`、带状态的 domain batch 和 transcript commit 在复制或调度 blocking 写之前，对借用输入做预算校验。事务写入边界再次校验，保持内部调用同样受限。
4. transcript 投影行数限制与 transcript commit 的 event 加 projection 总 payload 限制继续生效。

## 替代方案

- 只在 SQLite 事务内拒绝超限事件：会先把大批数据复制/排入 blocking pool，并可能先取得写锁。
- 对每种 domain event 单独维护 payload 上限：会复制同一容量契约并造成新事件类型漏配。
- 用 `serde_json::to_vec` 完整序列化后再测长度：大 payload 会在拒绝前额外分配完整副本。

## 影响

超限 domain event 和 batch 会在进入事务前失败，状态更新及 live broadcast 不发生。现有事件记录不变；没有 schema、IPC 或配置变化，不需要重置数据库。

## 验证

- 测试覆盖 domain batch 的 128 条上限与 129 条拒绝。
- 测试覆盖超大单条 domain payload 被拒绝，且带状态 batch 失败后 session status、event sequence 和 live stream 不变。
- 运行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-memory` 和 `cargo clippy --workspace --locked -- -D warnings`。

## 回滚

回滚只影响新写入的批次限制，不要求数据迁移。若回滚，既有事件仍可读取，但未来 domain event 会重新失去与 transcript 相同的批次预算。
