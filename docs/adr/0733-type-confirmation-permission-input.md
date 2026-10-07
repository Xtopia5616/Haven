# ADR 0733：确认命令复用权限输入 enum

## 状态

已采纳并实施。

## 背景

`PermissionEffect`、`PermissionScope` 与 `PermissionTarget` 是 Common 中的闭合权限词汇，分别表达 allow/deny、once/session/always 与 operation/group/tool。`resolve_confirmation` 仍接收三个 `String`，由 App handler 自行 trim/lowercase/parse；因此 generated command request 与 UI callback 将三个字段都暴露为开放字符串。

唯一生产 caller 是 shell `+layout.svelte::handleConfirm`。它消费 `ConfirmationDialog` 发出的 `ConfirmationDecision`（`stepId`、`approved` 和可选权限值），补齐默认值，再构造 `ResolveConfirmationRequest`。确认弹窗的所有按钮只产生上述规范值。UI 以 step id 做 in-flight 去重；command error 返回 `false`，pending dialog 可以重试。

Rust 用 `owner` 与 request ID 选取唯一 pending confirmation，再根据 owner 分流 AppCommand 与 Agent session/scheduled owner。Receipt 和能力目标仍在 backend 校验，target 只能选择 capability 自身或其祖先。Once 不写授权；Session grant 在唤醒执行者前 durable commit；Always grant 经配置运行时协调 gate 持久化。Expired/stale 是明确 command response；授权写入、确认仲裁和失败恢复均不由 renderer 决定。`PermissionTarget::parse` 仍由 Memory session-grant 持久读取边界使用，本 ADR 不删除它。

## 决定

- `resolve_confirmation` 参数改为 Common `PermissionEffect`、`PermissionScope`、`PermissionTarget`，删除 handler 私有的字符串解析 helper。
- IPC generator 从 Rust enum 导出 `PermissionEffectInput`、`PermissionScopeInput`、`PermissionTargetInput`，生成的 request 是 Tauri 权限字段类型的唯一 TS wire contract。
- `ConfirmationDecision` 保留为 UI callback view，继续包含 `stepId` 和 `approved`；其权限字段引用 generated input types，shell 仍负责默认值及转为 `ResolveConfirmationRequest`。
- `ConfirmationDialog` 的 target options、pending target 和 `decide` parameters 使用生成 enum 类型；按钮和值域保持不变。
- 不改变 owner/request 路由、receipt 校验、capability ancestry 限制、expiry 处理、授权写入顺序或持久化结构。

## 替代方案

- 保留 handler string parse：拒绝。三个值域已有 Common owner，handler 与前端 request 不需要重定义开放 vocabulary。
- 让 `ConfirmationDecision` 直接成为 Tauri request：拒绝。它是子组件到 shell 的 renderer view，携带 `stepId` / `approved`，缺少 wire owner 语义且与后端 `request_id`/`owner` shape 不同。
- 删除 `PermissionTarget::parse`：拒绝。Memory 的 session authorization 持久读取仍需要从数据库文本严格解析 `PermissionTarget`。

## 影响与验证

Tauri request 静态类型从三个 `string` 收窄为 generated permission input enums；规范 JSON 值不变。空白或非规范大小写值不再由命令接受，UI 当前按钮不会生成这些输入。无数据库、配置或授权生命周期变化，无需重置数据。无效 Tauri enum 输入仍以 invoke error 返回；shell 保留 pending interaction 并允许重试。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check` / `test:run`（125 files / 992 tests）/ `build`、`scripts/check-ipc-contracts.ps1`（80 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-adr-index.ps1`（716 ADRs）与 `git diff --check` 均通过。

## 回滚

如回滚，必须同时恢复 Rust handler 字符串参数与 parser、generated command request、`ConfirmationDecision` / `ConfirmationDialog` 类型、IPC 文档和命名记录。无需回滚持久数据。
