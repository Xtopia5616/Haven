# ADR 0254：删除无调用的 MemoryWorker recall 转发

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent::MemoryWorker` 的 recall 公共面
- 关联：[ADR 0247](0247-memory-worker-inference-port.md)

## 背景

`MemoryWorker::recall_memory_query` 只是把 typed `MemoryQuery` 转发给
`MemoryService::recall`。全仓没有调用方；后台 worker 的职责是事实抽取、维护、outbox
和索引补齐，而不是承载交互式 recall 读取。保留这个转发会让 Agent 同时暴露两个相同
职责的记忆读取入口，也会模糊后续 MemoryRuntime/MemoryReader 的边界。

## 决定

1. 删除无调用的 `MemoryWorker::recall_memory_query` 及其专用 imports。
2. 交互式 recall 继续由 `MemoryService` 和现有 `MemoryRecallPort` 负责；本切片不新增
   同义 trait，也不改变 recall、embedding、缓存或事实抽取语义。
3. 将此视为公共面收缩，不将其计入尚未完成的 committed-event 驱动 MemoryRuntime。

## 影响与验证

AgentLayer、Tauri recall 命令和 Memory tool 的现有读取路径不变。验证包括 Agent 测试、
workspace 严格 Clippy、fmt 和全 workspace 测试；无 schema、IPC 或配置格式变化。

## 回滚

回退本切片提交并恢复转发方法即可；无需数据库重置。
