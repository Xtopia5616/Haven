# ADR 1334：将文件沙箱约束限定到工作区写入操作

- 状态：已接受
- 日期：2026-10-20

## 背景

`open_external` 将浏览器 URL 放在 `target` 字段中，并作为 `ExternalEffect` 进入统一授权。共享路径沙箱把所有非只读效果都当成文件写入；在 `WorkspaceWrite` 下，URL 因而被误判为工作区外的写入路径，在权限确认之前直接拒绝。`ReadOnly` 也会把外部效果误判为文件修改。

## 决定

- `ReadOnly` 和 `WorkspaceWrite` 的本地文件系统写入边界只约束 `OperationEffect::WorkspaceWrite`。
- `ExternalEffect` 继续由其网络策略、执行边界、权限规则和所需确认控制。它不会因目标路径位于 `writable_roots` 外而被拒绝；这不改变 `Deny` / `Restricted` 对 opaque 网络能力的硬拒绝，也不改变 `Ask` 的确认行为。
- 其它本地文件路径规则（如配置的 `allowed_paths`）继续按现有策略执行；此改动只收窄只读沙箱和可写根的适用效果。

## 替代方案

- 将 `open_external` 统一标记为只读：拒绝，会抹掉打开外链带来的外部效果分类，并可能弱化 Plan 与普通权限策略。
- 允许任意效果绕过路径策略：拒绝，真实的 `WorkspaceWrite` 文件操作仍必须受只读模式和可写根约束。

## 影响与恢复

`open_external` 在 `WorkspaceWrite` 与 `ReadOnly` 下均可进入现有授权流程；`WorkspaceWrite` 文件操作的边界保持不变。不涉及 IPC、持久配置、数据库或用户数据，无需重置。回滚时恢复路径沙箱对所有非只读效果的旧判断，并移除此 ADR 与回归测试。

## 验证

- 回归测试使用 opaque `open_external` URL、`NetworkPolicy::Ask` 和不包含目标的 `writable_roots`，验证 `WorkspaceWrite` 与 `ReadOnly` 都返回 `RequiresConfirmation`。
- 验证目标 crate 的格式、相关测试、locked 编译与严格 Clippy；共享安全策略影响若扩大到跨 crate 再升级为 workspace 门禁。
