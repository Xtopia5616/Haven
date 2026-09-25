# ADR 0368：Model discovery command contract boundary

- 状态：已采纳（2026-09-26）
- 基线：HEAD `46a5076`；开始时工作区干净
- 范围：`discover_models` / `discover_all_models` 的前端 request/result contract 与活跃 UI 调用边界
- 关联：ADR 0320（chat model operations）、ADR 0341（settings command boundary）、ADR 0357（memory commands）、ADR 0358（session history commands）

## 背景与审计

Rust command registry 已登记 `DiscoverModelsRequest` 与 `ModelInfo[]`，但设置页 discovery、聊天默认模型同步和媒体 STT 发现分别直接调用通用 `invoke('discover_models', ...)`。`modelDiscovery.ts` 还维护了局部 model response interface；前端因此没有一个可核对 Rust wire DTO、扁平请求和调用点的 typed boundary。

同一 command family 中，`discover_all_models` 仅由 `modelDiscovery.ts` 调用。`chatModelSync.ts` 自己持有按 base URL 分代的 module-level cache 和 in-flight promise；这属于调用方刷新状态，不应搬进 command adapter。

相邻命令审计结果：`check_llm_connection` 只有 `+layout.svelte` 一个调用点，所有 report 输入经 `normalizeLlmConnectionReport` 做一次校验/降级；未发现可合并的第二个 request 或 mapper。`get_performance_metrics` 已通过 `performanceMetrics.ts` 集中调用，属于 diagnostics，不并入本 discovery 切片。

## 决定

1. 在 `contracts/commands.ts` 定义 flat camelCase `DiscoverModelsRequest`；在 `contracts/model.ts` 定义与 Rust `haven_llm::ModelInfo` 对应的 snake_case `ModelInfo` 及 provider→model map。model result 保留开放索引签名，让新增 provider metadata 可穿过前端边界。
2. 新增 `modelDiscoveryCommands.ts`，为两个 discovery commands 提供 typed direct-forward helpers。helper 不校验、转换、筛选、排序或包装结果与错误；flat 参数、空结果、未知附加字段、调用 rejection 都原样返回。
3. 设置 discovery、聊天工具栏默认模型同步和 `MediaSettings` 的 STT discovery 改用该 helper。刷新策略、module-level cache/in-flight/stale response guard、provider filtering、role 参数、状态更新、错误处理和 toast 顺序继续由现有调用方持有。
4. IPC contract script 对照 `discover_models` 的 Rust 参数和 `ModelInfo` Rust/TypeScript 字段，并拒绝其他 UI 文件直接 invoke `discover_models` / `discover_all_models`。
5. 保留 `check_llm_connection` 既有 runtime normalizer 与 layout 状态/通知/错误流程；保留 diagnostics 与 `get_performance_metrics` 既有 helper。全局 Rust→TypeScript codegen 不在本 ADR 范围。

## 兼容性与影响

Rust command 名称、Tauri flat camelCase 参数（对应 Rust snake_case 参数）、响应 snake_case 字段和执行时序不变。`discover_models` 的 provider/role 可选字段仍可省略；chat caller 仍传 `role: 'chat'`，STT caller 仍传 `role: 'transcription'`。empty result 和 provider map 的空列表不被过滤。模型列表响应不经 mapper，unknown extension fields 保留。

Connection report 的 status/reason fallback、通知状态转移、probe 错误 catch 行为均未修改。无 Rust handler、router、DB、ID、X12、Settings apply、事件、配置或用户数据变化，无 codegen 和重置要求。

## 验收

新增 helper tests 覆盖扁平参数、空结果与未知扩展字段；`ModelSettings` 行为测试继续验证其 discovery 调用参数。helper 只直接返回 `invoke` Promise，IPC script 固定该形态，因此 command rejection 不被包装或吞掉；IPC scripts 同时校验 command/DTO contract 和 helper 无绕行。

```sh
corepack pnpm run check
corepack pnpm run test:run
corepack pnpm run build
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
pwsh -NoProfile -File scripts/check-ipc-events.ps1
```

## 回滚

回滚本提交即可恢复调用方的直接 discovery invoke 与局部 model response interface，并移除 helper、contract type、IPC 检查和本 ADR/路线图记录。无数据、schema 或配置回滚步骤。
