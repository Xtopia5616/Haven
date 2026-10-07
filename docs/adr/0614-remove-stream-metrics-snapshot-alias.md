# ADR 0614：删除 StreamMetricsSnapshot 同义 alias

## 状态

已采纳并实施。

## 背景

`StreamMetricsSnapshot` 只是命令 request contract 类型 `UiMetricsSnapshot` 的完整 alias。它既没有添加字段约束，也没有定义 aggregator 专用的计算 shape；provider 注册、性能指标 aggregator 和测试都传递相同的 UI metrics request 值。

## 决定

1. 删除 `streamAggregator.ts` 的 `StreamMetricsSnapshot` alias。
2. aggregator callback、performance metrics provider 与测试直接使用 `contracts/commands.ts::UiMetricsSnapshot`。
3. 保留 `UiMetricsSnapshot` 作为发送 `get_performance_metrics` 命令时 renderer metrics 的契约 owner。

## 替代方案

- 保留 alias 并称作 aggregator 输出：拒绝，该名不会产生 nominal type，也没有额外约束；调用方仍可把所有结构相同的值互换。
- 新建 aggregator-only interface：拒绝，现有 aggregator 不增加字段、不归一或转换该值。

## 影响与验证

- 仅改变 UI 内部 TypeScript 类型引用；指标收集值、命令参数和 backend response 不变。
- 命名路线图 §5.7 继续保持 Active；其他 UI aliases、contracts 与组件 owner 仍需审计。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `StreamMetricsSnapshot = UiMetricsSnapshot` 并改回三处引用；无 wire 或持久化迁移。
