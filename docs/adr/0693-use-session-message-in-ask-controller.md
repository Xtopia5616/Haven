# ADR 0693：Ask controller 直接使用 SessionMessage

## 状态

已采纳并实施。

## 背景

`chatAskInteraction.ts` 声明私有 `AskMessage`，字段为 `id`、`type`、`content`、`awaiting` 与 `resolved`。这些字段已经由 UI reducer 的 `SessionMessage` 承载，而 `SessionReducer.getMessages()` 也直接返回 `SessionMessage[]`。controller 再将结果强制 cast 为私有子集，形成第二个消息词汇并掩盖了字段 owner。

## 决定

- 删除 `AskMessage`，由 `createAskInteractionController` 直接读取 `SessionMessage[]`。
- 保留 `AskResponseView` 作为 resolved Ask response 的专用 contract；它与完整 Session message 的职责不同。
- Ask 选项、交互状态及提交行为不变。

## 替代方案

- 保留 `Pick<SessionMessage, ...>` 子集：拒绝。读取结果仍来自同一 `SessionMessage` owner，子集没有新增约束或独立生命周期。
- 保留当前强制 cast：拒绝。它允许 reducer shape 与 controller 假设静默漂移。

## 影响与验证

- 仅收敛前端 TypeScript 类型 owner，不改变状态、DOM、IPC 或持久化。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引检查和 `git diff --check`。

## 回滚

恢复私有 `AskMessage` 与其 cast 即可；无数据或协议迁移。
