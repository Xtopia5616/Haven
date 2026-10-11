# 0928：统一 Common ToolRun contract 根路径

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven-common` 已从 crate root 导出 `ToolRunCompletionPayload`，但其单类型源码模块 `tool_run_completion` 仍为 `pub mod`，可形成第二条导入路径。`ToolRunLease` 则位于 `#[doc(hidden)] pub mod tool_run_lease`，Common 根没有导出类型；Memory 和 Tools 因而必须直接依赖 Common 模块路径。`doc(hidden)` 只影响文档展示，不限制下游访问，也没有表达该模块是稳定领域子语言的意图。

这两个类型分别是跨 crate 的 completion payload 和共享 lease value contract；它们各自只有一个生产类型，源码模块只是物理布局，不需要成为 Common 的公开子 API。

## 决定

- 将 `tool_run_completion` 与 `tool_run_lease` 源码模块改为私有。
- 由 Common crate root 导出 `ToolRunCompletionPayload` 与 `ToolRunLease`，作为唯一跨 crate 路径。
- Memory persistence 与 Tools completion owner 统一从 `haven_common` 根导入 `ToolRunLease`；completion payload 保持原有根导入。
- 不改变 lease 状态、不变量、claim token 比较或 completion wire/persistence 字段。

## 影响

这是未发布 workspace API 的破坏性收口，不保留旧模块路径。无序列化、数据库、IPC、lease 竞争或运行时行为变化；`ToolRunLease<T>` 的泛型时钟类型继续允许进程内 `Instant` 与持久层 UTC 时间表达各自时钟域。

## 验证

- 静态搜索确认生产代码不再使用 `haven_common::tool_run_lease` 或 `haven_common::tool_run_completion`。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留 `doc(hidden) pub mod` 并依靠调用方约定：拒绝，编译器仍允许依赖实现路径。
- 把 lease contract 移到 Memory 或 Tools：拒绝，二者共享同一通用状态规则，任一业务 owner 都会引入反向/重复依赖。
- 将 `ToolRunLease` 与 `ToolRunCompletionPayload` 合并：拒绝，一个描述竞争/有效期，另一个描述完成交付字段，生命周期与时钟语义不同。

## 回滚

若出现外部调用方依赖旧模块路径，可恢复 `pub mod`，但仓内仍应统一使用 Common 根导入。lease 规则改变时，应先保留跨时钟域的单一 lease contract，再另立 ADR 讨论字段或方法语义。
