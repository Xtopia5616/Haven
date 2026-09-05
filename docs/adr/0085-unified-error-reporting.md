# ADR 0085：统一错误上报、日志与前端通知

## 背景

前端页面长期各自拼接错误文案、选择 toast 时长，并在不同上下文下重复或遗漏日志。`unknown` catch 值还可能被直接转成 `[object Object]`；未处理的异常没有统一的用户反馈。后端虽然已有 `log_err`，但部分命令辅助路径仍可绕过它。

## 决定

- 前端以 `errorHandling.ts::reportError` 作为可恢复错误的组合入口：`formatError` 负责单行、限长文案，`logger` 负责带上下文的日志，`addNotification` 负责 error toast。
- `tauri.ts::invoke` 在 IPC 边界记录一次命令失败；已记录的错误由页面 catch 通过 `log: false` 只补用户反馈，避免重复 ERROR。
- `addNotification` 使用 `NotificationType` 和按语义统一的默认时长（info 3s、success 3s、warning 4s、error 5s）。错误 toast 统一使用 `alert` 语义、内容驱动高度和可换行文本。
- 根布局注册 `error` 与 `unhandledrejection` 兜底处理，日志上下文固定为 `global`，只显示不泄露内部细节的通用提示。
- 后端 `log_err` 保留稳定的两行消息前缀，同时增加 `command` / `error` 结构化字段；返回值和日志字段均使用单行、限长、已脱敏文本，命令共享辅助路径的失败也必须通过该入口。

## 替代方案

- 只统一 CSS：不能解决错误文案、日志上下文和重复记录问题。
- 只在每个页面补 `try/catch`：仍会持续产生实现漂移，也接不住未处理异常。
- 让后端把完整错误直接返回 UI：会把内部路径、网络细节或 provider 诊断泄露到用户界面。

## 影响与回滚

本次改变错误 toast 的默认时长、可访问语义、长文案布局和前端日志上下文，不改变 IPC 成功载荷、数据库或用户配置，不需要数据重置。回滚代码与本 ADR 即可恢复旧的分散处理方式。

## 验证

- `cd ui; corepack pnpm run check`
- `cd ui; corepack pnpm run test:run`
- `cd ui; corepack pnpm run build`
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
