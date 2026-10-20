# ADR 1337：对话切换列表只保留可继续会话

## 状态

已采纳（2026-10-20）。

## 背景

聊天页的紧凑会话切换器合并运行时会话与最近历史，因此已完成的会话会继续出现在切换列表中；宽屏会话栏虽隐藏已完成项，但 reducer 仍保留活动会话供结束提示使用。显式打开已完成历史时，Agent 只把 actor 的状态改为内存中的 `paused`，数据库仍保留 `completed`，所以应用重启后该会话再次显示为已完成。

## 决定

1. 对话切换列表展示当前运行时的非终态会话，以及历史中的 `paused` 和 `error` 会话。完成事件后的 reducer 状态优先于可能过期的历史快照，已完成会话立即从切换列表消失。
2. 已完成历史仍可从“历史”工作区打开；重新打开时将持久状态由 `completed` 改为 `paused`，发出对应的 Session lifecycle 更新，并恢复未投递输入。该状态不会自动进入 dispatcher，必须由用户继续或发消息才会运行。
3. 打开已完成历史后，UI 清除旧的完成提示并使用刷新后的暂停状态。错误历史仍按只读错误记录打开；任何显式调用 Agent reopen 打开 terminal 状态时都持久化为 `paused`。

## 替代方案

- 让完成历史继续留在对话切换列表：拒绝。历史页已有完整检索与打开入口，切换器只承担当前可继续会话的导航。
- 只在内存中改为暂停：拒绝。重启后状态会退回 `completed`，与用户刚刚重新打开的会话状态不一致。

## 影响与验证

- 不改变数据库 schema、IPC shape 或 transcript event；复用现有状态转换 `completed/error → paused`，无需重置用户数据。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check`、`test:run`（163 个文件 / 1359 项）与 production build 均通过；`git diff --check` 通过。
- 回归覆盖：完成条目在切换器消失、过期历史快照不能重新添加完成条目、暂停与错误历史仍可切换、完成提示在重开后清除，以及重开状态写回数据库为暂停。

## 回滚

恢复切换器对完成历史的展示，并将 `reopen_session` 的状态变化改回内存态；保留数据库当前状态写入不会破坏旧代码读取，但已经重新打开的会话会保持 `paused`，如需恢复终态应通过历史页的完成操作重新结束会话。
