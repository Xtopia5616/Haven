# ADR 1349：将 Memory 工具归入 Haven 根节点

## 状态

Accepted — 2026-10-21

## 背景

工具设置页按 `ToolManifest.identity.root` 将同一目录分类中的操作分组。Memory 操作已经属于 `haven` 分类，
但 root 仍为 `memory`，因此 UI 将其单独显示，与 Haven 管理操作分开。

## 决定

1. Memory 的五个 manifest root 使用 `haven`，作为 Haven 工具根下的 `memory.*` 操作显示。
2. 稳定模型工具名、权限 key、风险、Schema、renderer、运行实现及 `haven-memory` 持久化 owner 保持不变。
3. Memory 操作继续使用 `ToolConfig.memory` 与 `memory.*` permanent permission key；不新增配置迁移或数据重置。

## 替代方案

- 将模型可见名称及权限 key 重命名为 `haven.memory.*`：拒绝。当前目标是统一工具清单中的根分组；改名会改变工具调用和权限身份，还会令 `haven` 父级 grant 扩展到记忆操作。
- 只保留单独的 `memory` 根：拒绝。设置页仍会把 Memory 与 Haven 管理操作分开呈现。

## 影响、验证与回滚

变更只影响工具 manifest 的层级元数据和设置页分组；没有数据库、config、durable event 或 IPC shape 变化，用户数据无需迁移或重置。
如果构建工具目录失败，Memory 仍保留原独立运行路径。回滚时恢复 root 为 `memory` 即可，不影响模型调用名或权限。

验证：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`corepack pnpm run check`、
`corepack pnpm run test:run`。实际结果记录于提交说明。
