# ADR 0664：具名 SessionRun 准入等待项

## 背景

`SessionSupervisor` 为等待直接 SessionRun admission permit 的调用维护 process-local 注册表。每项原本是 `(usize, CancellationToken)`：ID 用于仅注销当前调用，取消令牌供 interrupt、delete、end 和 clear 唤醒阻塞中的容量等待。注册表字段、操作方法及 dispatcher cleanup guard 都只称 waiter，没有标明 admission scope；tuple 还要求按位置记住两个职责。

## 决定

- 用 `DirectSessionRunAdmissionWaiter { waiter_id, cancellation }` 表达一项等待登记。
- 注册表、ID 计数器、register/unregister/cancel 方法及 cleanup guard 同步使用 `DirectSessionRunAdmission` 名称。
- 维持每个 Session 可有多个并发等待、按 ID 独立注销、删除时移出并取消全部等待项的行为。
- 该状态仅在进程内，不改变持久数据、IPC 或配置，无需重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-agent --all-targets`
- `cargo clippy --locked -p haven-agent -- -D warnings`
- `scripts/check-adr-index.ps1`

`cargo clippy --locked -p haven-agent --all-targets -- -D warnings` was also attempted and reports existing lint failures in test targets outside this slice; those warnings are not part of this refactor.

## 回滚与重置

回滚仅恢复 process-local tuple 和旧方法名，不涉及持久数据或用户配置。
