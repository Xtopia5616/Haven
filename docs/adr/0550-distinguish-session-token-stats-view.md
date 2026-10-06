# ADR 0550：区分 reducer 统计与 usage view 输入

## 状态

已采纳并实施；UI 类型检查、测试与构建通过。

## 背景

UI 有两种同名 `SessionTokenStats`。`sessionReducer/types.ts` 的 reducer state 类型要求完整的 session 累计统计、费用和模型字段；`sessionUsagePresentation.ts` 的同名类型则把展示函数实际可用的统计字段定义为可选，以便接受部分恢复数据和测试输入。两者只部分重叠，要求不同。`+page.svelte` 已把展示类型临时导入为 `PresentationSessionTokenStats`，说明裸名称无法区分。

## 决定

1. reducer 的完整运行态类型保留为 `SessionTokenStats`。
2. 展示模块的稀疏可选输入类型改名为 `SessionTokenStatsView`，并同步更新展示函数、toolbar props、settings view 与聊天页回调。
3. 不改变任一类型字段、optional 约束、reducer 状态、usage 计算或组件行为。
4. 在命名规范中记录：同域类型的完整 state、稀疏 view 输入和 wire projection 需要用角色词区分。

## 替代方案

- 合并为 reducer 的完整类型：拒绝。展示函数允许部分统计数据，而 reducer 不变量要求完整累计状态；放宽 reducer 类型会削弱状态契约。
- 将展示类型改为 `Partial<SessionTokenStats>`：拒绝。展示模块会依赖 reducer 内部模型并暴露其所有字段，不再保持独立的显示输入边界。
- 保留原名并让页面继续加本地别名：拒绝。冲突会继续传播给其他消费者，调用者仍要自行消歧。

## 影响与验证

- 仅更名 UI 展示输入类型；事件、IPC、reducer 数据、usage calculations 和可见 UI 不变。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复展示类型 `SessionTokenStats` 与页面别名 `PresentationSessionTokenStats`，并移除此 ADR 与路线图/命名规范记录。无需 IPC、数据库或用户数据迁移。
