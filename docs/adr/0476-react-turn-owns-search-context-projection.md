# ADR 0476：由 ReAct turn 层拥有搜索上下文投影

## 状态

已采纳并实施（2026-10-05）。

## 背景

`stream_step.rs` 管理 provider stream 的分片、attempt reset、checkpoint、web-search 消费队列和结束排空。`prepare_search_context` 不读写任何 stream 生命周期状态，只在 `turn.rs` 处理聚合 provider response 时被调用一次；同一 turn 层已经负责 web-search 返回事件、工具批次与 turn-end 分流。搜索响应投影因此依赖方向与职责归属不一致。

本候选只针对 provider server-side search items 的 transcript 投影与 turn outcome，不移动 `StreamForwarder`、checkpoint writer 或重试 prompt 流程。现有 `session_events` 提交与 canonical projection 仍通过 `EffectBatch` 单写路径。

## 决定

1. 将 `SearchContextOutcome`、`prepare_search_context` 及其单元测试从 `stream_step.rs` 移到 `turn.rs`，与聚合响应处理和 `web_search_return_effects` 放在同一 owner。
2. 混合 real tool + search response 继续交给 `execute_tool_batch`，由既有 tool commit 携带 search items；纯 search response 继续提交搜索上下文与 branch point 后进入下一 turn。
3. synthesized final 与 search 同响应时继续通过一个 `ToolCall` transcript effect 提交搜索内容；没有 Thought 时沿用共享 `step-*` message identity。保持现有返回 outcome 和 turn-end 行为。
4. `stream_step.rs` 继续拥有 provider stream 生命周期、队列、checkpoint 与 attempt 边界。实现不增加 crate、公共 API、持久化来源、schema、wire 或 provider 契约。

## 替代方案

- 保持现状会让 turn response policy 留在只负责 stream 生命周期的模块中，且 turn 层仍需跨模块导入 outcome。
- 新建独立 `search_context.rs` 会为一个只由 turn 使用的 helper 增加模块跳转，没有形成新的 owner 或消费者边界。
- 扩大到所有 web-search 投影和 stream pump 会把响应策略与 callback 队列生命周期混在同一切片，超出本次证据支持的范围。

## 验收与影响

- 将 synthesized-final identity 回归测试与实现一并迁至 `turn.rs`。
- `cargo fmt --all -- --check`：通过。
- `cargo test --locked -p haven-agent`：569 passed，1 ignored。
- `cargo clippy --locked -p haven-agent -- -D warnings`：通过。
- `scripts/check-crate-dependencies.ps1`：通过，内部 crate 依赖清单与 Cargo metadata 一致。
- `scripts/check-ipc-contracts.ps1`：通过，79 个 handler 的 Rust 注册、生成 TypeScript、审阅安全元数据和文档一致。
- `git diff --check`：提交前通过。
- 没有数据库、配置、IPC、安全或用户数据迁移影响；前后行为与事件顺序保持不变。

## 回滚

可将 outcome、helper 和测试移回 `stream_step.rs`，恢复原跨模块调用。回滚不需要数据 reset；应保留同一 `EffectBatch` 提交顺序和 synthetic final identity 断言。
