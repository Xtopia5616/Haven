# ADR 0798：将性能指标 view 对齐到生成的 Rust DTO

## 状态

已采纳并实施（2026-10-08）。

## 背景

UI `PerformanceMetricsSnapshot` 在生成的 `MetricsSnapshot` 之外叠加 `Record<string, unknown>`，声称允许前向兼容扩展。但 Rust producer `haven_agent::MetricsSnapshot` 只有固定的 phases、counters、gauges 和可选 UI metrics 字段；生产消费者只将完整对象序列化下载，没有读取动态键。开放索引没有对应生产者或展示行为，只会放宽 UI 契约。

## 决定

保留有语义的 `PerformanceMetricsSnapshot` UI 名称，但让它直接 alias generated Rust `MetricsSnapshot`。`requestPerformanceMetricsSnapshot` 继续原样返回 invoke 结果，JSON 导出不会按 TypeScript 类型裁剪运行时字段。

## 影响与回滚

不改变 Rust DTO、IPC JSON、metrics 聚合、请求或下载内容。前端编译时不再允许访问未由 Rust DTO 声明的指标字段；以后新增指标由 Rust DTO 与 IPC generator 自然扩展。无持久化、配置或安全语义迁移。回滚时恢复 `Record<string, unknown>` 交叉类型。

## 验收

运行 UI 类型检查和完整 UI 测试；generated contract 无变更。
