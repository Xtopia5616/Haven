# ADR 0208：TurnEngine 产出 EffectBatch

## 状态

已接受（2026-09-22）。ADR 0209 将工具批次的 durable 提交纳入同一应用器。`sessions.react_state` 仍只为测试兼容保留，schema 为 v27。

## 背景

`TurnEngine` 过去以未分组的 turn 返回值直接调用 transcript writer、消息投影和暂停
生命周期 helper。模型响应的分类和副作用执行因此共享同一层，新增终态分支时容易
重复写入或在 UI 通知先于 durable commit 时产生顺序漂移。

## 决定

- 单次 turn 返回 `EffectBatch`，由 ordered `TurnEffect` 和 `TurnControl` 组成；
  turn-end 至少把 transcript、UI-only message projection 和 pause boundary 放入
  同一批次。
- `RunEngine` 是批次的唯一应用方。它按声明顺序执行 durable transcript，再执行
  投影/UI 和生命周期边界；批次执行失败时继续走现有 fail-closed 错误路径。
- provider stream 的增量 chunk、请求准备阶段的 media plan、工具本身的外部副作用和确认准入仍保留在各自
  runtime boundary；它们不是可延迟的 turn-end projection，不复制到第二套 writer。
- 本地 turn-end inject 排在 final transcript 之后。注入成功则保存 branch point 并继续 run，否则才应用 pause。
  取消边界和会话错误通知也进入同一批次，并且排在已接受的 transcript effect 之后，避免提前返回丢弃它们。
- `session_events` 仍是唯一恢复 authority；`EffectBatch` 不序列化、不写入数据库，
  只表示一次进程内 turn 的副作用计划。同一变更删除已无生产读写的 `react_checkpoints` 表，schema 升至 v27；
  `sessions.react_state` 列仍只为测试 fixture 保留，留到下一次 schema reset 删除。

## 影响与回滚

普通文本和显式 final-answer 的终态不再由 `turn.rs` 直接写入持久化/UI。工具批次作为
一个 effect 交给 run driver 执行，其内部 observation 写入仍留在工具执行边界。
删除 `react_checkpoints` 需要按 `docs/release-and-reset.md` 重置旧数据库；IPC 不变。
回滚代码时也要恢复 schema v26，不能只回滚调用方。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```
