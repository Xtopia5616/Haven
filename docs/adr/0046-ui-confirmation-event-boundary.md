# ADR 0046：UI 安全确认事件边界

## 背景

聊天路由页直接处理 app-shell 的 `confirm:requested` 事件，并同时负责确认队列与
对话框生命周期。事件 DTO 到队列项的转换属于独立的 UI 事件投影职责，继续留在路由
页会让确认相关边界难以复用和审查。

## 决定

- 新增 `ui/src/lib/chatConfirmationEventHandlers.ts`，集中把已由 app contract 转换
  的 `confirm:requested` DTO 映射为确认队列项。
- 路由页继续拥有确认队列、对话框状态、显示下一项以及 `resolve_confirmation` IPC
  调用；handler 通过显式回调写入队列并触发显示，不建立第二套状态或监听器。
- 保持后台会话确认不丢弃、到达顺序、会话标题回退、风险等级和权限 key 的既有语义；
  不修改安全策略、确认 DTO、事件 channel 或后端授权判断。

## 替代方案

- 继续把事件映射留在路由页：会延续安全事件投影与对话框编排混合，拒绝。
- 让确认 handler 直接持有 Svelte 状态或调用 IPC：会绕过页面生命周期，拒绝。
- 把队列逻辑放入 app contract：会让契约层承担 UI 状态，拒绝。

## 影响

这是 UI 内部安全事件投影边界拆分。确认弹窗、队列顺序与授权请求行为保持不变，
不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

## 回滚与重置

代码回滚时删除 `chatConfirmationEventHandlers.ts`，恢复 `+page.svelte` 中的
`confirm:requested` handler；本次不改变持久化数据、权限策略或配置，不需要用户重置。
