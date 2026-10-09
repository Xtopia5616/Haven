# ADR 0833：通过领域命令适配器调用设置与 Skill 执行命令

## 状态

Accepted — 2026-10-09

## 背景

`SettingsView` 已通过 `settingsCommand.ts` 加载设置，但撤销授权、暂存凭据、更新设置、记忆维护和 autostart 等操作仍直接 `invoke`。单数模块名也与 `sessionCommands.ts`、`toolsCommands.ts` 等领域命令适配器不一致。

`SkillCard` 为执行预览自行动态导入 `tauri.ts` 并调用 `execute_skill`；因此通用 UI 组件同时承担展示、参数解析和领域命令调用。ToolsView 已是技能管理的页面 owner，`toolsCommands.ts` 已管理 Skill/MCP/Tool 命令。

## 决定

- 将 `settingsCommand.ts` 与其测试重命名为 `settingsCommands.ts` / `settingsCommands.test.ts`；删除旧路径，不提供兼容导出。
- 将 SettingsView 使用的设置命令集中封装在 `settingsCommands.ts`，包括读取、授权管理、凭据暂存、配置更新、记忆维护、hotkey capture 与 autostart。SettingsView 仍拥有设置草稿、保存顺序、页面状态和错误呈现。
- `toolsCommands.ts` 增加具名 `executeSkill` wrapper，并复用 Rust 生成的请求/响应类型。ToolsView 把该操作作为 `onPreview` callback 交给 SkillCard。
- `lib/views/` feature view 和 UI 组件不直接 `invoke`；领域操作经 `*Commands.ts` 或 owner callback。Route/App shell 的跨域启动和应用生命周期命令维持其单独职责。
- IPC 字段和值、AuthorizationEngine 路径、确认队列、错误处理、设置保存顺序和可见交互保持不变。IPC 契约检查脚本登记这两个领域的唯一直接 invoke owner。

## 替代方案

- 保留 SettingsView 和 SkillCard 直接调用：拒绝。两个领域命令已各有稳定的 feature command owner；让 view/widget 各自绑定 raw invoke 会使调用点和授权语义难以全仓定位。
- 给 `settingsCommand.ts` 增加复数 alias：拒绝。测试版本不保留纯命名兼容层，所有调用点直接更新为规范模块名。
- 将每个设置命令拆成独立文件：拒绝。它们属于同一 Settings feature 边界，多个小文件不会带来新的 owner 或依赖方向。

## 影响与验证

只改变前端调用所有权与内部模块路径，不改变 Rust 命令、生成 DTO、数据库、配置、持久化或安全授权契约，无需数据重置。Feature view 继续负责用户可见流程；adapter 只转发生成请求并返回响应，不增加 UI 状态或通知。

验证：`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、`scripts/check-ipc-contracts.ps1`、Prettier 与 `git diff --check`。

## 回滚

将使用方恢复至原命令调用位置、恢复 `settingsCommand.ts` 路径并撤销本 ADR 与 UI/naming/roadmap 规则更新。无需数据库或配置重置。
