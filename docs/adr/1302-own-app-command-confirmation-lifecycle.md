# ADR 1302：由 App confirmation owner 管理 AppCommand 生命周期

状态：已接受并实施（2026-10-17）。

## 背景

AppCommand 的 pending confirmation map 存在 `AppState`，但入队、事件发布与超时分散在通用 `commands` 模块，resolve 仲裁和授权接受位于 session commands，批准后的 Skill、admin 与 sandbox 操作又由后者继续执行。多个 handler 可以直接锁定 map。Native admin request 的授权、执行及 UI 收尾也混在通用命令模块中。

这种分布模糊了 AppCommand pending 状态的唯一 owner，并让 Session、ScheduledToolRun 和 AppCommand 三种 resolve 路由与 AppCommand 实现细节耦合。

## 决定

1. 私有 `commands::confirmation` 模块提供 `AppConfirmationOwner`，由 `AppState` 持有。pending map、注册、事件发送失败清理、expiry、owner/request 身份仲裁、授权接受和异步 continuation 均归此模块；handler 不再直接访问 map。
2. `resolve_confirmation` 保留为跨 owner router。Session 和 ScheduledToolRun 继续经 Agent executor 的现有 owner-specific resolver；只有 AppCommand 委托给 `commands::confirmation`。
3. `commands::admin_adapter` 拥有 native admin request 的 App command authorization、执行和延迟确认后的 UI 收尾。权限策略仍由 Tools `AuthorizationPort` 与 `AuthorizationEngine` 决定，App 不复制策略。
4. 保持既有确认 ID、失效与 expiry 结果、拒绝/允许语义、永久授权先持久化后发布、操作输入和 receipt 后端隔离，以及事件顺序。持久 Session grant 和 `interaction_resolved` 的 append/retry 生命周期仍由 Agent 与 Memory owner 管理，本切片不改变该契约。
5. AppCommand pending request 仍只存在于进程内；不新增持久状态、Tauri payload、数据库字段或新的授权入口。

## 替代方案

- 继续让 `AppState` 暴露 map：拒绝。多个 command handler 能任意修改 pending 状态，不符合唯一 owner 约束。
- 把 AppCommand 仲裁并入 Agent executor：拒绝。AppCommand 没有 Session/ScheduledToolRun 的 Agent 生命周期；跨 owner command 只保留薄路由。
- 在 App adapter 中实现授权策略：拒绝。授权 decision 与实时撤权继续归 Tools。

## 影响与恢复

此变更只拆分 App 私有模块，wire contract、数据库、inbox 和用户可见授权行为均不变，无需重置数据。回退时恢复旧模块布局即可；无用户数据转换。

## 验收

- `cargo fmt --all -- --check` 通过。
- `cargo test --locked -p haven-app-binary`：253 passed。
- `cargo clippy --locked -p haven-app-binary -- -D warnings` 通过。
- `scripts/check-ipc-contracts.ps1` 通过：85 个 handler 的 Rust 注册、TypeScript、security metadata、文档与 UI/non-UI disposition 一致。
- 既有 App confirmation 测试覆盖 owner/request ID、stale/expiry、retryable failure 保留 pending、并发 resolve 只产生一个终态，以及拒绝/允许授权路径。
