# ADR 0835：按生产 owner 拆分遗留 store 测试桶

## 状态

Accepted — 2026-10-09

## 背景

[ADR 0203](0203-ui-domain-store-boundaries.md) 删除了跨领域生产入口 `stores.ts`，但将旧测试文件 `stores.test.ts` 暂时留作拆分候选。该文件目前把 `toolRunStore`、`notificationStore`、`messageFactory`、`sessionRuntimeStore` 和 `sessionUsage` 五个生产 owner 的 39 个测试放在一起。测试文件名因此继续暗示一个已删除的聚合 owner，也让修改者需要阅读无关领域测试才能定位对应模块的验证。

## 决定

- 将 ToolRun 状态与历史水合测试移至 `toolRunStore.test.ts`。
- 将通知生命周期测试移至 `notificationStore.test.ts`。
- 将消息构造测试移至 `messageFactory.test.ts`。
- 将 ReAct 执行阶段测试移至 `sessionRuntimeStore.test.ts`。
- 将 token/cache 用量与格式化测试移至 `sessionUsage.test.ts`。
- 删除 `stores.test.ts`；保留原有 39 个测试和断言，不新增生产 API、共享测试入口或兼容文件。
- 领域文件内的 Tauri mock 只留在真正需要验证 Tauri 边界的 ToolRun 与通知测试文件。

## 替代方案

- 继续使用 `stores.test.ts`：拒绝。它的源生产 owner 已不存在，且测试不验证一个共同状态边界。
- 删除整组测试：拒绝。重命名和归属调整不应降低现有行为覆盖。
- 建立新的测试聚合模块：拒绝。会再次形成无独立生产 owner 的共享桶。

## 影响与验证

仅调整 UI 测试文件归属和命名，不改变运行时代码、状态生命周期、IPC、持久化或用户数据，无需重置。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、相关文件 Prettier、ADR 索引与 `git diff --check`。

## 回滚

将五组测试恢复到 `stores.test.ts` 并删除五个独立测试文件；无需数据或配置重置。
