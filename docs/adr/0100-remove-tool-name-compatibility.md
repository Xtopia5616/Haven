# ADR 0100：删除旧工具名称兼容层

- 状态：Accepted
- 日期：2026-09-08

## 背景

当前正式工具名已经收敛为 `files` 与 `schedule`。代码仍保留 `file`、`file_search`、
`scheduled_action` 的配置迁移、历史 UI 映射和 renderer 兼容入口，导致工具注册、权限、
历史恢复和 UI 选择存在多个名称真源。项目当前明确允许破坏性变更，不需要为测试阶段的
旧数据维持兼容。

## 决定

1. 删除公共工具名称映射、配置权限迁移和 UI renderer 的旧名称别名。
2. `files` 是唯一文件工具名；搜索结果仅依据 `files` 的结果 shape 选择搜索 renderer。
3. `schedule` 是唯一模型可见定时任务工具名；`scheduled_action` 仅可作为 Rust 内部实现
   和领域术语，不得作为工具调用或权限 key 根。
4. 检测到旧工具设置或旧权限 key 时备份配置并以默认配置启动，不静默改写权限语义。
5. 旧历史步骤不再被重命名；需要旧历史数据的用户按发布说明重置数据。
6. 动态 MCP/Skill 工具在 UI 侧只接受 provider-safe 的 `mcp__` / `skill__` 前缀，
   不再识别旧的单下划线或原始 `::` 形式。

## 替代方案

- 保留一次性迁移：拒绝，仍会让旧权限语义继续进入运行时，并延长双名称维护窗口。
- 在 UI 中继续把旧历史名称映射到新 renderer：拒绝，历史数据兼容不应反向约束当前工具契约。
- 直接丢弃旧配置且不备份：拒绝，配置中可能包含用户需要恢复的非工具设置。

## 影响与验证

这是内部工具名、权限配置和历史展示的破坏性变更，不修改数据库 schema。旧配置会生成备份，
旧历史步骤不再自动翻译。必须验证配置重置、工具注册表、权限矩阵、UI 解析和全量测试。

## 回滚 / 重置

回滚代码即可恢复旧实现；回滚到旧版本前应使用升级前的配置备份。升级当前版本后，旧配置按
`docs/release-and-reset.md` 的完整重置流程处理。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-common`
- `cargo test --locked -p haven-tools`
- `cd ui; corepack pnpm run check; corepack pnpm run test:run; corepack pnpm run build`
