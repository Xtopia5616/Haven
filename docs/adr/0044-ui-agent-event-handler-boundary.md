# ADR 0044：UI Agent 事件 handler 边界

## 背景

聊天路由页仍直接承载 Agent thought、reasoning、web search、补充输入、工具 action、
tool output 和 observation 的事件处理。事件注册与页面状态编排因此和消息投影、流式
边界处理混在同一文件中，继续扩大 `+page.svelte` 的热点。

## 决定

- 新增 `ui/src/lib/chatAgentEventHandlers.ts`，以工厂集中生成 Agent 事件 handler。
- `+page.svelte` 继续拥有响应式状态、事件注册和流式聚合器实例；handler 工厂只接收
  当前会话读取、block 关联、chunk handler 与 flush 依赖，并负责事件到聊天消息的转换。
- 复用现有 `streaming.ts` 的纯 helper 与 `stores.ts` 的消息/模型状态 API，保持 thought
  snap、web search 分阶段卡片、补充输入、工具占位/观察结果和静默工具的既有语义。
- 不修改 Agent IPC DTO、事件 channel、持久化 schema 或后端 ReAct 行为。

## 替代方案

- 继续把 handler 留在路由页：会延续事件编排与消息投影职责混合，拒绝。
- 把事件处理直接放入 store：会让状态模块承担 Agent 事件语义和流式依赖，拒绝。
- 为拆出的 handler 新建第二套事件注册入口：会产生重复订阅与生命周期风险，拒绝。

## 影响

这是 UI 内部纯边界拆分。事件注册顺序、消息 id、流式 sequence 去重、工具卡片更新和
跨会话补充输入行为保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

## 回滚与重置

代码回滚时删除 `chatAgentEventHandlers.ts`，恢复 `+page.svelte` 中的 Agent handler
映射；本次不改变持久化数据或配置，不需要用户重置。
