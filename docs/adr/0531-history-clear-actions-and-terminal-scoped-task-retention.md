# ADR 0531：历史清理操作与任务历史边界

## 状态

已采纳并实施（2026-10-06）。

## 背景

历史页的批量导出只序列化当前加载的会话列表行，并提供上下文菜单的单条导出；它不包含完整会话 transcript。会话、任务和长期记忆的批量清理入口也不在各自标题旁，任务与事实目前没有清空命令。

任务列表同时展示 live work 与 terminal history。删除 waiting/running 任务会改变正在执行或已排队的工作；未完成 durable completion outbox 的 terminal 结果也必须保留，直到结果写入所属会话。

## 决定

1. 删除历史页的批量导出、选择模式和会话行的导出菜单项；保留单条删除与会话继续操作。把“清空会话”放到“会话历史”分区标题右侧。
2. 在“任务历史”和“长期记忆”分区标题右侧分别提供“清空历史”和“清空记忆”。所有批量清空通过确认对话框执行，并复用同一 `MaterialButton` danger 样式。
3. `clear_tool_run_history` 只清理 `completed`、`failed`、`cancelled` 的持久 ToolRun 行。数据库在一个 writer transaction 中清理；waiting/running 行与有未投递 completion outbox 的 terminal 行保留。服务层移除数据库确实删掉的 board 条目并返回删除数量。
4. `clear_facts` 删除全部 user/inferred 长期事实，依赖 facts 删除触发器清理关联 embeddings，并失效全量事实和 embedding 缓存。它不删除 session、episode 或 transcript。
5. 保留既有 `export_history` Tauri command registry；该命令没有 renderer 调用者，不由此 UI 删除改变其既有 IPC 行为。

## 替代方案

- 从 UI 当前已加载的 100 条任务中循环调用单条删除：拒绝。它会漏掉更旧历史，并产生部分清理状态。
- 清空全部 ToolRun 行，包括 waiting/running：拒绝。批量“历史清理”不应取消用户仍在等待或执行的任务。
- 清空长期记忆时连同会话/对话片段一起删除：拒绝。历史页中的长期事实与会话历史由不同持久化域管理。
- 保留多选导出但改成仅导出当前列表：拒绝。导出结果仍不是完整会话内容，且当前筛选/分页状态容易让用户误解导出范围。

## 影响与验证

- Tauri command directory 增加 `clear_tool_run_history` 和 `clear_facts`，生成 TypeScript response 为计数；不更改数据库 schema、ID 格式或现有命令 payload。
- UI 会话清空继续清除全部会话；任务清空只清除当时可删除的 terminal history；记忆清空只清除 facts。确认文案明确说明任务和记忆的范围。
- 验证：Rust workspace tests、UI tests/check/build、IPC contract、IPC events、ADR index、Rust formatting 与 diff 检查均通过。

## 回滚

回滚新增命令、service/repository 操作、生成的 IPC contract、UI 按钮和本 ADR；删除的 terminal history 与 facts 不恢复，因而回滚不执行恢复操作。数据库 schema 未变，不需要重置数据库。
