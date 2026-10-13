# ADR 1118：报告文件搜索完整性

## 状态

已接受（2026-10-13）

## 背景

`files.search` 会继续收集可访问路径的结果，并忽略遍历、metadata、open、seek、read 与 ripgrep 搜索错误。输出只用 `has_more` 与顶层 `truncated` 表示容量截断；因此只要没有触发数量上限，部分结果甚至空列表都可能被解释为完整搜索。renderer 也会把所有空列表显示为“没有匹配的结果”。匹配为空并不能证明搜索覆盖完整。

## 决定

- 私有 `FileSearchResult` 增加 `io_error_count`；文件名与全文搜索分别汇总并行遍历、metadata 和文件读取/匹配中的非取消错误。
- 工具 JSON 输出增加 `search_complete` 与 `io_error_count`。`search_complete` 仅在没有截断且错误数为零时为 true；错误仅计数，不输出路径或原始系统错误文本。
- `has_more` 与顶层 `ToolResult.truncated` 仍只表达达到搜索/输出上限。搜索有读取错误时保留可用结果并返回 `success`，通过 `search_complete=false`、失败计数和提示表达部分结果。
- 搜索 renderer 对不完整结果显示部分结果提示；只有 `search_complete` 为 true 时，空列表才显示“没有匹配的结果”。缺少新字段的历史 tool output 继续按既有行为渲染。
- 取消仍由外层作为取消错误返回，不计入搜索 I/O 错误。

## 替代方案

- 将任何读取失败改为整次工具失败：会丢弃已有的有效匹配，且用户无法继续检查可访问文件的结果。
- 复用 `truncated` 表示文件错误：会混淆“可能还有结果”与“某些路径不可访问”，并把部分搜索误报为达到容量上限。
- 仅修复 renderer 空列表文案：tool output 仍不携带机器可读的覆盖状态，模型/调用方无法区分空结果与失败的遍历。

## 影响与验证

- 仅改变 Tools JSON 结果与 UI renderer 行为；不改变持久化数据、配置或 IPC。无需重置。
- Rust 测试覆盖正常完整结果、容量截断导致不完整，以及 line-range 文件读取失败计数。
- UI contract 测试校验新字段，并覆盖“空结果 + 读取失败”提示及旧结果缺少新字段的兼容渲染。
- 验证：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引检查。

## 回滚

回滚时同时移除 `FileSearchResult` 错误计数、工具 JSON 字段、renderer 警示和本 ADR/命名契约；无需迁移持久数据。
