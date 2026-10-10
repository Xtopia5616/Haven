# 0897：通过生命周期 port 隔离受管资产注册表

## 状态

已接受并实现（2026-10-10）。

## 背景

App 的上传持久化与媒体清理从 `ToolsFacade.share_services().assets` 取得 `ManagedAssetRegistry`，直接调用具体注册表的登记、租约快照、清理锁和修剪方法。这个注册表持有工具输出与会话附件共用的进程内状态；App 需要的只是上传登记和引用对账能力，不应依赖该状态容器的实现类型。

清理仍由 App 编排，因为它同时协调 `SessionStore` 的持久引用、上传目录和生成媒体目录；Tools 继续持有资产注册表及生成媒体写入/清理互斥状态。

## 决定

- 在 `haven_tools` 暴露 `ManagedAssetLifecyclePort`，包含上传路径登记、session lease、清理互斥、受保护路径读取和注册项修剪操作。
- `ToolServices.managed_assets` 提供该 capability port，不再向消费者暴露 `ManagedAssetRegistry`。
- App 将该 port 作为 `AppServices.managed_assets` 注入清理 worker 和上传持久化流程；这些调用不再从 `share_services()` 取得注册表。
- `ManagedAssetRegistry` 仍是 Tools 内部唯一状态 owner；内置工具和测试可以使用其具体实现。

## 影响

- 上传原子提交后登记、pending/session lease 保护、generated-media 写/清理排斥、SessionStore 引用失败时 fail-closed，以及清理后修剪的顺序不变。
- App 继续拥有目录遍历、文件删除、上传与持久引用协调；Tools 继续拥有 asset id 到路径的进程内映射、lease 和 TTL。
- `share_services()` 的其他具体字段以及 `ToolRegistry` 可变入口仍待 §5.7 审查，本 ADR 不代表全仓边界审计完成。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- App 继续直接持有 `ManagedAssetRegistry`：拒绝，因为应用代码会与 Tools 的进程内状态容器绑定。
- 将整个文件清理流程移入 Tools：拒绝，因为清理需要同时协调 App 拥有的上传目录和 `SessionStore` 引用；App 负责清理事务，Tools 只提供生命周期 capability。
