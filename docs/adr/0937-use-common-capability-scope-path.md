# 0937：统一 CapabilityScope 的 Common 导入路径

## 状态

已接受并实现（2026-10-11）。

## 背景

`CapabilityScope` 定义在 `haven_common::types`，由 Common 持有类型与 capability-key 规则。Tools 又在 crate root 重新导出同一类型，因此消费者可以写 `haven_common::types::CapabilityScope` 或 `haven_tools::CapabilityScope`。生产调用方已使用 Common 路径；仓内只有 Agent 测试使用 Tools alias。

## 决定

- `haven_common::types::CapabilityScope` 是唯一规范 Rust 路径。
- 移除 `haven_tools::CapabilityScope` 根 re-export，并将测试调用改为定义者路径。
- Tools 的 `OperationPolicy`、`ConfirmationReceipt` 等公开 contract 继续使用同一个 Common 类型；消费者需要显式命名时依赖其实际 owner。

## 影响

这是未发布 workspace Rust API 的破坏性收口，不改变运行时类型、授权行为、Serde/wire 形状或持久化数据。删除的只是第二个 source-level 导入路径。

## 验证

- 静态搜索确认没有 `haven_tools::CapabilityScope` 引用。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留 Tools 根 re-export 作为便利 alias：拒绝。仓内没有生产消费者，且类型定义和规则 owner 已是 Common；同时保留会让同一共享类型有两个规范拼法。

## 回滚

若未来证明 Tools 需要独立拥有 capability identity，应先迁移类型与校验规则的 owner；不要只恢复第二个 alias。
