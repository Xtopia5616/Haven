# ADR 0595：明确 ChatSessionController 的会话动作职责

## 状态

已采纳并实施。

## 背景

聊天页存在三个不同的控制器 owner：`ChatController` 编排会话命令和 reducer 转换；`ChatViewController` 管理 DOM 滚动、自动跟随与布局观察；`ChatEventController` 负责 Tauri listener 的注册与释放。后两者的名字已经体现 view 与 event 生命周期，`ChatController` 则没有说明自己控制的对象和职责范围。

## 决定

1. 将文件、类、factory、依赖类型及路由实例统一命名为 `chatSessionController` / `ChatSessionController`。
2. 保留 `ChatViewController` 与 `ChatEventController`，因为它们管理不同的生命周期 owner，不合并成一个 chat 页面总控制器。
3. 保留 attachment 类型及其字段名称；它们是页面提交输入的类型，不因模块改名而改变角色。

## 替代方案

- 保留 `ChatController`：拒绝，页面中另有多个 chat controller，泛名无法从符号上区分会话动作与 view/event 生命周期。
- 合并三个 controller：拒绝，它们分别管理 reducer/命令、DOM observer、Tauri listener 生命周期，状态与释放边界不同。

## 影响与验证

- 仅重命名 UI 内部 TypeScript 模块与符号，并更新当前架构说明；不改变 Tauri command、事件、DOM 观察、reducer transition 或 UI 行为。
- 命名路线图继续保持 Active；组件、stores、其余 controllers、props、event handlers、contracts 和跨层命名仍待全项目审计。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、ADR 索引及 staged diff 检查。

## 回滚

将模块和符号恢复为 `ChatController` 命名并还原架构文档中的旧路径；无持久化或 IPC 迁移。
