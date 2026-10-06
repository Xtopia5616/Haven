# ADR 0538：区分录音采集状态与 App 状态响应

## 状态

已采纳并实施；Rust、UI 与 IPC 生成门禁通过。

## 背景

`haven_input::RecordingState` 是 input pipeline 的采集阶段枚举，取值为 Pending、Recording、Processing，由 `InputPipeline` 推进并被 stop/error 流程判断。`app-binary::commands::recording::RecordingState` 则是 `get_recording_state` 返回给 Tauri 的 App-owned shell 状态 DTO，只有 `is_recording` 与 `is_toggle` 两个字段。它们名称相同，却没有共享状态空间或状态 owner，读源码和检索符号时容易误认成同一个生命周期模型。

## 决定

1. 将 App command DTO 改名为 `RecordingStatus`，保留 `haven_input::RecordingState` 表示采集生命周期。
2. 保持 Tauri command 名 `get_recording_state`、序列化字段 `is_recording` / `is_toggle`、字段来源和运行行为不变；运行 IPC 生成器更新 TS 响应类型名。
3. 在 IPC 输出清单和全项目命名审计中说明两者的 owner 与状态范围。

## 替代方案

- 合并两类状态：拒绝。Input pipeline phases 与 App shell/toggle snapshot 有不同 owner 和转移规则，合并会制造第二真源。
- 保留两个同名类型并依赖 crate/模块路径区分：拒绝。名称本身未表达状态范围，跨层检索和生成类型仍含糊。
- 改 JSON 字段或 Tauri command 名：拒绝。本次问题只在 DTO 类型命名，wire key 与命令行为无需变化。

## 影响与验证

- Rust DTO 与生成 TypeScript 接口名称变化；Tauri command 名、JSON shape、Input 生命周期、UI 数据和运行时行为不变。没有数据库或配置变更。
- 验证通过：`scripts/check-ipc-contracts.ps1`（81 个 handler 的 Rust 注册、generated TypeScript、安全登记和 IPC 文档一致）、`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`corepack pnpm run check`（0 errors / 0 warnings）、`corepack pnpm run test:run`（122 files / 974 tests）和 `corepack pnpm run build`。Rust 手动性能 profile 保持 ignored；ADR index（521 条唯一编号记录）与 `git diff --check` 通过。

## 回滚

恢复 App DTO 的 `RecordingState` 名并重新生成 IPC contract 即可；没有持久化或 wire 字段迁移。
