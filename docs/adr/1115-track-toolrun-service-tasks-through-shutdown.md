# ADR 1115：统一 ToolRunService 后台任务所有权与 shutdown drain

## 状态

已接受（2026-10-13）

## 背景

ToolRunService 的 process runner、live output emitter、scheduled timer/dependency watcher，以及 background、scheduled 和 malformed-row persistence retry 都通过裸 `tokio::spawn` 启动。它们观察 root cancellation token，但 Service 不持有 join owner。`shutdown()` 发出取消后只请求 background 取消并返回；子进程树清理、终态持久化和 task destructor 可能仍在运行。并发第二次 `shutdown()` 也会在第一次完成 drain 前返回。

ToolRun admission、scheduled mutation、session cleanup 和 terminal retry 的现有语义已建立。本 ADR 仅收束 Service 内异步任务生命周期，不合并 background 与 scheduled 状态机。

## 决定

- ToolRunService 以 `tokio_util::task::TaskTracker` 登记所有 Service-owned spawn：background/scheduled admission 的耐取消持久化工作、process runner、output emitter、scheduled timer/watcher，以及 background/scheduled/quarantine persistence retry。调用者原本 await 的短持久化 worker 继续 await；detached worker 由 tracker drain。
- `shutdown()` 先取得独立 shutdown gate，再取得现有 `spawn_gate`。在 admission gate 下设置 shutdown 终态、取消 root token，并快照仍 Running 的 ToolRun；释放 admission gate 后请求取消，然后关闭 TaskTracker 并等待它变空。
- admission 在 shutdown 获得 `spawn_gate` 之前开始的操作可以完成注册，避免已经写入 durable row 却没有对应 worker。gate 之后的 schedule/spawn/restore/fire/terminal completion 不再启动新工作。由正在退出的 tracked task 派生的终态 retry 子任务仍被 TaskTracker 计数，parent 未完成时 drain 不会提前归零。
- 并发 `shutdown()` 调用经 mutex 串行化。后续调用等待前一 drain；若前一调用 future 被取消，后续调用检查 tracker 的 closed/empty 状态并继续未完成的 drain。
- root token 停止 waiting schedule 的内存 timer/watcher，但不取消其 durable `Waiting` row；下一次 `restore_pending` 可重新挂载。终态持久化 retry 沿用原有运行期间的重试策略；shutdown cancellation 会结束 retry worker，未提交的 durable Running 状态由既有 `restore_after_restart` 恢复流程修复。
- 不修改 ToolRun wire payload、schema、background/scheduled 区分或执行权 claim 语义。

## 替代方案

- 仅取消 root token 会让调用者无法知道 runner 是否已经清理子进程并写入终态。
- 继续使用裸 JoinHandle 会要求 Service 自己保存、回收并在动态重试创建子任务时实现 JoinSet drain；TaskTracker 已提供无 completed-handle 堆积的 group wait 语义。
- shutdown 时删除 Waiting schedule 或无限等待 terminal retry 会改变既有持久恢复语义，因此不纳入本切片。

## 影响

ToolRunService shutdown 返回时，所有登记的服务任务及其 future destructor 均已完成。background 进程取消及终态提交位于 join 边界内；临时数据库错误仍按已有恢复策略处理。Waiting schedule 保持持久化并可在下次启动恢复。

启用 `tokio-util` 的 `rt` feature 以使用 TaskTracker，无 schema 或配置重置要求。

## 验证

- `cargo test --locked -p haven-tools`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- IPC contract、ADR index 与 `git diff --check`

测试覆盖真实 background process shutdown 后已终止且 Service task group 为空、scheduled timer drain 后 Waiting 项仍保留、并发 shutdown 共享 drain，以及关闭后的 schedule admission 拒绝。

## 回滚

不得恢复无 join owner 的 ToolRun runner、timer 或 retry worker。若取消等待 drain，必须重新证明进程退出、terminal persistence 和 concurrent shutdown 的完成边界，并保留 Waiting schedule 的恢复契约。
