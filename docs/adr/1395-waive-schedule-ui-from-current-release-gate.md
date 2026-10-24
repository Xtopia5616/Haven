# ADR 1395：将 Schedule 真实 UI 验收从本次发布 Gate 豁免

## 状态

已按用户明确要求采纳；仅调整当前 Windows 发布 Gate 的验收范围。

## 背景

路线图 §5.1 原先要求当前安装包验证 Schedule 计划的真实 UI 流程。当前本机 mock provider fixture 只返回普通回复或安全的 Shell tool call，不会返回 Schedule tool call；用户要求本次 Gate 清除 Schedule 真实 UI 验收项。

## 决定

- 将 Schedule 计划创建、触发与结果展示的真实 UI 验收从当前 Windows 发布 Gate 的必验项中移除。此决定是范围豁免，不是 Schedule UI 已通过的证据。
- 仅豁免 Schedule 真实 UI 流程；后台 ToolRun 执行、工具确认路径、会话恢复与 rollback 等其他 Gate 项仍保留。
- 保留现有 Schedule 领域实现与自动化测试。本次范围变更不修改代码、fixture 或 Schedule 契约，也不把任务页空态当作 Schedule 验收。
- 后续发布如需 Schedule 真实交互证据，应在新的发布范围中重新纳入并使用支持 Schedule tool call 的隔离 fixture。

## 影响与验证

- 本次没有从当前安装包创建或触发 Schedule；既有页面仅显示 0 个定时计划，不能视作交互验收。
- 无代码、配置、数据库或安装产物变更。路线图 §5.1 与 ADR 1393 记录该范围豁免。

## 回滚

后续发布可通过新的 ADR 与路线图变更重新加入 Schedule 的真实 UI 验收；不得把本次范围豁免反向表述为测试通过。
