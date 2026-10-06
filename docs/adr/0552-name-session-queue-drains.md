# ADR 0552：Session 队列消费入口明确使用 drain 动词

## 状态

已采纳并实施；Rust workspace 格式、编译、严格 Clippy 与测试通过。

## 背景

`SessionSupervisor::get_follow_ups` 与 `get_steering` 都会找到指定 session 的 actor 并调用 `drain_follow_ups` / `drain_steering`。这两个操作取回并从 owner 队列移除待处理条目；紧接着再次调用会得到空集合。公开方法名 `get_*` 却像只读查询，隐藏了队列消费副作用。

## 决定

1. 将 `SessionSupervisor::get_follow_ups` 重命名为 `drain_follow_ups`。
2. 将 `SessionSupervisor::get_steering` 重命名为 `drain_steering`。
3. 同步更新 Agent 内部调用点与测试名，不保留旧 Rust API 别名。
4. 命名规范补充 `drain` 表示取出并清空集合/队列；保留队列容量、优先级、所有权、错误与消费时机。

## 替代方案

- 保留 `get_*`，仅依赖实现细节理解消费语义：拒绝。方法调用者应从入口名看出其清空队列的副作用。
- 改成 `take_*`：拒绝。底层 actor 已使用 `drain_*`，且操作按整个队列批量取出并清空，`drain` 更准确。
- 改变队列消费方式：拒绝。本次只澄清命名，不改变消费模型。

## 影响与验证

- 改动触及 Agent `SessionSupervisor` Rust API 与调用方/测试；不改变 IPC、持久化、队列数据或运行时行为。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1` 与 `git diff --check`。

## 回滚

恢复旧入口名及调用点，并移除此 ADR、命名规则与路线图条目；无需数据或配置重置。
