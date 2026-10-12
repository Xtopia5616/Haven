# 1007：校验性能指标的 JavaScript 整数精度

## 状态

已采纳并实施（2026-10-12）。

## 背景

Rust `MetricsSnapshot` 的 phase、counter、gauge 和 renderer counter 使用 `u64`，经 JSON 到达 UI 后静态类型都是 JavaScript `number`。`Number` 对大于 `Number.MAX_SAFE_INTEGER` 的整数不能精确表示，直接导出会静默改变极大计数值。ADR 0798 保留了 generated DTO alias 和有效快照原样导出，但没有覆盖运行时数值精度。

## 决定

- `get_performance_metrics` 的 UI adapter 校验 `phases`、`counters`、`gauges` 和可选 `ui` 中所有 Rust `u64` 字段都是非负安全整数。
- 不安全、负数或缺字段的响应拒绝导出；有效响应仍返回原对象，不裁剪未来扩展字段，也不改变 Rust DTO。
- 不新增独立的 metrics phase 数组长度常量；phase 数据只作整体诊断导出，Rust 继续拥有 phase 集合及其顺序。

## 影响

不改变 IPC JSON、Rust 聚合、生成类型、数据库或配置。只有畸形或无法被 JavaScript 精确表示的数值响应会失败关闭；无持久数据迁移或重置要求。有效快照的导出内容保持不变。

## 替代方案

- 保留未经校验的透传：拒绝；极大 `u64` 会在 JSON 导出中静默失真。
- 将 Rust 指标改为十进制字符串：拒绝；这会改变 generated IPC 类型及现有消费形状，当前没有跨越安全整数范围的生产证据。

## 验证

- `corepack pnpm exec vitest run src/lib/diagnosticsCommands.test.ts src/lib/performanceMetrics.test.ts`
- `corepack pnpm run check`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- 完整 UI 测试与构建在本轮全量验收中复跑。

## 回滚

移除 diagnostics parser 并恢复直接透传即可回滚；此操作会重新允许极大 `u64` 在 renderer export 中静默失真。
