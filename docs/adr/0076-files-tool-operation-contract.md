# ADR 0076: `files` 工具按操作约束输入并统一结构化结果

## 背景

`files` 是一个聚合工具，既处理文件读写，也处理目录列表、摘要和搜索。原有 schema 把所有参数放在同一个对象中，仅要求 `operation` 和 `path`，模型容易混用 `path`/`root` 或遗漏操作必需字段。与此同时，完整读取、目录列表和搜索在达到输出上限时，JSON 内的 `truncated` 与顶层 `ToolResult.truncated` 可能不一致。

## 决定

1. 保留工具名 `files` 和现有操作名。
2. 用 `oneOf` 为每个操作建立独立输入分支：
   - 文件操作使用 `path`；
   - `search` 使用 `root` + `pattern`；
   - `write`、`edit`、`copy/move` 分别要求各自的必需字段；
   - 每个分支拒绝不属于该操作的额外字段。
3. 在 `files` 工具边界为结构化结果补齐 `operation`、路径上下文和 `truncated` 字段。
4. 输出实际被截断时，JSON 字段和顶层 `ToolResult.truncated` 必须同时为 `true`；未截断时两者同时为 `false`。

## 影响

- 模型可以从 schema 直接得到操作级参数约束，减少无效调用。
- `search` 不再要求无关的 `path` 字段；包含旧式混用字段的调用会被明确拒绝，而不是静默忽略。
- 前端现有结果字段保持不变，新增字段为兼容的上下文元数据。
- 搜索结果上限和文件/目录读取上限的截断状态可被 Agent、历史投影和 UI 一致解释。

## 替代方案

- 拆成多个顶层工具（如 `file_read`、`file_write`、`file_search`）：调用更窄，但会扩大工具列表、权限键和前端契约；留待后续按实际调用数据评估。
- 只修改描述文本：不能阻止错误字段组合，也不能修复顶层截断状态漂移。

## 验证与回滚

- `cargo test --locked -p haven-tools files`
- 运行完整 `haven-tools` 测试和严格 Clippy。
- 若需要回滚，仅回退本 ADR、`files.rs` 和 `file_search.rs`；无需删除数据库或配置。
