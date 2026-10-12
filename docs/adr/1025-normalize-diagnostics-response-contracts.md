# 1025：统一 Diagnostics 响应契约与运行时校验

## 状态

已采纳并实施（2026-10-12）。

## 背景

Diagnostics 命令的解析职责分散在 Settings 和 Metrics contract 模块；UI 可接受 Rust producer 不会产生的 `LogLevel`、LLM 连接状态组合和任意长度的 metrics phase 数组。相关结构信息已经存在于 Common/Agent Rust 类型，但前端没有全部复用。

## 决定

- `LogInfo.level` 使用 Common 的 `LogLevel` 类型，使生成 IPC contract 成为同一闭枚举；JSON 字符串值与现有序列化格式不变。
- `LlmConnectionReport` parser 使用生成的 status/reason 值集，要求 Rust DTO 的 `provider`、`model` 字段存在，并拒绝不匹配的 status/reason 组合；通过后先物化生成 DTO，再投影 shell view。
- Agent 将 metrics phase 数量作为数组的唯一尺寸常量公开给 IPC generator；生成 TypeScript 常量并要求 UI parser 恰好接受该长度。
- Log、Shell availability、credential status 与 performance metrics parser 和对应类型统一归 `contracts/diagnostics.ts`；Settings contract 不再持有 diagnostics parser。UI 视图、命令 adapter 和测试统一经 diagnostics contract owner。
- IPC checker 校验生成数量与 Rust 常量相等，并保证 parser 继续消费生成的 DTO/枚举。

## 影响

不会改变命令、事件或持久化字段，也不改变当前 metrics phase 数量与日志 level 的序列化值。UI 内部只接受 producer 可生成的固定响应；新增的 generated metrics 常量用于编译期/运行时数组长度约束。

## 替代方案

- 在 UI 保留手写状态枚举和不定长 metrics 数组：拒绝，会让 Rust 固定 DTO 演化时留下漂移窗口。
- 将 Diagnostics parser 继续放在 Settings contract：拒绝，日志/指标/凭据状态由 diagnostics 命令适配器消费，与 Settings payload 校验职责不同。
- 在 UI 再手写 phase count：拒绝，Rust 的固定数组长度是唯一事实来源。

## 验证

- `pwsh -NoProfile -File scripts/generate-ipc-contracts.ps1 -Check`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`（79 handlers）
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`、`corepack pnpm run test:run`（144 文件、1196 测试）与 `corepack pnpm run build`
- `git diff --check`

## 回滚

若 metrics 阶段结构有意改变，应先调整 Agent 的 typed response 与 Rust 常量，再重新生成 IPC 并同步 UI 校验；不在前端单独扩大数组接收范围。
