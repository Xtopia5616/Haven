# ADR 1073：在 UI 提取 ToolRun 身份时校验 canonical ID

## 状态

已接受（2026-10-13）

## 背景

Session UI 从工具 observation 和后台任务结果注入文本中提取 `tool_run_id`，并用它关联 ToolRun 时间线、结果卡片和来源步骤。此前这些提取器只检查值是否为非空字符串，导致格式错误的文本也能成为 renderer 的实体身份。

## 决定

- 从 background observation、`schedule.set` observation 和 ToolRun result inject 中提取 ID 时，统一调用 `isCanonicalEntityId(value, 'toolrun')`。
- 无效或缺失的 ID 映射为 `null`；不得修补或规范化输入字符串，也不得将它用于 timeline 关联。
- 这是消费端身份边界校验，不改变 Rust producer、IPC、durable payload 或自由文本内容。

## 替代方案

- 只依赖 producer 正常生成 ID：不采纳，因为 observation 与 inject 文本在 renderer 中被重新解析，消费端仍需验证其身份形状。
- trim、补前缀或重写 UUID：不采纳，因为变换会创建不同于 producer 原始值的实体身份。

## 影响

规范 `toolrun-*` 行为不变；畸形身份不再参与 renderer 关联，缺失身份继续使用现有无身份 fallback。无数据库或 IPC 迁移。

## 验证

- streaming 单测覆盖规范 background/schedule ID、无效 ID 与 result inject。
- UI 类型检查和全量 UI 测试通过。

## 回滚与数据重置

无持久化格式变更，不需要重置。若 ID 格式变更，应先更新统一 canonical ID contract，再更新这些消费端校验。
