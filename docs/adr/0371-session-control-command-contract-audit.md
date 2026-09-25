# ADR 0371：Session control command contract audit

- 状态：已采纳（2026-09-26）
- 基线：HEAD `527b2a8`；开始时工作区干净
- 范围：`continue_session`、`interrupt_session`、`end_session`、`rollback_session`、`resolve_confirmation` 的 Rust/TypeScript request contract 与 UI 直接调用边界
- 关联：ADR 0313（ChatController）、0315（chat event registration）、0349（session terminal cleanup）、0350（session UI mapping）、0370（diagnostics command contract）

## 背景与审计

Rust handler 与 Rust/TypeScript command registry 已登记这五个命令，响应均为 unit/`void`。前四个命令的执行与 reducer/通知顺序由 `chatController.ts::ChatController` 编排；`resolve_confirmation` 由 `+layout.svelte` 处理，因为应用 shell 的确认弹窗在非聊天工作区仍必须可见。源码中没有第二个 UI 调用者。

`SessionIdRequest` 已定义，但 `RollbackSessionRequest` 和 `ResolveConfirmationRequest` 只有 registry 名称，没有对应的 TypeScript 字段 DTO。ChatController 通过 `unknown` 参数的通用 invoke dependency 传这些请求；布局的确认请求也没有命名类型。IPC 脚本此前没有把这组 Rust handler 参数与 renderer 字段逐项对照，也没有锁定其直接调用 owner。

rollback dialog 的 view state 将 `stepNumber` 表示为 `number | null`，Rust handler 却要求 `target_step: u32`。打开 dialog 的入口已先确认步骤存在；原 confirm callback 没把这一 UI 前置条件带到命令类型中。另有全局 `tauri.ts::invoke` 返回 `Promise<any>`，但这五个命令的 unit 结果都会被忽略；request payload 本身没有裸 `any`，ChatController 的通用 invoke 参数是 `unknown`。

没有同形输入的重复 mapper：ChatController 根据 rollback dialog 状态一次构造扁平 `sessionId`、`targetStep`、`pause` 和 `targetMessageId`；布局根据 shell confirmation selection 一次构造 `stepId`、`effect`、`scope` 和 `target`。两者处理不同请求。`resumeInteractions` 负责 resume response 的兼容归一化，live interaction event mapper 负责事件映射；它们不映射这组 command requests，也不纳入本 ADR。

## 决定

1. 在 `contracts/commands.ts` 为 rollback 和 confirmation 补齐命名 request DTO；字段使用 renderer 的 camelCase，与 Rust/Tauri snake_case handler 参数逐项对应。可选 Rust `Option` 参数在 TS DTO 中保持 optional/null 语义。
2. ChatController 的四个直接 invoke 使用 `satisfies` 约束到 `RollbackSessionRequest` 或既有 `SessionIdRequest`。页面 confirm callback 窄化 nullable dialog state 后传入必需的数字步骤；layout 为确认 in-flight 集合和 request 对象补上明确类型。
3. 保持 ChatController 为聊天页会话命令的单一编排 owner，保留 direct invoke，不增加只转发参数的 helper。`resolve_confirmation` 留在 layout shell owner，不迁入 ChatController，不增加薄 command wrapper。
4. IPC contract check 对照五个 handler 的 Rust 参数、Rust/TS registry、命名 request 字段和 void response，并禁止其他 UI 文件直接 invoke 这些命令；同时固定 confirmation 的本地 resolved、IPC、stale toast、通用 error 与 in-flight cleanup 顺序。回归测试固定 request names/responses、四条 ChatController 参数、rollback/continue in-flight 防重，以及既有失败路径。

## 兼容性与影响

Command names、扁平 Tauri 参数名、renderer 传递值和所有请求顺序保持。rollback 的 user/action 分支、continue 的锁与恢复顺序、end 的失败选中状态、interrupt 的 pending/成功通知，以及 confirmation 的同步本地 resolved 投影、in-flight 去重、allow/deny 默认值、stale 提示和一般错误报告均保持。

confirmation request 仍以 `stepId` 传入 Rust `step_id`；session 请求仍以 `sessionId` 传入 Rust `session_id`。请求和 unit 响应没有运行时 mapper；不会改变 live/resume interaction normalizer、ask/input 决策、事件去重、通知 owner、DB/ID/X12、UI 行为或全局 Rust→TypeScript codegen。当前通用 `tauri.ts::invoke` 仍是动态返回边界；本组命令不消费其 `void` 结果。

## 验证

使用 Node.js 24.20.0 与 pnpm 11.24.0：

```sh
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
pwsh -NoProfile -File scripts/check-ipc-events.ps1
```

无 Rust 代码变化，不运行 Rust gates。

## 回滚

回滚本提交并删除本 ADR 与索引/路线图/架构记录即可；无需数据库、配置或用户数据重置。
