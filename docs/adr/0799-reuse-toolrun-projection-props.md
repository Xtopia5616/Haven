# ADR 0799：复用 ToolRun card projection props

## 状态

已采纳并实施（2026-10-08）。

## 背景

`ToolRunCardProjectionOptions` 是 `projectToolRunCard` 的 resolver 输入 owner，但 `ToolRunCenter` 再次手写了四个相同 callback 签名。`MemoryView` 作为包装视图，又复制 ToolRunCenter 的行数据与回调 props 后原样向下传递，独立维护相同字段类型。

## 决定

1. `ToolRunCenter.Props` 从 `Partial<ToolRunCardProjectionOptions>` 继承可选 projection callbacks；默认函数行为保持不变。
2. `MemoryView.Props` 从 `ComponentProps<typeof ToolRunCenter>` 中 Pick 原样透传的 ToolRun 字段与回调，不再重复声明 shape。
3. MemoryView 自己拥有的可见性与新会话回调仍由本组件声明；历史列表与加载状态仍由它内部维护。

## 影响与回滚

只统一 UI 内部 props 的类型来源，不改变列表、筛选、投影、命令调用或 event 行为。无 IPC、持久化或配置影响。回滚时可恢复局部 callback/row type 声明。

## 验收

运行 UI 类型检查和完整 UI 测试；无 generated contract 变更。
