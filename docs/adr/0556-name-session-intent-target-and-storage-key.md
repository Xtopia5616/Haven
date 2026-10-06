# ADR 0556：明确 UI 会话意图目标与存储键

## 状态

已采纳并实施；UI 类型检查、测试与生产构建均通过。

## 背景

`sessionIntentStore.ts` 保存两类会话意图：从历史页面交给 chat route 的一次性恢复目标，以及用户明确选择新会话后跨应用重启保留的自动恢复抑制标记。类型 `ResumeTarget` 和 store `resumeTargetStore` 没有在导出名里表明实体范围；`NEW_ACTION_INTENT_KEY` 也没有说明它属于新会话动作。后者实际存入 localStorage 的值是字符串 `haven.no_auto_restore`。

SessionRail 的空状态同时把 Haven 的 session 实体称作“对话”，与其余 UI 和命名规范使用的“会话”不同。

## 决定

1. 将 `ResumeTarget` 和 `resumeTargetStore` 改为 `SessionResumeTarget` 和 `sessionResumeTargetStore`。
2. 将 `NEW_ACTION_INTENT_KEY` 改为 `NEW_SESSION_INTENT_STORAGE_KEY`；保留 localStorage 的键值 `haven.no_auto_restore`，因此既有重启状态继续生效。
3. 更新消费者、测试、`docs/naming.md` 的示例和路线图；SessionRail 空状态使用“会话”。
4. 保留两类意图的独立存储与生命周期：恢复目标是导航期间的一次性内存 handoff，新会话意图是启动和提交逻辑读取的内存状态，并通过 localStorage 标记跨重启保留；同属 session 意图不足以证明应合并。

## 替代方案

- 合并为一个统一的 `sessionIntentStore` 状态对象：拒绝。恢复目标包含要选择的 session 及其投影信息，且只在本次页面导航期间有效；新会话标记跨重启持久化并参与提交与自动选择，两者的写入方、消费者和清理时机不同。
- 修改 localStorage 键值以匹配新常量名：拒绝。它会使已经写入旧键的用户在重启时丢失“不要自动恢复”的意图；本次只改代码符号名。
- 继续使用泛化的 `ResumeTarget`：拒绝。该类型由 session history 与 chat startup 跨模块传递，名称应显式带实体作用域。

## 影响与验证

- 只重命名 UI 内部 TypeScript 导出和 localStorage key 常量，并调整一个空状态文案；不改变 localStorage 中的键值、命令/事件、数据库、session 恢复顺序或提交行为。
- 验证 Svelte 类型检查、UI 测试、生产构建和 ADR 索引；确认旧符号名不再出现在活动 UI 源码中。

## 回滚

恢复旧 TypeScript 类型/store/常量符号及其消费者，并撤回 SessionRail 文案；localStorage 键值未变化，无需迁移或重置数据。
