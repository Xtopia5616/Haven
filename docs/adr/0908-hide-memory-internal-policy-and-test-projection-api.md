# 0908：收回 Memory 内部策略与测试投影 API

## 状态

已接受并实现（2026-10-11）。

## 背景

Memory 根仍无条件导出一组只有 Memory 内部调用的实现符号：可见事实集合 helper、volatile predicate 与 opposite-polarity helper、矛盾维护阈值、transcript commit 容量常量，以及 branch/recovery event type 字符串。`ProjectionCutoff` 也只参与 SessionStore 内部 rollback 解析。它们不是跨层 domain capability：暴露后，下游可以依赖安全过滤实现、维护调参和事务容量细节。

`PartialMessageCheckpoint` 描述 raw `partial_messages` 行，目前只有 raw `Database` 的读取/原子 take 测试入口返回它。生产流式文本生命周期已由 Agent `PartialStore` 与 Memory `SessionStore` 的 promote/discard 操作承接，生产消费者不需要检查 raw checkpoint 行。

## 决定

- 从 `haven_memory` 根移除仅由 Memory 内部使用的 fact helper、矛盾阈值、transcript 容量、branch/recovery event type 与 `ProjectionCutoff` 导出，并将实现可见性收回到 crate 或模块内部。
- `PartialMessageCheckpoint` 只在 `test-support` 下从 crate root 导出，以支持 raw `Database` 测试；默认生产构建不暴露该 SQLite 投影 DTO。
- 保留被公开 DTO 字段或 store 方法签名实际引用的类型，即使下游主要通过类型推断使用；保留跨层消费的 fact safety/normalization 和 contradiction policy capability。

## 影响

- Memory 生产根 API 不再把内部事务/容量策略和 raw partial row shape 作为可依赖契约。
- 过滤结果、矛盾判定、事件写入与回放、partial promotion/discard 的行为不变。
- 不改变数据库 schema、持久化字段、事件 payload 或用户可见行为。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 将这些符号保留在根 API 并约定下游不要调用：拒绝，因为 Rust 访问性会继续允许消费者绑定到内部策略。
- 删除 `PartialMessageCheckpoint` 类型：拒绝，因为 test-support 下 raw Database fixture 仍需要可见返回类型。
- 收回 `ActiveBranchPoint`、recall result diagnostics 或 store 返回 DTO：拒绝，因为它们仍构成公开方法的真实输入/输出合同。
