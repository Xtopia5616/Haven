# ADR 0032：UI 流式事件归并边界

## 背景

聊天页同时处理流式 chunk 的 sequence 去重、按帧排队、同一消息合并、
thought/reasoning sibling 关联和 action/observation 读取前的同步 flush。
这些操作构成一个独立的事件归并状态机，但原先与页面滚动、会话选择和模型
菜单状态混在 `+page.svelte` 中，难以在无 DOM 环境下验证。

## 决定

- 新增 `ui/src/lib/streamAggregator.ts`，集中拥有 pending chunk 队列、帧调度、
  overflow 上限、step block ID map、sequence 去重、同步 flush 和清理。
- `+page.svelte` 只提供 active session 查询与模型状态回调，并继续负责权威
  thought/action/observation/web-search 事件的业务处理；这些处理在读取消息前
  通过 aggregator 的同步 flush 保持原顺序。
- 保持每帧合并、最旧 chunk 溢出、重复 seq 丢弃、thought 首 chunk 完成 reasoning
  和 minted message ID 关联语义不变；本片不改变 Tauri 事件协议、消息 DTO 或后端持久化。

## 替代方案

- 继续把 chunk 状态机留在 `+page.svelte`：会让路由组件继续膨胀且无法独立测试，拒绝。
- 让 `streaming.ts` 同时拥有排队与纯消息变换：会混淆无状态消息 helper 和有生命周期的
  UI 事件状态，拒绝。
- 在本片重写 stream 事件契约或恢复合并：会扩大跨层变更范围，暂不采用。

## 影响

这是 UI 内部模块重组。流式气泡顺序、active-session 模型状态和权威事件前的可见状态
保持不变，不需要清理 localStorage、数据库或重新配置。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

新增测试覆盖 sequence replay 去重以及 reasoning 在 thought 前完成；既有 streaming、
stores 和路由相关测试全部通过。

## 回滚与重置

代码回滚时删除 `ui/src/lib/streamAggregator.ts` 及其测试，移除 `+page.svelte` 的
aggregator 接线并恢复原 chunk 队列实现；本次不改变持久化数据或配置，不需要用户重置。
