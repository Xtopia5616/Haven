# ADR 0209：删除 ReAct snapshot，并把 session 副作用收进单一应用边界

## 状态

已接受（2026-09-22）

## 背景

生产恢复已经只 replay `session_events`，但 `ReActSnapshot` 仍是公开类型。
`MessagingPoller` 还按 session 缓存 inbox receiver、轮询节拍和标题。工具批次则在
执行路径上直接写 transcript、branch point 和 confirm pause，与
`TurnEngine -> EffectBatch` 的单一应用器不一致。

## 决定

- 删除公开 `ReActSnapshot` 和 `sessions.react_state`。测试只通过 event projection
  建立 transcript，不再序列化 snapshot。schema 升至 v28；旧数据库删除后重建，不迁移。
- session-local inbox watch、`steps_since_poll` 和 title cache 放在 `SessionState`，
  随 session clear 一起清除。进程级 `MessagingPoller` 只保留 `MessagingService` 和
  heartbeat coalescing，避免一个 session 占满 blocking pool。
- turn 终态，以及工具批次中的 assistant tool-call transcript、branch point、按序
  observation、confirm/ask pause，都经 `EffectBatch` 由 `apply_committed_batch` 按声明
  顺序提交。确认等待文案仍是 UI-only `ProjectChatMessage`，不进入 durable event stream。
- turn-start context injection 定义当次 provider 请求，stream chunk 属于流边界；二者都
  不是可延迟的 turn effect，不复制进第二套 writer。

## 替代方案

保留 `react_state` 测试列会让 schema 继续携带没有生产读者的 snapshot 模型。把轮询缓存
留在 `MessagingPoller` 会让 session 清理和 actor 所有权分裂。让工具批次继续直接写，则会
在 confirm pause 与 transcript commit 之间保留第二套顺序。

## 影响、重置与回滚

IPC 与前端事件契约不变。含有 `sessions.react_state` 或 `react_checkpoints` 的旧数据库必须按
`docs/release-and-reset.md` 删除 `haven.db`、`haven.db-wal` 和 `haven.db-shm` 后重建，不能
运行时迁移或与新库混用。回滚本变更必须同时回到 schema v26，不能只回滚调用方。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```
