# ADR 0537：前端性能指标响应对齐生成契约

## 状态

已采纳并实施；UI 门禁通过。

## 背景

`get_performance_metrics` 的 Rust handler 返回 Agent-owned `MetricsSnapshot`，IPC 生成器已为该响应生成具名的 `phases`、`counters`、`gauges` 与可选 `ui` 字段。前端 `settings.ts` 却把同一响应别名为 `Record<string, unknown>`；`diagnosticsCommands.ts` 因而只能从 `unknown` 作宽泛断言，调用方无法获得稳定字段的静态检查。该类型也放在 settings contracts 中，与其 diagnostics 消费职责不符。

## 决定

1. 在 `contracts/diagnostics.ts` 定义前端 `PerformanceMetricsSnapshot`，以生成的 Rust `MetricsSnapshot` 为已知字段，并交叉 `Record<string, unknown>` 保留未来诊断扩展字段的访问能力。
2. 让 diagnostics command 与性能指标 UI API 共用这一类型；删除 settings contract 中宽泛的 `MetricsSnapshot` alias。
3. 保持 `UiMetricsSnapshot` 输入仍从 generated command request 派生，Rust DTO、IPC 字段、运行时解析/过滤行为均不变。

## 替代方案

- 继续使用 `Record<string, unknown>`：拒绝。它隐藏当前稳定响应字段的名称和类型。
- 只使用闭合 generated DTO：拒绝。前端诊断读取约定保留未知扩展项；开放索引签名可表达这一访问约定，同时不抹掉已知字段。
- 修改 Rust/IPC 响应：拒绝。本次没有字段契约缺陷，不需要扩大 wire 变更面。

## 影响与验证

- 仅调整前端静态类型归属和名称，无 Rust、Tauri IPC、序列化、持久化或运行行为变化。
- 验证通过：`corepack pnpm run check`（0 errors / 0 warnings）、`corepack pnpm run test:run`（122 个 test files、974 tests 全部通过）、`git diff --check` 与 ADR index（520 条唯一编号记录、链接解析通过）。Vitest 输出 Node `TimeoutNaNWarning`，未影响退出状态或测试结果。

## 回滚

恢复 settings contract 的旧 alias 与消费者导入即可；没有数据、IPC 或用户状态回滚。
