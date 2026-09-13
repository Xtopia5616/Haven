# ADR 0141：统一工具目录分类

## 状态

Accepted — 2026-09-13

## 背景

Prompt 索引原先按工具名的第一个点号片段分组，UI 又按 operation root 分组。
因此 `ask`、任务和记忆无法归入 Haven，文件、窗口、输入和进程也无法统一归入
系统；分类规则在不同边界重复维护。

## 决定

1. 在公共 `ToolDef` 中增加仅供目录使用的 `ToolCatalogGroup`，不改变工具名称、
   权限 key、provider schema 或执行边界。
2. 固定分类为 `haven`、`system`、`agent`；动态技能和 MCP 分别使用 `skills`、
   `mcp`，未声明的测试/扩展工具使用 `other`。
3. `ask`、记忆、后台/定时任务、偏好、清单和 Haven 管理能力归入 `haven`；
   `notify`、shell、HTTP、文件、媒体、窗口、输入、进程、剪贴板和系统能力归入
   `system`；跨会话协作 `agent.*` 保持独立。
4. Agent Prompt 和工具页都消费同一分类元数据；工具页按分类展示卡片，卡片内仍保留
   每个工具的独立启停、风险和 Schema。

## 影响与验证

分类字段仅在内部目录和 builtin 工具列表的 UI 响应中传播，不进入 provider-facing
`tools[]` Schema；现有工具名和授权规则保持兼容。新增测试覆盖分类元数据、Prompt
目录、UI 分类卡片和旧工具 JSON 契约。

验证命令：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cd ui && corepack pnpm run check
cd ui && corepack pnpm run test:run
```
