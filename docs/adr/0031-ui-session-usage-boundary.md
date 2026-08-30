# ADR 0031：UI 会话用量状态边界

> 部分决定已由 [ADR 0052：删除已到期的兼容层](0052-remove-expired-compatibility-layers.md) 取代；原 re-export 仅保留为历史记录。

## 背景

`stores.ts` 在会话消息边界拆出后仍同时承载 token usage 状态、LLM 单次调用明细、
恢复/清理操作、缓存命中率计算和显示格式化。它们都围绕会话用量生命周期，
但与 action、通知、会话选择和录音状态没有共享状态不变量。

## 决定

- 新增 `ui/src/lib/sessionUsage.ts`，集中拥有 `sessionTokenStatsStore`、
  `sessionLlmUsageStore`、恢复/追加/清理操作、token 总量与缓存命中率计算，
  以及用量显示格式化函数。
- `stores.ts` 保留原有导出路径，通过 re-export 兼容聊天页、MemoryView、
  ToolResultCard 和既有测试；后续新代码应直接依赖语义明确的用量模块。
- 保持实时 `agent:usage` 累计、恢复数据覆盖空列表、estimated/restored 标记、
  缓存 accounting 规则和格式化结果不变；本片不改变事件协议、后端用量 DTO 或
  持久化语义。

## 替代方案

- 继续把用量逻辑留在 `stores.ts`：会让通用 store 文件继续聚合无关生命周期，拒绝。
- 在聊天页内计算并保存累计用量：会破坏 MemoryView 恢复与 ToolResultCard 的共享状态，
  拒绝。
- 同步重命名 wire 字段或重写 token 计算：会扩大契约变更范围，暂不采用。

## 影响

这是 UI 内部模块重组。原有 import 路径、用量展示和恢复行为保持不变，不需要
清理 localStorage、数据库或重新配置。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

重点回归 stores re-export、恢复/清理、缓存 accounting 和 token/cost 格式化；
本片 UI 测试集应全部通过。

## 回滚与重置

代码回滚时删除 `ui/src/lib/sessionUsage.ts`，移除 `stores.ts` 的 import/re-export，
并恢复原用量实现；本次不改变持久化数据或配置，不需要用户重置。
