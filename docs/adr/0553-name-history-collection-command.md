# ADR 0553：历史集合 IPC 命令使用 list_history

## 状态

已采纳并实施；命令从 `list_history` 扩展为带 Session 作用域的完整 history family，详见 [ADR 0679](0679-unify-session-runtime-and-history-terms.md)。旧名称只保留在本记录中说明原决策。

## 背景

Tauri `get_history` 接收 `limit` / `offset` 并返回 `Vec<SessionRecordDto>`。它调用的权威持久查询已命名为 `SessionStore::list_history`；前端 wrapper `getHistory` 和 IPC contract registry 则继续使用 `get`，使同一分页集合读取跨层出现两种动词。

## 决定

1. 将 Rust Tauri handler 和命令名从 `get_history` 改为 `list_history`。
2. 将 UI wrapper `getHistory` 改为 `listHistory`，生成请求类型、response alias 与 contracts metadata 都指向 `list_history`。
3. 同步更新 Tauri 注册、IPC 文档、输出契约清单和 IPC 检查脚本；重新生成 `generatedCommands.ts`，不手工维护生成内容。
4. 不保留旧 IPC 命令别名。请求参数、返回 DTO、分页顺序、SessionRecord 投影、错误语义和历史存储均不变。

## 替代方案

- 只改 UI wrapper：拒绝。Tauri command 本身仍是集合查询却使用 `get`，跨层术语仍不一致。
- 保留旧 command 名作为兼容 alias：拒绝。该 command 仅由当前 UI 调用；版本化 command map 会同步生成，双入口没有独立消费者价值。
- 把 `SessionStore::list_history` 改回 `get_history`：拒绝。Memory/App 其余集合读方法已经使用 `list`，且这会扩大不一致。

## 影响与验证

- 这是 Rust↔UI IPC 名称变更；仅更新当前应用内部消费者，不改 DTO、数据库、配置或持久数据。
- 验证：IPC contract generation/check、Rust workspace fmt/check/clippy/tests、UI check/tests/build 与 `git diff --check`。

## 回滚

同步恢复 Rust handler、Tauri 注册、UI wrapper、生成契约、metadata、检查脚本和文档中的 `get_history`；无需数据或配置重置。
