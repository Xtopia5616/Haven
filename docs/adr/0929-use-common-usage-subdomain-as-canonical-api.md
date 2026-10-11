# 0929：让用量 contract 只经 Common usage 子域导出

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven-common::usage` 是文档明确的跨层用量值与策略唯一 owner，仓内消费者全部使用 `haven_common::usage::{...}`。Common 根仍额外 re-export `CacheAccounting`、`CacheDiagnosticOutcome`、`CacheDiagnostics`、`CacheUsageSource`、`LlmCallKind` 和 `PromptCacheStrategy`；静态搜索确认没有任何根路径消费者，留下了一组无用的第二入口。

命名规则允许真实领域子语言保留命名模块，但同一实体不能同时拥有 crate-root alias 和子域规范路径。

## 决定

- 保留公开 `haven_common::usage` 模块作为唯一用量 contract 入口。
- 删除 Common root 上六个未使用的用量类型 re-export；类型与模块可见性保持不变。
- 文档明确：独立领域子域模块一旦作为规范入口，其类型不再重复 re-export 到 root。

## 影响

这是未发布 workspace API 的破坏性收口，不保留无消费者的 Common root alias。所有仓内调用路径已经是规范 `usage` 路径，因此运行时代码、序列化、用量计算和事件/持久契约不变。

## 验证

- 静态搜索确认六个类型都没有 `haven_common::<Type>` 根路径消费者，且全部仍从 `haven_common::usage` 导入。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留根 re-export 作为便利别名：拒绝，仓内没有消费者，且会继续允许双路径混用。
- 将 `usage` 模块改私有并扁平导出：拒绝，Common 架构文档把它定义为显式领域 owner，根 namespace 也会丢失用量契约的边界语义。
- 为迁移保留 alias：拒绝，项目明确不要求向下兼容。

## 回滚

若未来证据表明 Common 根路径显著改善实际调用端 API，再重新评估 root 导出设计；恢复 alias 不应同时改变 `usage` 的 owner 或重新迁移仓内消费者。
