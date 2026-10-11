# 0960：将 ScheduledToolRunSpec 限制到测试 API

## 状态

已接受并实现（2026-10-11）。

## 背景

`ScheduledToolRunSpec` 是 `ToolRunService` 的调度输入，字段包括 schedule 规则、执行参数以及由 Tools 注入的 `session_id`。生产构造点位于 Tools builtin 内部；跨 crate 的 `ToolRunTestSupportPort::schedule` 只供非默认 `test-support` 构建使用。Tools crate root 此前仍在默认构建中无条件导出该类型，使其他生产 crate 可以直接构造内部调度输入，但没有真实生产消费者。

## 决定

- `haven_tools` 默认 crate root 不再导出 `ScheduledToolRunSpec`。
- 仅在 `test-support` feature 下从 crate root 导出该类型，供 Agent/App 的跨 crate 测试 fixture 使用。
- Tools 内部生产代码继续通过 `tool_run_types` 模块使用该类型；调度行为、字段和 ToolRun contract 不变。

## 替代方案

- 保留默认 root export：会继续让无生产消费者的内部调度 DTO 成为下游可构造 API。
- 将类型整体设为私有：会破坏现有的跨 crate `test-support` schedule fixture，且无需移除 Tools 内部构造能力。

## 影响

默认生产依赖不能再通过 `haven_tools::ScheduledToolRunSpec` 构造调度输入；Tools 内部与启用 `test-support` 的测试消费者保持可用。持久化、IPC、ToolRun 生命周期和运行行为均未变化，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-app-binary --lib`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

若未来出现真实的生产跨 crate schedule producer，应先确认其是否属于 Tools 的生命周期 owner；优先新增具名 Tools 操作，而不是恢复默认导出可由下游任意构造的输入 DTO。
