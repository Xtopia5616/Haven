# ADR 0075：对话停止操作改为可继续的中断

## 背景

对话头部的“结束会话”和输入区的停止按钮曾经都调用 `end_session`，导致
“停止当前输出”和“关闭会话”无法区分。输入区的重复新建/结束按钮也让同一
个会话操作出现两套入口。

## 决定

1. 会话头部保留“新建会话”和“结束会话”；底部工具栏不再重复渲染这两个操作，
   仅在存在多个并行会话时显示切换入口。
2. 输入区的无输入状态按钮调用新的 `interrupt_session` IPC 命令。该命令把
   `running`/`pending` 会话置为 `paused`，运行中的 provider 调用同时取消，
   但不删除会话、历史或后台任务；后续输入仍可继续同一会话。
3. 页面级加载动画使用 viewport 居中定位，与启动 hydration 前的加载层保持同一
   几何位置。
4. 设置页保存操作栏只在脏状态显示；“放弃更改”使用 outlined 按钮保持可见。

## 替代方案

- 只修改按钮文案并继续调用 `end_session`：会让 UI 文案与实际生命周期不一致，
  违反停止和结束必须可恢复区分的约定。
- 保留底部重复按钮：会继续造成两个入口表达同一会话动作，且窄窗口空间更紧张。

## 影响

- IPC 命令目录从 67 项增至 68 项；新增命令仅接受 session id，并沿用已有会话
  状态转换、取消 token 和事件通知路径。
- 中断会保留当前已完成的持久化内容；被取消的未完成 provider 输出不作为最终答案
  写入历史，用户可以继续输入或从暂停会话恢复。

## 验证

- `cargo test --locked -p haven-agent -- session`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`

## 回滚 / 重置

回滚代码即可恢复旧 UI；不涉及数据库 schema、配置数据或历史数据重置。
