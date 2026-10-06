# ADR 0562：区分诊断命令读取与性能指标聚合入口

## 状态

已采纳并实施。

## 背景

`ui/src/lib/diagnosticsCommands.ts::getPerformanceMetrics(ui?)` 是直接调用 `get_performance_metrics` Tauri 命令的低层 accessor；`ui/src/lib/performanceMetrics.ts::getPerformanceMetrics()` 则读取页面注册的 renderer 指标，再委托给该 accessor。二者 owner 和生命周期不同，但重名使跨模块调用链难以从符号辨认。

## 决定

1. 将命令边界 accessor 命名为 `readPerformanceMetricsSnapshot`，表示其读操作与快照结果。
2. 页面侧 `getPerformanceMetrics` 保持为组合入口，负责获取当前 renderer 指标并调用底层 accessor。
3. 不改变命令名、参数 shape、snapshot 类型、错误传播和前端指标注册生命周期。

## 替代方案

- 合并两个函数：拒绝。命令边界与页面指标 provider 各自有不同职责，测试也分别验证它们。
- 两边都保留 `getPerformanceMetrics`：拒绝。文件路径虽能区分，导入时名称不能说明哪层会采集 renderer 指标。
- 改名页面侧入口：拒绝。该入口是设置诊断视图要读取完整指标的调用面，`getPerformanceMetrics` 对其消费者更直接。

## 影响与验证

- 只改 UI 内部 TypeScript 导出名及其消费者/测试；IPC 与运行行为不变。
- 验证：UI `check`、`test:run`、`build`，ADR 索引和差异空白检查通过。

## 回滚

将 `readPerformanceMetricsSnapshot` 恢复为 `getPerformanceMetrics`，并恢复 `performanceMetrics.ts` 的显式 import alias。
