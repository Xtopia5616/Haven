# ADR 0631：统一 UI diagnostics 读取 wrapper 动词

## 状态

已采纳并实施。

## 背景

Diagnostics renderer 中，`getLogInfo`、`getApiKeyStatus`、`getPerformanceMetrics` 与 `readLogTail`、`readPerformanceMetricsSnapshot` 都是读取当前诊断投影的 UI 函数。不同动词让相邻功能看起来像有不同查询契约。底层 `get_log_info`、`get_api_key_status`、`get_performance_metrics` 是现有 Tauri IPC 命令名，受生成 contract、命令表和调用方约束。

## 决定

1. Renderer 读取投影统一使用 `readLogInfo`、`readApiKeyStatus` 与 `readPerformanceMetricsSnapshot`。
2. `diagnosticsCommands.ts` 中直接调用 `invoke` 的性能指标 helper 命名为 `requestPerformanceMetricsSnapshot`；更高层 wrapper 从注册的 renderer provider 获取当前 UI metrics 后返回完整 snapshot。
3. 保留 Tauri 命令字符串、参数和响应 DTO；本 ADR 只重命名 TypeScript wrapper。
4. 在函数动词词汇中明确：`read` 表示从本地资源或 owner 读取内容/当前投影；`fetch` 保留给跨边界 I/O 读取。

## 替代方案

- 将所有 wrapper 改为 `get*`：拒绝，这些读取不按稳定 key 定位单一记录，且与既有 `read*` 习惯不一致。
- 重命名 Tauri commands：拒绝，本片只需统一 renderer 动词，wire 命令本身是既有 IPC contract。
- 将指标底层 invoke helper 与上层 provider 汇合函数继续都叫 `readPerformanceMetricsSnapshot`：拒绝，内部调用动作与面向页面的组合读取处于不同 owner。

## 影响与验证

- 这是 UI 内部 TypeScript API 重命名，IPC 名称和 payload 不变。
- IPC contract gate 中两条静态断言已同步到既有决策：parser 返回 ADR 0574 建立的 `ToolManifestView`，并且 ADR 0590 删除的无消费者 `FactSourceRef` façade 不再被要求重新导出。
- 命名审计 §5.7 继续覆盖 renderer wrapper 与 Tauri command 边界；其它 UI modules 和 IPC DTO 仍需逐域审计。
- 验证：`pnpm run check`、`pnpm run test:run`、`pnpm run build`、IPC contract check、ADR 索引及 staged diff 检查。

## 回滚

恢复旧 renderer wrapper 名称与调用点；保留既有 Tauri 命令，无需 IPC 或数据迁移。
