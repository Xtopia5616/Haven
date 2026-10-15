# ADR 1207：通过 Tauri 续接一次性 sandbox 输出晋升确认

## 状态

已实现 Tauri command、AppCommand confirmation continuation 和 renderer 的 Once-only 操作样式。源 review 的产品 UI 与前台 Shell 生产接线仍待后续切片。

## 背景

ADR 1206 建立了 Tools 内部的 proof-bound 单文件晋升，但未提供用户确认续接。现有 app confirmation owner 可复用统一 interaction 生命周期；它原本面向无 session 的 Skills/Admin 命令，默认接受本次、会话或永久授权。

## 决定

- 新增 `promote_sandbox_output` Tauri command，先校验 canonical session/step，再通过 ToolsFacade 构造唯一授权请求并调用 `AuthorizationPort`。仅 `RequiresConfirmation` 可进入确认队列；Unexpected AutoApproved fail closed，policy block 仍拒绝执行。
- `UiConfirmationPending` 继续由 `AppCommand` owner 管理 interaction；完整 session-bound `AuthorizationRequest` 与 receipt 仅留在 backend pending registry。renderer 不获得 source path、执行 proof 或文件系统 capability。
- sandbox 晋升 action 的 `resolve_confirmation` 只接受 `PermissionScope::Once` 与 `PermissionTarget::Operation`；会话/永久 scope 以及更广 target 都保持 pending 并返回可重试错误。拒绝也仅作为本次决定，不创建授权规则。
- 接受后，App owner 用 request+receipt 调 `confirm_for_execution`，再把不透明 execution proof 交回 ToolsFacade。session id 留在授权请求中，因此现有 permanent deny、session deny、workspace 和路径规则照常复核。
- renderer 根据专用 tool name 显示 Once-only 操作提示，隐藏会话/永久授权及持久拒绝菜单；backend 限制是最终安全边界。
- confirmation summary 显示有限长度的目标路径、变更索引和“拒绝覆盖”行为；源 sandbox 路径不进入 prompt 或 IPC。
- 非 Windows 构建保留相同 IPC contract，但 command fail closed 并返回平台不可用。

## 替代方案

- 将 sandbox session grant 或永久 grant 写入授权配置：拒绝，单次晋升不应授权后续模型或其他调用写文件。
- renderer 自行执行文件复制或持有 proof：拒绝，confirmation owner 和 Tools execution owner 承担信任边界。
- 用通用 Admin continuation 执行：拒绝，晋升需要精确授权请求、confirm proof 与 Files policy 派生目标能力。

## 影响与迁移

新增一个 Tauri command 和一次性 UI 决策选项约束；没有数据库或配置迁移。当前 Shell 尚未把 AppContainer 输出转交 review owner，review UI 尚未提供 manifest 选择与目标选择，因此本 command 仍不能构成完整用户工作流。

## 验证

- App unit test 验证 SandboxOutputPromotion 只接受 Once/Operation，同时普通 Skill app confirmation 行为不变。
- Tools unit test 验证 confirmation summary 不泄漏 sandbox 源路径，且呈现目标和 no-overwrite 语义。
- UI test 验证专用 confirmation 只显示单次允许/拒绝。
- IPC 生成与契约门禁检查 command request/response 和 security inventory 同步。
- 完整 Windows confirmation→proof→文件晋升端到端验收留给 Shell 和 review UI 接线切片。

## 回滚

移除新增 command 与 action variant、Once-only dialog 分支及本文/索引/路线图更新。无持久数据或既有规则需要迁移。
