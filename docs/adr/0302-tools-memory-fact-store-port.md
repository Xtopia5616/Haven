# ADR 0302：MemoryTool 通过 MemoryFactStore 访问事实

- 状态：Accepted
- 日期：2026-09-25
- 范围：`haven-tools` 内置 MemoryTool 的事实 search/list/remember/forget 与 keyword recall fallback
- 关联：[ADR 0019](0019-memory-fact-graph-write-boundary.md)、[ADR 0020](0020-memory-fact-query-ranking-boundary.md)、[ADR 0021](0021-agent-memory-embedding-boundary.md)、[ADR 0028](0028-agent-fact-inference-boundary.md)、[ADR 0285](0285-app-memory-fact-store.md)、[ADR 0254](0254-remove-unused-memory-recall-forwarder.md)

## 背景

`MemoryTool` 原先持有 `Arc<Database>`，并在工具实现中调度 blocking closure，直接执行事实搜索、列表、用户事实写入和 triple 删除。它也在 recall slot 未绑定时直接创建 `MemoryRetriever`。因此 Agent 工具适配层知道数据库调度和 facts repository，而 App fact CRUD 已有的 `MemoryFactStore` 只覆盖了较窄的 CRUD 调用。

事实工具操作属于 Memory 持久层；参数校验、凭据拒绝、risk/session/cancel 语义和 JSON 输出仍属于 Tools adapter。向量/embedding-aware recall 仍由桌面注入的 `MemoryRecallPort` 提供，MemoryService/MemoryWorker 的召回所有权与优先级不变。

## 决定

1. 扩展现有 `MemoryFactStore`，保留现有 App fact CRUD API，并增加异步 typed ports：可选 exact subject 的 scoped search、exact-subject list、跨 subject recent list、按 `(subject, predicate, optional object)` 删除，以及 `MemoryQuery -> MemoryRecall` 的 keyword-only fallback。接口表达操作意图，不暴露连接或数据库对象。
2. `MemoryFactStore` 负责 blocking-pool 调度。Search 与 list ports 使用 `MemoryRetriever` 的共享敏感事实过滤和 source snippet redact，并保留底层搜索/列表返回顺序；工具在 search 入参边界继续规范化查询、在输出前按原 limit 截断。跨 subject 列表继续使用 `Database::list_facts` 的原排序，exact subject 列表继续使用原 subject 查询排序。
3. `MemoryTool` 仅持有 `Option<MemoryFactStore>` 和既有 `MemoryRecallSlot`。它负责参数校验、敏感 predicate/object 写入拒绝、risk/session/cancellation 行为和现有 JSON shape；事实读取与写入均调用 typed store，不持有或调度 raw Database。
4. Recall 先调用已绑定的 `MemoryRecallPort`；只有 slot 未绑定时，调用 `MemoryFactStore::recall_keyword`，按 `MemoryRetriever::retrieve(query, None)` 生成 keyword-only 结果。该 fallback 不获取 embedding vector，不改变 MemoryService/MemoryRecallPort 的 embedding-aware recall 语义。
5. Builtin composition 在拥有 `AdminContext` 时从其可选 DB 创建 `MemoryFactStore` 并传给 MemoryTool。AdminContext 对其他 admin surfaces 的职责不变。

## 影响与验证

- 无 schema、IPC 或 Cargo 依赖变更，无需重置数据。
- MemoryFactStore 测试覆盖 scoped search、敏感可见性、exact/recent list 顺序、triple delete 和 keyword fallback 的命中/空结果。
- MemoryTool 测试覆盖跨 subject search/list、敏感值拒绝、空 recall/list/search、已绑定 recall slot 优先、已有参数与输出契约。
- 不以日志、网络或真实用户目录作为测试前提。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-tools`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 保留 MemoryTool 的 Database/blocking 调用：继续让 Tools 穿透 Memory 持久层，拒绝。
- 将操作并入 MemoryService 或新增通用 storage trait：会把纯 facts CRUD 与 recall 编排/Agent service 或泛化接口耦合，拒绝。
- 让工具 fallback 获取向量：会改变现有 desktop MemoryRecallPort 优先级和 embedding-aware recall owner，拒绝。

## 回滚

恢复 MemoryTool 原 Database 路径并移除新增端口及本 ADR、README 索引和路线图记录即可。无数据格式或依赖变更，不需要重置。
