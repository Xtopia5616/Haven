# ADR 1092：限制 MediaPlan renderer 缓存

## 状态

已接受（2026-10-13）

## 背景

MediaPlan store 为每个 session 最多保留 32 条，但对 session 数没有限制。长时间运行并结束大量 session 时，顶层 UI store 会持续增长。

## 决定

- 每 session 保留最近 32 条 MediaPlan；全局最多缓存最近使用的 32 个 session，写入时刷新该 session 的 LRU 顺序。
- 当前可见 session 结束时保留其缓存，使终态 media cards 仍可渲染；非当前 session 到达 completed/error 时清理其缓存。
- 删除 session 仍立即清理；显式 MemoryView 清理可以清除单个或全部缓存。

## 影响

缓存内存受固定上限约束，当前可见的终态卡片继续工作。超过容量的最久未更新 session projection 会被驱逐。

## 验证

UI store 测试覆盖单 session 与全局限制、LRU 刷新和清理；session lifecycle 测试覆盖活动与非活动终态策略；运行 UI 全量测试与 Svelte check。

## 回滚

不得移除全局 session 上限或在删除 session 后保留对应 projection。
