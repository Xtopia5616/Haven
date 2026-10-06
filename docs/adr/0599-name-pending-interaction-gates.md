# ADR 0599：命名 pending interaction gates

## 状态

已采纳并实施。

## 背景

Ingress 在写入用户消息前读取当前 session 的 pending Confirm 与 Ask 状态，以决定新输入是 Answer 还是 FollowUp。`SessionSupervisor::pending_interaction_gates` 原返回 `(confirm_pending, ask_pending)`，调用点必须记住字段顺序；无 active session 时也手工构造同序 tuple。

## 决定

1. 增加 `PendingInteractionGates { confirmation_pending, ask_pending }` 作为状态快照。
2. 无 actor 或无 active session 时返回 `Default`，两项均为 false。
3. Ingress 按具名字段判断：仅当 session 非 Running、有 pending Ask 且没有 pending Confirm 时将输入路由为 Answer。

## 替代方案

- 保留两个 bool 并只改局部变量名：拒绝，actor/supervisor 契约仍靠 tuple 位置编码，默认分支仍会重复顺序。
- 将两个 gate 合成一个 `has_pending_interaction`：拒绝，Confirm 与 Ask 对 ingress 的路由优先级不同，不能压成一个事实。

## 影响与验证

- 仅变更 Agent 进程内返回类型，不改变 interaction replay、消息持久化、Confirm 优先级或 Answer/FollowUp 语义。
- 命名路线图仍保持 Active，其他 Rust crate、UI、IPC/event 与配置持久名继续逐域审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`、`cargo test --locked -p haven-agent`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(confirm_pending, ask_pending)` 并还原 tuple 解构；无持久化迁移。
