# ADR 0305：ActionService action 持久化通过 ActionStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`ActionService` 对 actions、scheduled trigger 和 completion outbox 的全部持久化调用
- 关联：[ADR 0236](0236-background-action-terminal-cas.md)、[ADR 0248](0248-background-action-terminal-commit-order.md)、[ADR 0215](0215-action-board-typed-view.md)

## 背景

`ActionService` 同时持有进程内 action board、并发门控、恢复与重试策略，也持有 `Arc<Database>` 并自行安排 `run_blocking`。这让 Tools 直接依赖 SQLite facade，并把业务编排和存储调度耦合在同一个服务中。后台终态 CAS、scheduled 状态转换、completion outbox、历史列表和重启清理仍有分散的数据库入口。

ActionService 的持久化读写有稳定且有限的业务面：completion claim/ack，action list/get/delete，异常 waiting scheduled row quarantine，重启中断标记，background create/session bind/terminal transition，以及 scheduled create/list/start/requeue/finish/cancel。终态 action 与 completion outbox 的原子事务和 transcript durable 后 ack 是既有正确性契约。

## 决定

1. `haven-memory` 新增并 re-export 异步 typed `ActionStore`。Store 私有持有 `Arc<Database>`，自行使用既有 SQLite blocking scheduler；它不提供通用调度方法，也不暴露 `Database`。端口使用具体业务名、参数和 `ActionRow`、`ScheduledActionRow`、`ActionCompletionOutboxRow`、`ActionStatus` 等既有类型。
2. ActionStore 覆盖 ActionService 当前全部持久化调用：outbox claim/ack、action list/get/delete、waiting scheduled quarantine、interrupted action marking、background save/session bind/finish/cancel/finish-with-completion，以及 scheduled save/list/start/requeue/finish/cancel。
3. `ActionService` 只保留一个 `Option<ActionStore>` 句柄，通过 `set_action_store` 注入。`haven-app-binary` 组合根从现有共享 `Arc<Database>` 构造 store，并通过 `StartupWiring` 交给 Tools；manager/runtime 不再把原始数据库绑定到 ActionService。App action commands 继续经 ActionService 获取历史，wire payload 不变。
4. ActionService 继续拥有内存 board、并发门控、CAS 胜者发布、quarantine/终态写失败重试、scheduled 恢复、生命周期事件和错误映射。Store 只负责具体持久化端口与 blocking 调度。
5. `finish_background_action_with_completion` 在 Memory 内复用既有单事务操作，只有 action 从 `running` CAS 成功才插入 outbox。Agent 只在 transcript/event 投影 durable 后调用 ack。普通完成、取消、restart cleanup、scheduled start/finish/cancel 仍使用既有 CAS 条件。
6. 未配置 store 时保留现有 headless 语义：action 和 scheduled 状态可在内存运行，后台终态以内存 first-wins 提交，restore 返回空结果，outbox ack 是 no-op；明确要求持久历史的 `list_persisted_actions` 返回配置错误。持久模式的错误和重试策略不迁入 Store。

该变化只收窄 Tools → Memory 的持久化边界，不改变 schema、IPC、action 状态值、对外事件、retry/recovery 策略或用户数据。

## 影响与验证

- 无 schema、IPC、配置或依赖变化；无需重置数据。
- ActionStore 行为测试覆盖 completion claim/ack、终态 CAS 胜者快照、scheduled claim/terminal CAS 与 malformed waiting row quarantine。
- ActionService 测试覆盖缺少 store 时的 memory-only 终态行为与历史查询配置错误；现有后台完成、outbox、scheduled 恢复、重试和生命周期集成测试保持。
- 结构复核：生产 `action_service.rs` 不含 raw `Database`、`run_blocking` 或直接数据库方法调用。
- 验收命令：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-tools`、`cargo test --locked -p haven-agent`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 让 ActionService 保留 `Arc<Database>` 并仅增加辅助函数：仍由 Tools 拥有存储 facade 和 blocking 调度，拒绝。
- 为 action persistence 定义通用 `run_blocking` 或 closure port：会暴露实现调度并允许上层任意 SQL/DB 操作，拒绝。
- 把 CAS 发布、retry/backoff 或 scheduled recovery 移入 ActionStore：混合持久化事务与进程生命周期策略，拒绝。
- 迁移全部 Job/MemoryWorker 生命周期并改 schema：超出 action 持久化边界，也非达成此次目标所需，拒绝。

## 回滚

代码回退恢复 ActionService 的 Database 持有与既有调用，删除 ActionStore、测试、本 ADR、README 索引和架构/路线图记录即可。无数据格式或迁移要求。
