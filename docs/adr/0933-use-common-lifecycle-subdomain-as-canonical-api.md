# 0933：让生命周期状态只经 Common lifecycle 子域导出

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven_common::lifecycle` 定义 session、transcript step 与 ToolRun 的规范生命周期词汇，模块注释明确其职责是 persistence、runtime 与 IPC 共用的 durable lifecycle vocabulary。Common root 又重复 re-export 了四个状态类型；workspace 调用方同时使用两种路径，且大部分走根路径，形成真实的双入口。

## 决定

- `SessionStatus`、`SessionStepStatus`、`SessionWaitingReason` 和 `ToolRunStatus` 只经 `haven_common::lifecycle` 导入。
- 迁移 workspace 内生产代码与测试，删除四个 Common root aliases。
- 保持状态值、Serde、持久化编码和转换逻辑不变。

## 影响

这是未发布 workspace API 的破坏性收口，不保留根 aliases。领域状态的来源由路径明确表示，ToolRun completion payload 等不属于 lifecycle 的 contract 仍留在其既有 API。

## 验证

- 静态搜索确认 workspace 没有生命周期状态的 Common root 导入。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留根路径并要求未来代码选择其一：拒绝，已有混用会持续扩大双入口。
- 将状态留在通用 `types` 模块：拒绝，状态有专属 owner 和明确的跨层生命周期语义。

## 回滚

若未来证据表明 lifecycle 命名模块不应作为唯一入口，应迁移所有消费者到另一个单一 owner，并删除 `lifecycle` 模块；不得保留重复 re-export。
