# ADR 0701：统一 ToolResult 截断状态同步 owner

## 状态

已采纳并实施。

## 背景

工具执行结果存在两层相关状态：`ToolResult.truncated` 记录结果级截断元数据；具体工具 JSON 可包含模型可见的 `truncated` 正文字段。成功路径已有 `ToolResult::from_output` 作为同步入口，但一些调用点先手写正文字段又调用该入口，另一些直接构造成功结果；Skill 的失败输出带正文标记，却没有同步结果级字段。

这两层不应合并成一个字段：正文 shape 由各工具契约决定，有的工具没有该字段；结果级标记仍可表示正文之外的截断状态。

## 决定

- 正文 `truncated: true` 必须使 `ToolResult.truncated` 为 true。
- 正文契约需要暴露截断标记的成功结果统一使用 `ToolResult::from_output`；该方法负责从正文推导结果标记，并在显式截断时补上模型可见标记。
- `ToolResult::failed_with_metadata` 从已有正文标记推导结果级标记，不改写失败正文。
- 删除调用点在 `from_output` 之前重复写入的 `truncated: true`，并将 files 与 messaging 列表投影收敛到同步入口。
- 不给没有正文标记的工具扩展 JSON 字段；`ToolResult::truncated` 仍可只设置结果级标记。

## 替代方案

- 把 `ToolResult.truncated` 与正文 `truncated` 合并：拒绝，前者是结果元数据，后者属于操作输出 shape，且正文字段并非所有工具都有。
- 在所有 JSON 结果中强制加入 `truncated`：拒绝，这会把工具专属字段变成新的通用正文契约。
- 继续让每个调用点分别同步：拒绝，重复写入和漏写已经导致行为分叉。

## 影响与验证

工具正文 JSON shape 保持不变。`messaging.list` 与 Skill 超限失败现在会让 `ToolResult.truncated` 与已有正文标记一致；其余变更把既有同步逻辑集中到唯一入口。无 IPC、数据库或持久化格式变化，无需重置。验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`（801 passed、2 ignored，另有 7 项 MCP 集成测试通过）、`cargo clippy --locked -p haven-tools -- -D warnings`、ADR 索引与 `git diff --check`。

## 回滚

恢复原有调用点和失败构造行为，并撤回本 ADR、`docs/naming.md` 规则及路线图条目；无数据迁移。
