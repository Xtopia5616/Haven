# 0940：将 ManagedAssetRegistry 限制为 Tools 内部实现

## 状态

已接受并实现（2026-10-11）。

## 背景

`ManagedAssetRegistry` 持有 asset-id 到宿主路径的映射、session/pending lease 与生成媒体清理锁，是 Tools 的进程内实现。App 生产代码只依赖 `ManagedAssetLifecyclePort`；具体 registry 只用于 Tools 内部装配，以及 App 隔离文件系统测试的 fixture。此前 `ManagedAsset` 与 `ManagedAssetRegistry` 无条件从 Tools crate root 导出，允许生产消费者绕开 lifecycle port 直接读取或修改 registry。

## 决定

- 默认生产构建只从 Tools crate root 导出 `ManagedAssetLifecyclePort` 和其签名所需的 `GeneratedMediaCleanupGuard`。
- `ManagedAsset` 与 `ManagedAssetRegistry` 在 Tools 内部保持 `pub(crate)` 可达；仅非默认 `test-support` feature 向跨 crate 测试导出这两个类型。
- App 的生产调用继续使用 lifecycle capability，测试 fixture 继续显式开启 `test-support`。

## 影响

这是未发布 workspace Rust API 的破坏性收口。App/Tools 的生产数据流、文件路径校验、lease 与 GC 顺序不变；测试构造入口仍可用。数据库、配置、IPC 与持久化数据均不变，无需重置。

## 验证

- 静态搜索确认 App 生产代码只接收 `ManagedAssetLifecyclePort`，无 `ManagedAsset` row 消费者。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引及 `git diff --check`。

## 替代方案

- 保留 concrete registry 供 App 直接调用：拒绝。App 已通过 port 完成其生命周期职责，直接暴露 implementation state 扩大跨层 API 且允许绕开 owner。
- 将 `ManagedAsset` 作为默认公开 DTO：拒绝。跨层消费者无需读取 registry 内的宿主路径行；若未来出现真实只读消费者，应新增不含内部 lease 状态的显式 snapshot DTO。

## 回滚

若未来 App 需要新的资产生命周期操作，应将具名能力加到 `ManagedAssetLifecyclePort` 并由 Tools 实现，不恢复生产构建中的 concrete registry 导出。
