# ADR 0047：UI 会话生命周期事件边界

## 背景

聊天路由页直接处理 session created、updated、completed、error 和 title-updated 事件，
同时混合草稿迁移、错误态、ask 状态、终态消息清理、流式 block 清理和会话列表刷新。
这些是会话事件投影职责，继续放在路由页会让生命周期顺序难以独立审查。

## 决定

- 新增 `ui/src/lib/chatSessionEventHandlers.ts`，集中生成会话生命周期事件 handler。
- 路由页继续拥有 Svelte 响应式状态与生命周期回调；handler 通过显式回调执行草稿迁移、
  active session 选择、错误态切换、ask 清理、终态消息收尾、内存回收和列表刷新。
- 保持 session created 的 draft 迁移与新会话意图竞态保护、pending 清除 ask 等待态、
  终态清理顺序及 title 更新行为；不修改 session IPC DTO、事件 channel、持久化 schema
  或后端状态机。

## 替代方案

- 继续把生命周期 handler 留在路由页：会延续事件投影与页面状态混合，拒绝。
- 让 handler 直接持有 Svelte 状态：会绕过显式生命周期依赖，拒绝。
- 把生命周期策略放入 contracts 层：会让契约层承担 UI 内存与错误态副作用，拒绝。

## 影响

这是 UI 内部事件投影边界拆分。会话切换、草稿归属、错误恢复、终态流式消息收尾和
历史列表刷新保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

## 回滚与重置

代码回滚时删除 `chatSessionEventHandlers.ts`，恢复 `+page.svelte` 中的 session 事件
handler；本次不改变持久化数据或配置，不需要用户重置。
