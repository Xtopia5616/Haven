# ADR 0040：UI agent result renderer 边界

## 背景

`agent` 工具结果包含同伴列表、创建/排队状态、超时、自动消息和回复等多种
结果 shape。它的条件模板已经成为 `ToolResultCard` 中最后一个复杂 custom body。

## 决定

- 新增 `ToolAgentResult.svelte`，承载 `agent` 工具的全部结果 body 展示。
- `toolResultRenderers.ts` 按 `custom + toolName=agent` 选择该组件；公共卡片继续
  负责折叠、参数、复制菜单、流式等待和 hint。
- 保持同伴低信任提示、注册列表、超时/创建/排队状态、回复正文和原有中文文案；
  不改变 agent IPC DTO、会话队列或后端协作语义。

## 替代方案

- 继续在 `ToolResultCard` 添加 agent 条件分支：会保留单体卡片热点，拒绝。
- 把 agent 结果降级为通用 JSON：会丢失状态和低信任展示语义，拒绝。
- 在本片修改 agent 数据契约：没有必要且扩大跨层风险，暂不采用。

## 影响

这是 UI 内部 renderer 拆分。agent 结果的展示保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 `ToolResultCard` agent 测试覆盖拆分后的 registry renderer。

## 回滚与重置

代码回滚时删除 agent renderer 组件，移除 registry 映射并恢复 `ToolResultCard`
对应分支；本次不改变持久化数据或配置，不需要用户重置。
