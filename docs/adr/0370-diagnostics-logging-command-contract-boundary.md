# ADR 0370：Diagnostics and logging read command boundary

- 状态：已采纳（2026-09-26）
- 基线：HEAD `fd17bac`；开始时工作区干净
- 范围：SettingsView 的 `get_log_info`、`read_log_tail`、`get_performance_metrics`、`check_shell_available` 与 `get_api_key_status` 读取边界
- 关联：ADR 0007（settings diagnostics contracts）、ADR 0341（settings command boundary）、ADR 0368（model discovery command contract）、ADR 0369（tools catalog command contract）

## 背景与审计

`get_log_info`、`read_log_tail`、`check_shell_available` 与 `get_api_key_status` 都由 `SettingsView` 直接 `invoke`，再在组件内调用 `contracts/settings.ts` 中相应的 response parser。`get_performance_metrics` 已由 `performanceMetrics.ts` 集中调用，该模块还拥有 renderer stream metrics provider。

审计未发现重复的 Rust DTO、TypeScript response interface 或 runtime parser：Rust DTO 分别由 `log.rs`、`settings.rs`、`model.rs` 和 `haven-agent` metrics 定义；`LogInfo`、`LogTail`、`ShellAvailability` 与 `ApiKeyStatus` 各只有一份前端接口和 validator。`parseLogInfo` 等 parser 投影为既有字段，忽略额外字段；API-key status 中动态的 model/provider names 由 `Record<string, boolean>` 表达。metrics response 使用动态诊断 snapshot，不应增加会丢字段的 mapper。`SettingsPayload` 中的开放嵌套设置契约属于 ADR 0341，本轮不处理。

## 决定

1. 新增 `diagnosticsCommands.ts`，集中提供上述五个 typed command helpers。日志、shell 与 API-key helpers 继续调用现有 parser，不复制校验或字段映射；metrics helper 直接返回 invoke 结果。
2. 在 `contracts/commands.ts` 为日志 tail、shell availability 和 renderer metrics 添加命名 request types。`StreamMetricsSnapshot` 改为复用 `UiMetricsSnapshot`，避免重复声明同一组三个 renderer counters。`MetricsSnapshot` 以 `Record<string, unknown>` 保持开放，未来诊断字段继续透传。
3. `performanceMetrics.ts` 继续拥有 renderer metrics provider，只把 command invocation 委托给 `diagnosticsCommands.ts`。SettingsView 只调用 typed helpers；它仍拥有刷新顺序、局部状态、通知和现有错误处理。
4. `check-ipc-contracts.ps1` 对照 Rust registry、请求字段和 response DTO 字段与前端合同，验证 helper 使用既有 parser/开放 metrics response，并拒绝其他 UI source 直接 invoke 这五个命令。不引入 Rust→TypeScript codegen。

## 兼容性与影响

Rust command names、flat Tauri 参数、renderer camelCase 参数名、response snake_case 字段均不变。日志 tail 仍请求 300 行；Rust 端默认值、10–2000 clamp、文件选择和尾部顺序不变。日志/shell/API-key parser 的错误和字段投影不变；API-key response 仍只向 UI 暴露配置 presence flags 和动态 model/provider flags。metrics 请求仍在无 renderer snapshot 时传 `undefined`，有 snapshot 时传 `{ ui }`，所有 response fields 原样保留。

`SettingsView` 的 shell 探测顺序、log viewer 加载状态、notification/error 文案与 catch 次序不变。本切片不涉及 Settings update/apply、permission/autostart writes、memory maintenance、connectivity probe、Rust command handlers、DB/ID/X12、Tools/LLM runtime 或通知语义。无 schema、配置或用户数据迁移。

## 验证

command helper 回归覆盖五个命令的名称与参数、现有 response projection、动态 metrics fields 和 invoke rejection 传播。SettingsView 既有导出与初始化测试保持。IPC contract script 校验 Rust/TypeScript registry、DTO/request fields 和无绕行边界。

```sh
corepack pnpm run check
corepack pnpm run test:run
corepack pnpm run build
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
pwsh -NoProfile -File scripts/check-ipc-events.ps1
```

没有 Rust 文件变更，因此不运行 Rust 门禁。

## 回滚

回滚本提交可恢复 SettingsView 对四个读取命令的直接 invoke，并恢复 `performanceMetrics.ts` 的直接 metrics invoke；同时删除 diagnostics helpers、命名 request/result types、相关 contract assertions 与本 ADR/路线图记录。无数据、配置或 schema 回滚步骤。
