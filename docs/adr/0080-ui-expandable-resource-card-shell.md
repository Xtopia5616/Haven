# ADR 0080：UI 可展开资源卡片壳层

## 背景

内置工具、技能和 MCP 服务器卡片都需要相同的展开/收起、右键菜单、键盘激活和卡片壳层样式。三处组件分别维护了同一套交互与约 30 行结构 CSS，容易造成无障碍行为和视觉 token 漂移。

## 决定

- 新增 `ExpandableContextCard`，唯一负责资源卡片的展开状态、Enter/Space 键盘激活、右键菜单生命周期和公共卡片壳层。
- `BuiltinToolCard`、`SkillCard`、`McpServerCard` 通过 `header`、`actions`、`children` snippets 注入各自的业务展示与操作。
- 资源卡片继续由各自组件拥有业务状态、context-menu 项目和回调；公共组件不读取 store、不调用 Tauri，也不解释资源数据。
- 取消三张卡片内重复的壳层状态与结构 CSS，不保留旧实现或兼容层。

## 替代方案

- 继续在三张卡片中复制结构：实现简单，但后续修复键盘、右键菜单或卡片几何时会产生漂移。
- 抽象成通用资源数据模型：会把工具、技能和 MCP 的不同业务语义强行合并，扩大组件边界，本次不采用。

## 影响与回滚

本次只改变前端内部组件组合，不改变 Tauri 命令、事件、数据格式或用户数据，不需要重置。回滚本提交即可恢复三张卡片的本地壳层实现。

## 验证

- `cd ui; corepack pnpm run check`
- `cd ui; corepack pnpm run test:run`
- `cd ui; corepack pnpm run build`
