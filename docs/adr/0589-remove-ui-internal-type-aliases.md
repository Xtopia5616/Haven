# ADR 0589：删除无语义增量的 UI 内部类型别名

## 状态

已采纳并实施。

## 背景

UI 逻辑模块和组件脚本中存在若干 type alias，只把领域 contract 换成另一名称：`LlmUsage`、`ModelMap`、`ResumeData` 及未使用的 resume step/message 别名、`ToolRunEntry`、`MemorySession` 和 `TaskToolRun`。它们没有增加字段约束、renderer shape、领域 owner 或生命周期，消费者需要在多个名字之间跳转才能找到真实类型来源。

## 决定

1. 无语义增量的模块局部 alias 删除，生产消费者直接引用 `contracts/` 中的 canonical type。
2. `buildResumeMessages` 接受 `SessionResumeInput`；它生成的 `ResumeMessage` 是独立的 renderer shape，继续由 resume-message builder 拥有。
3. 测试 fixture helper 改为 `sessionResumeInput`，明确其构造的是领域恢复输入。
4. 本 ADR 只覆盖首轮扫描命中的模块，不代表整个 UI alias 与 controller/props 审计完成。

## 替代方案

- 为每个模块保留短 alias：拒绝，alias 不表达新的消费角色，反而增加词汇分叉。
- 删除领域 contract façade：拒绝，领域 `contracts/` 模块是生产消费者使用的导入边界，本次不改变其职责。
- 把 `ResumeMessage` 改为输入 contract：拒绝，前者有 renderer 专用字段，形状和用途均不同。

## 影响与验证

- 改动仅在 UI 内部类型引用，不改变运行时值、JSON、IPC、事件、存储或用户行为。
- 更新 `docs/naming.md` 与架构路线图；无数据迁移或重置要求。
- 验证通过：`corepack pnpm run check`、`corepack pnpm run test:run`（122 files / 975 tests）、`corepack pnpm run build`、ADR 索引与 `git diff --check`。

## 回滚

恢复相关局部 alias 及其导入，不涉及生成契约或持久数据。
