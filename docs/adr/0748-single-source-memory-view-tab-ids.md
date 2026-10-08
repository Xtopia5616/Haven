# ADR 0748：MemoryView 页签 ID 使用单一来源

## 状态

已采纳并实施。

## 背景

MemoryView 将 `sessions`、`tasks`、`memory` 同时写在 `MemoryTabId` union、URL runtime guard 的 ID 数组和 MaterialTabs options 中。新增或重命名页签时，这些列表可能各自漂移。

## 决定

- `MEMORY_TAB_IDS` 是唯一的页签 ID tuple，`MemoryTabId` 从 tuple 元素派生。
- URL guard 与 MaterialTabs options 都从该 tuple 派生。
- `MEMORY_TAB_LABELS` 按 `Record<MemoryTabId, string>` 穷尽检查标签，tuple 顺序继续决定界面顺序。

## 影响与验证

仅收敛 UI 内部常量与类型 owner；URL 参数、页签顺序、标签和行为不变。通过 Svelte 类型检查与 UI 全量测试验证。

## 回滚

可恢复重复的 union、验证数组和 options literal。没有 wire、持久化或路由格式迁移。
