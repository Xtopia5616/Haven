# ADR 0045：UI Agent 用量事件边界

## 背景

聊天路由页在注册 Agent 事件时仍直接处理每步 LLM 用量、累计用量、调用明细和上下文
压缩通知。用量状态虽已从 `stores.ts` 拆出，但事件投影仍与页面生命周期和其他事件
混在一起。

## 决定

- 新增 `ui/src/lib/chatUsageEventHandlers.ts`，集中生成 `agent:usage` 与
  `agent:compaction` handler。
- 用量 handler 继续通过既有 store API 计算 inclusive/exclusive cache accounting、
  累计 token、费用和每步 LLM 调用明细；压缩 handler 继续使用既有 token 格式与通知
  时长。
- `+page.svelte` 只负责把该 handler 工厂接到已有 `agentEventListeners` 适配器；不
  修改 Agent IPC DTO、事件 channel、持久化 schema 或工具卡片消费方式。

## 替代方案

- 继续把用量 handler 留在路由页：会延续页面事件编排与用量投影耦合，拒绝。
- 把压缩通知放入 usage store：会让状态模块承担通知副作用，拒绝。
- 修改 usage DTO 以适配展示：没有必要且扩大跨层风险，暂不采用。

## 影响

这是 UI 内部事件投影边界拆分。实时 token 卡片、每步用量明细、恢复估算覆盖和压缩
通知保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

## 回滚与重置

代码回滚时删除 `chatUsageEventHandlers.ts`，恢复 `+page.svelte` 中的两个用量事件
handler；本次不改变持久化数据或配置，不需要用户重置。
