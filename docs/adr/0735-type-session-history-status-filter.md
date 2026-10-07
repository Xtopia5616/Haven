# ADR 0735：Session history 查询复用严格状态过滤 enum

## 状态

已采纳并实施。

## 背景

`search_session_history_filtered` 与 `export_session_history` 都将可选 `status` 作为精确过滤条件传给 Memory `SessionHistoryFilter`，但 Tauri 与 query DTO 都将它声明为 `Option<String>`。唯一 production UI caller 是 `MemoryView.filterParams`；状态菜单只有 `completed`、`paused`、`error`，空选择映射为 `null`。`export_session_history` 没有 UI caller，但共享同一持久 Session 查询。

持久 `sessions.status` 的闭合集合是 `pending`、`running`、`paused`、`completed`、`error`，schema 有对应 CHECK。SessionStore 的 history query 以精确状态筛选；paused 继续使用原专用 SQL 分支。Common `SessionStatus` 是生命周期 owner，但它的自定义 Serde 解码会把未知字符串 fail-safe 映射为 `Error`；若直接拿来解析 IPC 请求，拼写错误会被错误地当成 `error` 过滤。

MemoryView 的加载失败由当前 query catch 清空结果、报告错误并结束 loading；用户可再次更改筛选或重新加载。过滤是只读，不修改配置、授权或数据库。

## 决定

- 在 Memory history-query owner 增加严格 Serde enum `SessionHistoryStatusFilter`，包含全部五种持久 Session 状态，并由 `SessionHistoryFilter.status`、两个 Tauri handler 共用。
- App handler 接受 `Option<SessionHistoryStatusFilter>`；Memory query 在进入现有数据库 string predicate 前通过 `as_str` 映射成精确状态文本。
- IPC generator 产生 `SessionHistoryStatusFilterInput`；MemoryView 从生成的值列表校验 `MaterialSelect` 的字符串 callback，UI 菜单只展示现有三种筛选值，空选择仍发 null。
- 保留 `SessionStatus` 的生命周期及未知值 fallback、paused SQL、分页/日期/搜索与缓存语义，数据库字段和 schema 不变。

## 替代方案

- 保留任意字符串并由 SQLite 返回空列表：拒绝。无效筛选值不能静默伪装成有效查询。
- 直接在 IPC 使用 `SessionStatus`：拒绝。它的 tolerant Deserialize 会把未知输入映射为 Error，不满足闭合请求契约。
- 过滤 enum 只列 UI 当前展示的三种状态：拒绝。底层持久历史和 export query 还允许按 pending 或 running 精确筛选。

## 影响与验证

两个命令生成的可选 `status` 从 `string | null` 收窄为 `SessionHistoryStatusFilterInput | null`。无效和非规范大小写输入在 Serde 解码阶段拒绝，而 `null` 仍表示不过滤。无数据库、配置或授权生命周期变化，不需要数据重置。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check` / `test:run`（125 files / 992 tests）/ `build`、`scripts/check-ipc-contracts.ps1`（80 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-adr-index.ps1`（718 ADRs）与 `git diff --check` 均通过。

## 回滚

如回滚，需恢复 `SessionHistoryStatusFilter` 与 query DTO 的 String 字段、两个命令签名、MemoryView 的生成类型 guard、generated request、IPC/naming/roadmap 文档和本 ADR 索引；不涉及持久数据迁移。
