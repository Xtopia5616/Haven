# ADR 0049：UI ask 交互边界

## 背景

聊天路由页直接持有 ask 选项选择、批量问题回答、忽略、恢复清理和重复提交防护。
这些状态跨越消息 store、InputRouter 和 ChatBubble 回调，继续留在路由页会混合输入
编排与问答交互策略。

## 决定

- 新增 `ui/src/lib/chatAskInteraction.ts`，集中持有每会话的 ask 选择与已解决 id，
  并负责批量回答组合、忽略、恢复清理和重复提交保护。
- 路由页通过显式回调提供 active session、滚动跟随、`askSelectionsReady` 响应式写入
  和普通消息提交；InputRouter 与 ChatBubble 继续使用原有回调契约。
- 同一批次已解决的 ask id 在会话恢复/结束前保持幂等，避免重复点击再次提交；不改变
  ask 消息结构、会话 IPC、持久化 schema 或后端恢复行为。

## 替代方案

- 继续把 ask 逻辑留在路由页：会延续页面输入编排与问答状态混合，拒绝。
- 把选择状态放进全局 store：会扩大状态生命周期并引入额外清理路径，拒绝。
- 在 ChatBubble 内直接提交消息：会让展示组件越过路由输入边界，拒绝。

## 影响

这是 UI 内部交互状态边界拆分。单问/多问组合、选项与文本合并、忽略文案、恢复清理
及重复点击行为保持既有意图；本片补齐重复提交防护，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

新增 `chatAskInteraction.test.ts` 覆盖批量回答、忽略幂等和恢复清理。

## 回滚与重置

代码回滚时删除 `chatAskInteraction.ts` 及其测试，恢复 `+page.svelte` 中的 ask 交互
状态与 handler；本次不改变持久化数据或配置，不需要用户重置。
