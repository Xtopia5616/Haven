# ADR 0658：具名 LLM stream attempt 输出处置状态

## 背景

LLM router 的 `StreamAttemptHooks` 用 `FnMut(bool)` 通知 provider attempt 开始；`true` 表示清除先前可见输出，`false` 表示保留。Agent 也把同一裸布尔值从 stream call 参数传给 LLM crate，并在 callback 中再次按 bool 分支。字段名和注释能解释调用意图，但 callback 自身的函数类型不携带这个语义。

## 决定

- 新增 `StreamAttemptOutputDisposition::{PreserveExisting, ReplaceExisting}` 作为 LLM crate 对外的具名状态。
- `StreamAttemptHooks`、`ActiveStreamHooks` 与 Agent `stream_llm_call` 全链路传递该 enum，不保留布尔重载或 alias。
- LLM 流回调类型分别命名为 `StreamChunkCallback` 与 `StreamAttemptStartCallback`。
- 普通 stream 首次输出使用 `PreserveExisting`；结构 retry 和 stream-rule guidance retry 使用 `ReplaceExisting`。
- retry 次数、partial checkpoint、前端 reset 事件及可见输出时序不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

代码回滚需将 enum 替换回 bool 并恢复 `FnMut(bool)`。本次只改 LLM/Agent Rust API，不触及 IPC、数据库或配置。
