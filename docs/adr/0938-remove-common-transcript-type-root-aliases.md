# 0938：移除 transcript types 的 Common 根 aliases

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven_common` 根曾重新导出 `CanonicalMessage`、`CanonicalRole`、`CanonicalToolCall`、`ContentPart`、`FollowUp`、`InjectSource`、`MessageAttachment`、`PEER_KICKOFF_PREFIX` 与 `TranscriptMessageKind`。这些定义都位于公开的 `haven_common::types` 模块，仓内没有根路径 import 或引用；生产代码与测试统一使用定义模块路径。

## 决定

- 删除 Common 根的九个 transcript type/constant aliases。
- 统一保留 `haven_common::types::*` 作为这些定义的 Rust 路径。
- 不把类型全部扁平导出到根，也不改动定义、字段、Serde 或 transcript owner。

## 影响

这是未发布 workspace Rust API 的破坏性收口，不改变消息内容、事件、数据库或 provider wire contract。被移除的根路径在仓内没有消费者。

## 验证

- 静态搜索确认没有 `haven_common::<transcript item>` 或根 import。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留根 alias 作为便利入口：拒绝。仓内所有消费者已通过定义模块访问；两条路径只增加选择，不表达不同 owner。

## 回滚

若未来 transcript contract 移至 Common 根，应在迁移定义与全部 consumer 后保留一个规范路径；不要同时恢复模块 alias。
