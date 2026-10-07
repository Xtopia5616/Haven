# ADR 0667：统一 ReAct 类型名大小写

## 背景

Agent 把 ReAct 品牌写作 `ReActEngine`、`ReActRunOutput`，但同域 context batch、actor barrier 和 future 使用 `ReactContextBatch`、`ReactLoopBarrier`、`ReactLoopFuture`。UI 的 phase 类型也写作 `ReactExecutionPhase`。这使类型名无法按同一项目术语搜索或分组。

## 决定

- PascalCase 类型及 enum variant 中统一保留品牌拼写 `ReAct`：`ReActContextBatch`、`ReActLoopBarrier`、`ReActLoopFuture`、`ReActExecutionPhase` 与 `ReActExecutionPhaseSnapshot`。
- snake_case 和 lowerCamel 名称继续使用 `react`，例如现有 `drain_react_context` 与 `reactExecutionPhaseStore`。
- 不改运行阶段值、actor mailbox 消息顺序、execution phase、事件或序列化契约。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`
- `scripts/check-adr-index.ps1`

## 回滚与重置

回滚只恢复 Rust 私有类型/variant 与 UI 本地类型名称；无 IPC、数据库或配置影响，无需重置。
