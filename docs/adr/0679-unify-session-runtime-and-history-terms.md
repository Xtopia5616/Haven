# ADR 0679：统一 Session 运行态与持久历史术语

## 状态

已采纳并实施。

## 背景

项目把多种生命周期都称为 sessions/history：`get_sessions` 只枚举当前进程中驻留且未终结的 Agent session actor；持久化历史由 `SessionStore` 查询；Memory 的 SQLite 查询缓存又以 `_sessions` 为 key。全量清理入口名为 `clear_history`，但实际会删除所有持久 Session、取消所属 ToolRun、清理 session trust，并释放运行态资源。与此同时，App 的 `history` 命令模块服务于 Session，而 ToolRun 有自己的独立历史命令。

这些名称隐藏了 runtime 与 persisted 两类状态、单条与全量删除边界，也让 Session history 与 ToolRun history 发生词汇碰撞。

## 决定

1. 当前进程中驻留且未终结的 actor 投影使用 `list_runtime_sessions`，响应类型为 `RuntimeSessionListResponse`，UI wrapper 和 startup 端口使用 `listRuntimeSessions`。
2. 持久会话查询明确使用 SessionHistory 作用域：App 命令模块为 `session_history`；命令与 `SessionStore` 方法使用 `list_session_history`、`count_session_history`、`search_session_history`、`search_session_history_paginated`、`count_session_history_search`、`search_session_history_filtered` 和 `export_session_history`；Session 相关前端 wrapper 收纳在 `sessionCommands.ts`，query request aliases 使用 SessionHistory 领域名。
3. `Database` 的存储列表 API 命名为 `list_persisted_sessions`。列表缓存使用 `session_history_page` key 与对应的 get/put/invalidate 名称。
4. 删除所有持久会话的命令、Agent façade、executor 和 store API 统一叫 `delete_all_sessions`。Supervisor 内部清理 actor registry、pending queue、confirmation 与 session trust 等运行状态的临界区 helper 叫 `clear_session_runtime_state_locked`；应用关闭入口叫 `clear_session_runtime_state_for_shutdown`。删除没有生产消费者的旧全局运行态清理 API。
5. 删除旧 IPC 名和内部 Rust/UI 别名，不增加兼容入口。更新 `docs/naming.md`、IPC 目录、输出清单、检查脚本和架构路线图。此前关于集合读取动词与历史命令名的 [ADR 0549](0549-use-list-for-ui-collection-queries.md) 和 [ADR 0553](0553-name-history-collection-command.md) 按此记录更新当前状态。

## 替代方案

- 继续统一称作 history：拒绝，因为命令实际删除的是会话实体，也会清除多个运行时 owner。
- 把 runtime snapshot 并入持久历史查询：拒绝，actor snapshot 不包含终态会话，二者来源、更新频率和生命周期不同。
- 只改 Tauri 命令：拒绝，Store、executor、UI wrapper 和 cache key 的泛名会继续掩盖同一边界。

## 影响与验证

- Tauri command 名称与 Rust/UI 内部 API 均发生破坏性重命名，不改变请求/响应字段、查询排序、过滤、分页、删除顺序、事件和运行时生命周期。
- 不改 SQLite schema 或持久数据，因此无需数据库重置。无需配置迁移；应用需由匹配的前后端版本构建。
- 已执行生成契约、IPC 目录和 Rust workspace 编译检查；测试未运行。

## 回滚

同步恢复旧 handler、注册、Store/executor API、缓存 key、UI wrappers、生成契约、审计脚本与文档名称；无需持久数据迁移或重置。
