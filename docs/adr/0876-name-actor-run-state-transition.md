# ADR 0876：区分 Session admission 与 Actor 运行态标记

## 状态

Accepted — 2026-10-10

## 背景

`SessionDispatcher::begin_direct_session_run(session_id)` 获取全局运行容量、注册并撤销 admission waiter、校验 Actor 实例与生命周期、持久化 `Running` 状态，并返回持有容量 permit 的 `DirectSessionRunLease`。它随后调用 `SessionActorHandle::begin_direct_session_run()`。

Actor 方法并不执行 session admission，也不创建 lease；它只通过 Actor mailbox 检查本地 `running` 位和终态，再设置该位并重置运行取消 token。两个层次使用相同完整动词，掩盖了 admission 编排与 Actor 局部状态转移的差异。

## 决定

- 将 Actor 方法和 mailbox command 改名为 `try_mark_direct_run_active` / `TryMarkDirectRunActive`，准确表达其局部、条件式状态转移。
- 保留 Dispatcher 的 `begin_direct_session_run`，它仍是 direct run admission 与 lease 生命周期的唯一 owner。
- 不合并两个入口，不移动容量、持久状态或 lease 逻辑，也不改变 Actor 的运行标记、取消 token 和失败语义。

## 影响与兼容性

本次仅重命名 Agent crate 内部方法与 Actor command。无 IPC、配置、持久化或外部 API 变化，无需重置；不保留旧名称 alias。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`，以及新 ADR 文件的 Prettier 检查。
测试套件未运行。

## 回滚

若未来 Actor 接管完整 admission 生命周期，应先定义唯一状态 owner 和 lease 责任，再重命名 Dispatcher API；当前不能因相邻调用链而合并这两层。
