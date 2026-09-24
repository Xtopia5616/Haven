# ADR 0285：App 事实管理命令通过 MemoryFactStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：`list_facts`、`add_fact`、`delete_fact` 三个 App 命令的 facts 持久化边界
- 关联：[ADR 0019](0019-memory-fact-graph-write-boundary.md)、[ADR 0020](0020-memory-fact-query-ranking-boundary.md)、[ADR 0022](0022-memory-fact-maintenance-boundary.md)、[ADR 0254](0254-remove-unused-memory-recall-forwarder.md)

## 背景

`commands/memory.rs` 直接通过 `state.db.run_blocking` 调用事实列表、用户事实写入和删除方法，并在 App 命令中选择 source、过滤不可见事实。SQLite 调度和事实读取策略因此跨过了 Memory 边界。Facts 是独立记忆域数据，不属于 `SessionStore`；本切片也不需要把 recall 重构成 `MemoryReader`。

## 决策

1. 在 `haven-memory` 新增具体的 `MemoryFactStore`，负责这三个命令所需的 async blocking-pool 调度和 Database 调用。`ApplicationRuntime` 在组合时创建并持有它，App 命令只调用 typed 方法。
2. `MemoryFactStore::list_facts` 保留 source 为空或缺省时调用 `Database::list_facts`、source 非空时调用 `list_facts_by_source` 的规则，并继续使用 `MemoryRetriever::filter_visible_facts` 过滤敏感 SPO、隐藏 credential-like 事实及 redact 敏感来源摘要。
3. `add_fact` 的输入 trim、必填校验、敏感谓词/对象拒绝、tags trim/filter 继续由 App adapter 负责；校验失败仍直接返回原 IPC 文案，不进入存储或日志路径。`MemoryFactStore` 接收已规范化输入并调用原 `set_user_fact`。
4. `delete_fact` 继续调用既有 `Database::delete_fact`，包括对缺失 ID 的既有成功语义。三个命令的数据返回类型及 `log_err` 命令上下文保持不变。
5. `SessionStore` 继续只负责 session 数据。MemoryFactStore 不创建第二个 runtime、通用 storage trait 或 MemoryReader，也不改变 `recall_memory`、`run_memory_maintenance` 或 MemoryService 所有权。

## 后果与未改变语义

App facts 命令不再直接依赖 `Database::run_blocking` 或事实 repository 方法；SQLite blocking 工作和 list 可见性策略归 `haven-memory`。命令校验、`Fact` IPC 返回 shape、source 选择、有效置信度排序、user-fact 图谱规则、tags 和删除结果语义均保持不变。未修改 schema、SQL、持久化数据、recall 或维护；无需重置用户数据。调用方 future 被丢弃时，已启动的 `spawn_blocking` 工作仍可能继续。

## 验证

- MemoryFactStore 测试覆盖 all/source 列表、空 source 等价 all、敏感事实过滤与来源摘要 redact、user fact tags 往返和删除。
- App helper 测试覆盖 subject/predicate/object trim、空字段和 credential-like predicate/object 错误文案、tags trim/filter。
- 运行 `cargo fmt --all`、haven-memory 与 haven-app-binary focused tests、相关 Clippy；资源允许时运行 workspace 和 UI 门禁。

## 替代方案

- 继续让 App 调度 blocking closure：保留了原有边界穿透，拒绝。
- 把 facts 放进 `SessionStore`：混合独立记忆域和 session 生命周期，拒绝。
- 把这三个操作加到 `MemoryService`/AgentLayer 或新增泛化 trait：会让 CRUD 存储依赖无关的 recall/Agent service 或扩大公共抽象，拒绝。

## 回滚

回退本切片提交，恢复 commands 中的原 blocking closure 并删除 `MemoryFactStore`、runtime wiring 与本 ADR 即可。无 schema 或用户数据变化，不需要重置。
