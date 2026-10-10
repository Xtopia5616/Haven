# ADR 0887：将 Messaging transport 收口在领域 facade 后

## 状态

Accepted — 2026-10-10

## 背景

ADR 0886 将身份校验放到 `MessagingService`、`SessionMailbox` 和 JSONL transport 的边界，但 `inbox` 仍是公开模块，调用方可直接构造 `InboxBus` 并调用 storage 方法；`MessagingService::new(MessageTransport)` 也让普通生产依赖能替换 transport，绕过唯一装配路径。跨 crate 的 Agent/Tools 测试依赖这些生产可见入口，导致测试便利性与正式 API 混在一起。

## 决定

- `MessagingService` 是跨 crate 的消息操作 facade；`inbox` 与 `messaging_service` 实现模块私有，领域 DTO 从 crate 根导出。
- `InboxBus`、registry/storage helpers 和直接操作方法只在 `haven-messaging` 内可见。删除 transport 上无消费者的 `root` 和重复 `deliver_system_notice` 操作；system notice 统一经过 `MessagingService`。
- 普通生产构建只通过 `MessagingService::default_root` 或 `with_session_mailbox` 创建 facade。低层 transport trait 和 `new_for_test` / `file_transport_for_test` 仅在非默认 `test-support` feature 下跨 crate 可见，供故障注入与隔离目录测试使用。
- 对其他 crate 逐项审查公开实现类型、构造器和 getter；只有该 crate 的正式领域 facade、跨层 DTO 和确有业务语义的 port 保持公开。具体实现不能仅因测试调用而继续公开。

## 替代方案

继续公开 `InboxBus`，依赖文档约定调用方使用 `MessagingService`。拒绝：这不能阻止新增直接调用，也不能保证 claim/ack 生命周期、统一校验和 transport 恢复语义由唯一 owner 控制。`pub(crate)` 与私有模块能让绕行在编译阶段失败。

## 影响与兼容性

这是测试版本内的 Rust API 破坏性变更；不保留兼容 re-export。JSONL envelope、registry、SQLite 和 IPC wire shape 均不变，不需要重置数据。生产行为仍由同一 `MessagingService` 和 JSONL transport 实现。

## 验证

执行 `cargo fmt --all -- --check`、`cargo test --workspace --locked` 和 `cargo clippy --workspace --locked -- -D warnings`。`test-support` 只由 Agent/Tools 开发依赖启用，workspace 测试覆盖其跨 crate 测试消费者。

## 回滚

若编译发现正式的生产消费者需要 transport 实现，应将该操作提升为 `MessagingService` 的领域方法或明确的领域 port；不恢复 `InboxBus` 的公共构造与方法。
