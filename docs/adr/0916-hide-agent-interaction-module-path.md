# 0916：统一 Agent Interaction crate 根导出路径

## 状态

已接受并实现（2026-10-11）。

## 背景

Agent 在 crate root 已 re-export 全部跨 crate 使用的 Interaction 类型和 `replay_session_interactions`，但仍将实现模块声明为 `pub mod interaction`。这给每个符号提供了重复的公开路径。全仓搜索只找到 App 会话命令一处继续使用 `haven_agent::interaction::InteractionStatus`；其他 App 消费者和工具代码都通过 crate root 导入。

## 决定

- 将 Agent `interaction` 模块改为私有实现模块，保留 `InteractionDetails`、`InteractionEnvelope`、`InteractionKind`、`InteractionOwner`、`InteractionRequest`、`InteractionStatus` 与 `replay_session_interactions` 的 crate-root re-export。
- 把 App 唯一的嵌套模块路径改为根导出的 `InteractionStatus`。
- 保持 `crate::interaction` 作为 Agent 内部实现引用路径。Interaction 生命周期、授权 receipt、durable event replay、IPC DTO 与用户行为不变。

## 影响

这是 Rust crate 的源代码路径收敛；不新增、不删除根 API，也不保留重复的 `haven_agent::interaction::*` 外部入口。没有持久化、schema、IPC、权限或运行时行为变化，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

全仓路径搜索确认 App/Tools 没有其他 `haven_agent::interaction::*` 消费；工作区编译覆盖 crate 内部访问和保留的根导出。

## 替代方案

- 同时保留根与模块两种导出路径：拒绝；同一 Interaction 类型有两个地址，没有额外能力且会诱导调用方继续依赖实现模块。
- 删除 crate-root re-export，只保留 `interaction::*`：拒绝；这会保留实现模块作为公共入口并要求全体消费者改用更深路径，不能改善 owner 边界。

## 回滚

恢复 `pub mod interaction` 与 App 的嵌套路径即可；没有数据恢复步骤。
