# ADR 0557：统一最近会话恢复 IPC 命令术语

## 状态

已采纳并实施；IPC 生成/一致性检查、Rust workspace 门禁和 UI 门禁均通过。

## 背景

App 命令 `get_last_conversation` 查询 `SessionStore::latest_session_record` 并返回 `Option<SessionResumeResponse>`。它只在 chat startup 使用；调用方将返回的 session transcript 投影到 UI，并对未完成 session 调用 `reopen_session`。命令名沿用早期 conversation 术语，既没有表达实际实体 `Session`，也没有体现它与 `get_session_for_resume` 共用 resume response projection 的关系。

## 决定

1. 将 Tauri command 改名为 `get_latest_session_for_resume`，与底层最新 session 选择和已有的 `get_session_for_resume` 术语对齐。
2. 将 App 内 store helper 改为 `latest_session_for_resume_from_store`；将 UI wrapper 与 startup dependency 改为 `getLatestSessionForResume`，将 startup 操作改为 `resumeLatestSession`。
3. 更新 Rust command 注册、Rust 安全/boundary registry、生成 TypeScript map、UI invoke owner、IPC 检查清单、IPC 文档、输出契约清单、测试与命名/路线图文档。
4. 不保留旧 IPC alias：仓库调用扫描显示唯一消费者是当前 UI，所有正式调用方与契约检查同步迁移。
5. 保持 `latest_session_record` 的排序、`SessionResumeResponse` DTO、无记录时的 `None`、error logging、terminal-session 过滤及 startup reopen 顺序不变。

## 替代方案

- 只改 UI wrapper：拒绝。Rust command registry、生成 map 与 IPC 文档仍会暴露旧术语。
- 保留 `get_last_conversation` 并新增同义 alias：拒绝。当前只有本仓 UI 消费者，双入口没有兼容消费者价值，且会继续让两个名称表达同一个契约。
- 改用 `get_latest_session`：拒绝。响应是用于恢复的 session projection，与 `get_session_for_resume` 共用 DTO 和语义边界，命令名应表达用途。

## 影响与验证

- 这是 App Tauri command 名称变更；request/response shape、数据库、持久数据、恢复行为和 UI 展示均不变。当前应用内所有消费者随同一版本更新，无需数据重置。
- 验证 IPC generator/check、`check-ipc-contracts.ps1`、Rust fmt/check/strict Clippy/workspace tests、UI check/tests/build 与 staged diff 检查。

## 回滚

同步恢复 Rust command、注册与 contract metadata、UI wrapper/startup dependency、生成 map、检查脚本、IPC/输出文档和测试中的 `get_last_conversation`；无需迁移或重置数据。
