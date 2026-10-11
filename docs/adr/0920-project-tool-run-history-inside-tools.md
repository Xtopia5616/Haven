# 0920：在 Tools 内投影与分页 ToolRun history

## 状态

已接受并实现（2026-10-11）。

## 背景

`ToolRunRow` 是 Memory 的持久化行模型，包含动态 `tool_args`、continuation `prompt`、本机 `log_path` 与其它仅供执行/恢复使用的字段。旧的 App management capability 将该 row 从 Memory 透传到 Tools，再暴露给 App；App command 自行解析字符串 `kind`/`mode`、筛掉非终态并映射 IPC DTO。虽然 mapper 有测试确保敏感字段不会序列化到 UI，但 App 仍能访问这些字段，且 history 的 `take(limit)` 发生在全量 SQLite 查询之后。

## 决定

- Memory 的 typed `ToolRunStore` 增加终态历史查询，可按 kind/session 过滤并在 SQLite 侧限页；持久化状态过滤由 Memory 的 `ToolRunStatus` 列表负责，最大页容量 200 由该 store 强制执行。全量读取只经单独命名的 background board hydration 方法，普通 history 查询必须传 limit。
- Tools 是 ToolRun 语义 owner：将持久化 `ToolRunRow` 转换为已有安全 typed `ToolRunView`，并通过 management capability 提供 board history 与有界 history query。持久化 kind 转为 `ToolRunKind`；scheduled mode 解析为 `ScheduleMode`。不认识的持久化 kind fail-closed 返回错误。
- `ToolRunHistoryQuery::new` 保留调用方请求；Memory store 将持久 history page 截断至 200，App IPC 默认仍请求 50。board hydration 使用具名的无上限 background 查询以保持现有 terminal background history 行为。
- App 不再依赖 `haven_memory::ToolRunRow`，也不再解析持久化 kind/mode 或实现终态筛选；它只将 `ToolRunView` 映射为现有 `ToolRunEvent` IPC DTO。
- Live board 与持久化 history 共用 `ToolRunView`，不新增第二种几乎相同的展示 DTO。`ToolRunView` 不含 `tool_args`、`prompt`、`log_path`、`watch_tool_run_id` 或持久化 `created_at`。

## 影响

不改变 SQLite schema、持久化状态、Tauri command 参数/响应字段、事件 channel、UI 类型、安全授权或恢复语义。App 的 history 输出仍为 terminal rows、同样的排序、同样的默认/最大结果数；查询现在先过滤终态再分页，减少无关行读取。未知 kind 错误从 App projection 移到 Tools persisted projection。Board history 不限条数以保持当前行为。

这是内部 Rust API 的有意破坏性收口：移除 `ToolRunManagementCapability` 和 App port 返回 `ToolRunRow` 的方法，改由 Tools typed view/query 契约取代，不保留兼容别名。

## 验证

- Memory 回归覆盖终态过滤、kind/session 条件与限页。
- Memory store 回归覆盖终态过滤、kind/session 条件、限页和超大 limit clamp；Tools 回归覆盖 typed kind/mode 投影、background board preview 上限、非 scheduled row 不显示 schedule mode 与未知 kind 错误。
- 全 workspace formatting、check、tests、Clippy；ADR 索引检查。
- 静态调用图确认 App 不再引用 `ToolRunRow` 或 `list_persisted_tool_runs`，history limit 不再在 App 装载 rows 后应用。

## 替代方案

- 继续在 App 过滤并仅加 SQL limit：拒绝，因为 App 仍会持有存储实现行并需要理解 ToolRun 持久化字段。
- 新建 `ToolRunHistoryView`：拒绝，因为现有 `ToolRunView` 已是同一组安全展示字段，复用能避免重复契约。
- 把 board history 一并截断至 200：拒绝，因为这会改变当前 board hydration 可见的 terminal background history 范围；仅用户请求的 history query 有明确页上限。

## 回滚

可恢复旧 capability 和 App row mapper；不需要持久数据迁移或 schema 回滚。
