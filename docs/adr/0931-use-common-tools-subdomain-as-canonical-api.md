# 0931：让工具 contracts 只经 Common tools 子域导出

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven-common::tools` 是跨 crate 工具 schema、执行分类、retry safety 与 presentation contract 的领域子域。Common root 额外 re-export 其中 15 个类型，但 workspace 的生产代码和测试全部使用 `haven_common::tools::*`；静态搜索确认没有任何同名 root 路径 consumer。重复 export 只增加可选拼法，不提供现存调用便利或独立 contract owner。

## 决定

- 保留 `haven-common::tools` 作为工具 contracts 的唯一命名子域。
- 删除 `ToolAvailability`、`ToolCatalogGroup`、`ToolDef`、`ToolErrorClass`、`ToolExecutionOutcome`、`ToolIdentity`、`ToolManifest`、`ToolModel`、`ToolPolicy`、`ToolPresentation`、`ToolPrompt`、`ToolResultEnvelope`、`ToolRetrySafety`、`ToolRetryability` 与 `ToolSource` 的 Common root re-export。
- 不改类型、schema、serde、执行语义或消费者导入路径。

## 影响

这是未发布 workspace API 的破坏性收口，不保留未使用的 root aliases。工具定义仍由 Common 的 `tools` 子域共享；Tools crate 的执行 trait/adapter 与 Common 的工具 contract 继续分属不同 owner。

## 验证

- 静态搜索确认所列类型均无 `haven_common::<Type>` 或 grouped root import，且仓内消费者从 `haven_common::tools` 导入。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 同时保留根与子域路径：拒绝，重复入口没有消费者需求且会让调用者各自选词空间。
- 隐藏 `tools` 模块并将类型扁平导出：拒绝，工具 contract 本身就是稳定的领域子语言，namespace 帮助区分 schema 与别的 Common DTO。

## 回滚

若未来消费者证明根路径更适合作为唯一公共 contract，应将 `tools` 模块收为私有并把全体消费者迁移到根路径；不得同时恢复两套入口。
