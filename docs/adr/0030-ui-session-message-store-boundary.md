# ADR 0030：UI 会话消息状态边界

> 部分决定已由 [ADR 0052：删除已到期的兼容层](0052-remove-expired-compatibility-layers.md) 取代；原 re-export 仅保留为历史记录。

## 背景

`stores.ts` 同时保存任务、通知、会话消息、token usage、录音和模型状态。
会话消息还有自己的生命周期：草稿迁移、session key 迁移、回滚截断、流式
sequence 去重以及清理。这些状态操作会被聊天页、提交流程和 MemoryView
共同调用，继续混在通用 store 文件中会扩大 UI 状态修改的回归面。

## 决定

- 新增 `ui/src/lib/sessionMessages.ts`，集中拥有 `sessionMessagesStore`、
  `_draft` 生命周期、消息增删改、回滚截断、session 迁移和流式 sequence map。
- 初始切片曾由 `stores.ts` 提供过渡性 re-export；该兼容层已按 ADR 0052 删除，
  现有页面、提交流程和测试统一直接依赖本模块。
- 保持 optimistic user bubble 的 prepend/received/steering 处理、rollback cut
  规则、draft 保留策略、sequence 去重和清理时机不变；本片不改变 Tauri 事件
  协议、消息 DTO 或后端持久化。

## 替代方案

- 继续扩展 `stores.ts`：会让互不相关的 store 生命周期耦合，拒绝。
- 让 `+page.svelte` 直接管理消息 map 和 sequence：会把跨页面状态重新放回
  路由组件，拒绝。
- 在本片同时重写消息类型或事件契约：会扩大变更范围，暂不采用。

## 影响

这是 UI 内部模块重组。消息列表、流式渲染、提交和回滚行为保持不变，仓库内旧
`stores.ts` 导入已迁移，不需要清理 localStorage、数据库或重新配置。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

重点回归 draft/session 迁移、rollback 截断和 sequence map 清理；并用导入检查
确认会话消息没有回到 `stores.ts`。本片 UI 测试集应全部通过。

## 回滚与重置

代码回滚时删除 `ui/src/lib/sessionMessages.ts`，移除 `stores.ts` 的 import/
re-export，并恢复原会话消息实现；本次不改变持久化数据或配置，不需要用户重置。
