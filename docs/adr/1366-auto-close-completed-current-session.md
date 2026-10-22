# ADR 1366：自动关闭已完成的当前会话

## 状态

已采纳 — 2026-10-22

## 背景

当前聊天页在会话进入终态 `completed` 后仍显示该会话。用户结束会话后，页面继续保留终态 transcript 和 composer 上下文，下一条输入仍关联已结束会话，需要手动创建新会话。

## 决定

1. 收到 `completed` 生命周期事件时，若该会话仍是聊天页当前会话，先完成终态流清理，再清除当前会话选择并显示空白的新会话界面。
2. 自动关闭只影响页面选择和进程内终态投影；持久会话与历史 transcript 保留，用户可以从历史中重新打开。
3. 完成通知继续按现有设置发送。错误、暂停以及 `turn_completed` 暂停状态不自动关闭。
4. 自动关闭设置 fresh-session intent，避免同一页面稍后的会话列表刷新立即自动选择旧会话；用户明确切换历史会话或提交新消息后，沿用现有流程清除该 intent。
5. 非当前会话完成时不更改聊天页选择。

## 考虑过的方案

- 删除已完成会话：拒绝。自动关闭是页面导航行为，不应删除用户历史。
- 暂停或错误时也自动关闭：拒绝。这些状态仍需用户查看原因或继续处理。
- 自动完成轮次时关闭：拒绝。`turn_completed` 表示会话可继续，只结束当前轮次。

## 后果

- 结束当前会话后，聊天页回到空白会话入口，用户下一条消息将创建新会话。
- 重新打开已完成历史时，transcript 仍从持久历史恢复。
- 不改变 Rust、Tauri IPC、数据库 schema 或事件 payload，因此无需数据库重置。

## 验收

- UI lifecycle 测试覆盖当前会话完成后关闭、非当前会话完成不改变当前选择，以及暂停的 `turn_completed` 不触发关闭。
- 运行 UI 检查、相关测试和生产构建。

## 验证结果

- `corepack pnpm run check`：通过，0 errors、0 warnings。
- `corepack pnpm exec vitest run src/lib/chatSessionEventHandlers.test.ts src/lib/chatEventController.test.ts --maxWorkers=1`：通过，2 个测试文件、16 项测试。
- `corepack pnpm run build`：通过，生产构建与 CSP manifest 生成完成。
