# 0952：限制 ToolRun lifecycle payload 的跨 crate 构造

## 状态

已接受并实现（2026-10-11）。

## 背景

`ToolRunLifecyclePayload` 是 `haven-tools` 产生并交给 App 投影的事件 contract。生产构造点都在 Tools 内部，但 crate root 公开了 `new`，且 DTO 可由下游 struct literal 构造，允许 App 生产代码自行伪造 ToolRun lifecycle event。App 只有 projection tests 需要构造 fixture。

## 决定

- 将 payload 标为 `non_exhaustive`，阻止下游 struct literal 直接构造。
- 生产 `new` 收回为 `pub(crate)`，只由 Tools owner 创建 lifecycle payload。
- 为跨 crate projection tests 提供 `test-support` feature 下的 `new_for_test`；App 的两个测试改用该入口。生产构造与事件字段内容保持不变。

## 影响

Tools event sink 与 App projection contract、IPC shape、ToolRun 状态和持久化均不变。普通生产构建不再包含任意创建 payload 的 API；无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `git diff --check`

## 回滚

若新增真实的跨 crate lifecycle payload producer，应由 Tools 提供带领域语义且能保持事件不变量的 API；不要重新开放通用生产构造器或暴露实现模块。
