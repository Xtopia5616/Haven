# ADR 0139：工具页按 operation root 收束内置能力

## 状态

已接受（2026-09-13）

## 背景

0137 将内置能力的模型入口统一为独立的 `root.operation` view。工具页因此会把同一能力根下的
多个 operation（例如 `files.read`、`files.search`）显示成多张卡片，增加了浏览和管理成本；但每个
operation 的 Schema、风险、启用状态和正式名称仍然是独立契约，不能在后端合并。

## 决定

1. 工具页仅在展示层按 operation view 的 `root` 聚合为一张卡片；没有 operation view contract 的
   builtin 保持单卡片展示。
2. 聚合卡片展开后逐项展示原始 operation 名称、描述、风险、Schema 和启用状态，并继续以完整的
   `root.operation` 名称调用 `set_tool_enabled`。
3. 搜索和启用状态筛选作用于聚合卡片中的所有 operation；混合启用状态的能力在两种状态筛选下都
   保留，以免隐藏仍可操作的内部项。计数显示聚合后的 UI 卡片数。
4. 本变更不修改 Rust 工具目录、模型 prompt、权限 key、配置结构、IPC DTO、执行行为或历史数据。

## 替代方案

- 在后端返回一个聚合工具：会破坏 0137 的独立 model-facing operation view 及其安全边界，拒绝。
- 仅隐藏 operation 卡片而不提供展开详情：会丢失独立开关和 Schema，拒绝。

## 影响与验证

UI 新增纯函数分组/筛选测试和工具页聚合渲染测试；独立工具的现有展示与操作保持不变。
验证命令为 `cd ui; corepack pnpm run check`、`corepack pnpm run test:run` 和
`corepack pnpm run build`。

## 回滚

回滚本 ADR 对应提交即可；无需数据库、配置或 snapshot 重置。
