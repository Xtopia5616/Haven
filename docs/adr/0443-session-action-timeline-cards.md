# ADR 0443：会话时间线中的 Action 任务卡

- 状态：已采纳
- 日期：2026-10-03
- 范围：对话时间线中的后台任务/定时任务呈现与后台等待提示
- 关联：ADR 0037、0335、0344、0373、0393

## 背景

`ActionEvent` 已携带 `session_id` 与两类 Action 共用的状态/时间字段；工具 observation 也保留创建结果中的 Action ID。对话页目前只在消息末尾显示“等待后台任务结果，完成后将自动继续”，任务列表则展示 Action 详情。用户需要在当前会话中就近看到任务状态与结果，同时保留同步工具卡和 Action 生命周期的独立边界。

## 决定

1. 对话页只接收 `session_id` 等于当前会话 ID 的 Action。优先使用 Action 的 `source_step_id` 与 transcript 工具消息 ID 定位来源步骤；没有匹配步骤 ID 时，再用 observation 中的 Action ID 作为恢复与旧记录回退。后台 observation 使用 `action_id`，`schedule.set` observation 使用创建结果 `id`。暂时无法定位来源步骤的 Action 仍出现在其所属会话时间线末尾。
2. Background 与 scheduled 共用 `ActionTimelineCard` 外观和 Action 卡片投影。卡片保留各自的状态、开始/触发时间和细节：后台任务展示命令及实时预览/输出，定时任务展示模式、正文与触发时间。
3. 后台等待会话只在一个正在运行的后台任务卡中显示“等待结果，完成后自动继续”及待等任务数；若 Action 行尚未同步，则显示同款等待状态卡。删除独立时间线等待横幅，避免重复提示。
4. 普通同步工具结果继续由 `ToolResultCard` 展示。Action 卡不取代工具结果、会话状态或 ActionService 生命周期。
5. 使用现有 `list_action_history` 命令新增可选 `session_id` 过滤；切换到会话时加载最多 200 条该会话终态 Action，并与实时 Action 事件合并。UI 缓存最多保留 16 个会话、每会话最多 200 条；数据库仍是历史权威。
6. `sourceActionId` 是前端 reducer 内部从 observation 重建的定位回退；`sourceStepId` 是 ActionEvent 稳定步骤锚点。后台终态若已投影回原工具卡，Action 卡保留状态和任务信息但隐藏重复输出；尚未投影 transcript 结果时才由 Action 卡呈现终态详情。

## 替代方案

- 只在 TaskCenter 展示 Action：用户仍需离开当前会话才能了解任务进度，拒绝。
- 保留独立等待横幅并再加任务卡：会重复表达后台等待，拒绝。
- 再新增一个 IPC 来源锚点字段：`ActionEvent.source_step_id` 已可定位后台来源步骤，定时任务的 observation 也含 Action ID，没有必要重复字段。

## 影响

前端会话时间线新增共用 Action 卡，并移除旧等待横幅。现有只读历史命令增加可选会话过滤字段；无 schema、配置或用户数据迁移。来源缺失时按明确的会话 owner 回退到时间线末尾，不按文本内容猜测或跨会话关联。

## 验证

- `cargo test --locked -p haven-memory -- action_history_can_be_filtered_by_session_for_timeline_hydration`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
- `corepack pnpm --dir ui run build`
- `git diff --cached --check`

回归覆盖后台与定时 observation 的稳定 Action ID、来源步骤锚定、按会话恢复终态记录、等待提示只渲染一次、无 Action 行时的等待状态，以及已写回 transcript 的后台结果不会重复显示。

## 回滚

删除时间线 Action 卡与锚定投影，恢复旧等待横幅并移除相应 UI 测试和本 ADR 索引即可；无需数据或配置重置。
