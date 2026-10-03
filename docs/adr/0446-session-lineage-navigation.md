# ADR 0446：会话菜单中的 Agent 父子会话导航

- 状态：已采纳（2026-10-03）
- 范围：Agent spawn 会话的父/子关系读取与聊天页导航
- 关联：ADR 0442

## 背景

ADR 0442 将会话来源和 `parent_session_id` 持久化，并提供 SessionStore 的直接子会话查询；当时没有产品侧用途，所以没有把来源暴露到 UI。Agent 创建的 peer 现需要能从聊天界面查看并返回其父会话，也要能从父会话打开已创建的子会话。

## 决定

1. 新增只读 `get_session_lineage(session_id)` IPC，返回已持久化的父会话（如存在）及最多 50 个直接子会话。响应使用仅含菜单所需字段的应用 DTO，不序列化存储模型的 `origin`。父会话已删除时返回 `null`，保留 child 的历史来源记录，不级联创建虚假会话。
2. 聊天页会话切换菜单按需请求当前会话的 lineage，并呈现“返回父会话”和 Agent 子会话入口。选择后复用现有会话恢复与切换流程；该 UI 不承载 peer mailbox、运行状态或协作权限。
3. 命令使用 SessionStore 的 typed 读取边界，不读取 raw Database，不修改 Session、Messaging、Action 或 transcript 的生命周期。会话历史、resume 与 lineage IPC 共用排除持久 `origin` 的应用 DTO，避免存储模型字段隐式扩展 wire 契约。

## 替代方案

- 只提供存储与查询接口：用户仍无法从聊天页面找到已生成的 Agent 子会话，拒绝。
- 将 lineage 放入 Messaging registry：registry 只负责协作运行时状态，不能作为持久会话来源的另一真源，拒绝。
- 扩展所有 SessionInfo 与 session event：关系只在会话菜单打开时需要；新增单一按需读取命令可避免扩大每次会话列表和事件 payload，拒绝。

## 影响与回滚

新增一个只读 Tauri 命令及聊天会话菜单区域，无数据库、配置或数据迁移。子会话列表固定最多 50 条，按 SessionStore 的稳定顺序返回。回滚时删除该命令、UI 菜单区域、契约登记与本 ADR；不需要重置数据。

## 验证

- Session command 测试覆盖普通 parent、Agent 子会话、直接 children 和已删除 parent 的读取边界。
- UI 测试覆盖菜单展示父/子入口、切换回调及 flat command 参数。
- 运行 IPC 契约检查、前端类型检查和 UI 测试；完整门禁结果记录在交付说明中。
