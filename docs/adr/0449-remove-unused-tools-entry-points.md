# ADR 0449：移除未使用的 Tools 入口

- 状态：Implemented
- 日期：2026-10-04
- 范围：`haven-tools` 中旧附件注册与独立授权输入 accessor
- 关联：ADR 0345、0384、0388、0403

## 背景

`ToolsManager::register_managed_assets` 仍将附件放入待认领的 ingress lease，但没有仓库调用方；当前 Agent 和 App 都通过 `register_managed_assets_for_session` 建立有明确 session owner 的 lease。录音 staging 仍直接使用 `ManagedAssetRegistry::register_under_root_pending`，所以该底层操作不能随旧 manager 入口一起删除。

`ToolsManager::get_authorization_input` 也没有调用方。实时执行与不可变 catalog 路径都使用 typed `AuthorizationRequest`，分别由 `get_authorization_request` 与 `get_authorization_request_from_snapshot` 提供；live `AuthorizationEngine` 仍保有 allow/deny/confirm 决策权。

## 决定

1. 删除无调用方的 `ToolsManager::register_managed_assets`。会话附件继续通过 session-scoped 注册；录音 pending asset 的显式 registry 路径保持不变。
2. 删除无调用方的 `ToolsManager::get_authorization_input`。canonical input 继续作为 typed `AuthorizationRequest` 的一部分交给授权 owner。
3. 不保留 source-compatibility wrapper。全仓生产与测试调用点均已核对；Haven 当前没有承诺稳定的 Rust 下游 API。

## 替代方案

- 保留 ingress lease manager API：它缺少 session owner，且没有仓库调用方，会重新暴露旧的 pending ownership model，拒绝。
- 保留独立 canonical-input accessor：调用方得到输入却没有同一请求的 operation policy，可能让 authorization boundary 被拆成两个查询，拒绝。
- 删除 pending registry primitive：录音 staging 仍需要它，且职责与 session attachment lease 不同，拒绝。

## 影响与验证

- 这是 `haven-tools` Rust source API 收窄；无数据库、配置、IPC、provider wire 或用户数据变化，无需重置。
- 验证所有 workspace 调用点仅引用当前入口，运行 Tools 测试与严格 Clippy；workspace 检查和测试覆盖依赖 crate 的编译。
- session-scoped attachment lease、录音 staging 与 canonical authorization request 的既有行为测试保持不变。

## 回滚

如需恢复 API，可恢复对应 facade 方法；不改变底层 asset registry、session ownership 或授权决策流程，无需数据重置。
