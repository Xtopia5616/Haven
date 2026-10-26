# ADR 1415：UI 会话操作结果与展示副作用分层

## 状态

已实施 — 2026-10-25。

显式操作刷新失败的生产传播边界于 [ADR 1431](1431-ui-fork-session-action-results.md) 补齐；下述当时的替身测试不代表列表 owner 已传播真实查询拒绝。

## 背景与证据

`ChatSessionController` 的会话切换、rollback 与消息提交同时承担命令/投影协调和页面通知、草稿、弹窗或列表刷新。切换以 `boolean` 混合了过期选择与失败；rollback 和提交返回 `void`，页面无法区分未执行、已提交及其后刷新失败。提交完成后列表刷新使用未等待的 Promise，刷新拒绝可能未处理，也无法表达 transcript 已成功提交。

## 决定与不变量

1. 会话切换返回 `switched`、`superseded` 或携带错误的 `failed`；现有 `SessionReducer` 选择版本继续隔离快速切换的迟到结果。
2. rollback 返回 `ignored`、`rejected`、`failed` 或 `rolled-back`。成功结果单独携带选择是否仍当前，以及 transcript 刷新是完成、被新快照取代还是失败。路由按结果决定通知、恢复草稿、关闭弹窗和刷新列表；rollback 命令的串行锁与加载态仍由 controller 管理。
3. 提交返回 `submitted` 或 `failed`，不调用会话列表刷新。路由使用小型组合函数独立记录提交和列表刷新结果；刷新失败保留 `submitted`，并报告为列表刷新失败。
4. 继续生成仍使用已有选择版本及显式 submission token；相同内容的独立提交仍是独立请求。controller 内部 follow-up 在提交结果后执行单次列表刷新。

## 替代方案与停止条件

- 仅更换返回值类型为 `boolean` 或 `void`：仍无法区分过期、拒绝、命令失败和提交后刷新失败。
- 立即把所有会话操作改写成通用 action bus 或新状态 store：本切片只迁移三条已验证的垂直链，不新增万能编排 owner。

## 影响、风险与回滚

仅改变 UI 内部 TypeScript 结果与副作用归属；无 Rust、IPC、数据库、配置或持久事件变化，无需重置。列表刷新错误现在与提交错误分开报告。回滚时恢复原 controller 回调和调用方处理，并还原本 ADR 与路线图状态；不影响持久数据。

## 验证

- `corepack pnpm exec vitest run src/lib/chatSessionController.test.ts --maxWorkers=2`：34 项通过；覆盖快速切换、rollback 重入/旧选择和提交成功后刷新失败。
- `corepack pnpm run check`：0 errors、0 warnings。
- `corepack pnpm run test:run --maxWorkers=2`：170 个文件、1517 项通过。Vitest 输出 `TimeoutNaNWarning`，无失败。
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`：1312 条记录唯一且本地链接有效；`git diff --check` 通过。
- 真实 WebView 并发验收继续由路线图 §5.1 跟踪。
