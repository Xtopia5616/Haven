# ADR 0523：后台 shell 任务沿用来源工具调用呈现

- 状态：已采纳
- 日期：2026-10-06
- 范围：后台 shell 工具调用的会话 UI 与生命周期关联
- 关联：ADR 0172、0215、0443

## 背景

`shell(background=true)` 本身是一次普通工具调用，但执行进程会脱离当前 `SessionToolRunner` 调用并由 `ActionService` 管理。会话时间线因此同时出现工具调用卡和 Action 卡；两张卡虽通过 `action_id` / `source_step_id` 关联，却重复表达同一工作。实时 `action:output` 预览又不携带来源步骤，使所有 Action 生命周期事件不能统一按一个稳定工具步骤理解。

## 决定

1. 后台 shell 仍经 `SessionToolRunner` 的工具执行、安全授权和标准 observation 路径启动；返回的 `ToolRegistration::Action` 将 detached Action 绑定到所属 session。`ActionService` 继续独占子进程、取消、输出尾部、持久化和完成 outbox 生命周期，不新增第二条工具执行通道或通用 Job executor。
2. `source_step_id` 是后台执行与发起工具调用之间的稳定关联身份。`action:created`、`action:updated`、`action:finished` 和 `action:output` 均携带该字段；输出事件仍只暴露有界 preview，不带命令参数、日志路径或其他内部字段。
3. 若会话中存在来源工具调用，后台 Action 状态、实时输出和等待提示投影到该工具调用的 `ToolResultCard`，不再另插一张后台 Action 卡。来源消息缺失时，时间线使用同一 `ToolResultCard` 展示 Action 作为恢复兜底。
4. `action_id` 仍用于取消、历史和 completion outbox；`source_step_id` 仅负责关联工具调用。任务中心仍按 Action 读写；后台完成结果仍经 Agent completion delivery 成为模型可用的 transcript 内容。
5. 定时 Action 继续使用独立任务卡。同步工具结果仍沿用原有工具 renderer；后台任务与同步工具调用统一在来源调用卡内展示状态、参数和输出。

## 替代方案

- 保留两个并列会话卡：重复展示同一后台 shell 操作，拒绝。
- 把所有同步工具状态改为 Action 并由 ActionService 执行：会让可取消、可持久化的 detached 生命周期渗入普通工具调用及其 transcript 语义，增加第二个状态权威，拒绝。
- 删除 Action owner 并让 SessionActor 持有子进程：进程生命周期会被绑定到 ReAct 调用，破坏后台执行脱离当前生成和持久 completion 的边界，拒绝。

## 影响

- 持久 schema、命令和 Action ID 不变；`ActionEvent.source_step_id` 是对既有可选字段的使用扩展，没有数据重置要求。
- 工具调用卡成为会话内后台 shell 状态的唯一可见投影；任务中心和 ActionService 仍是任务状态的读写 owner。
- 来源工具消息未恢复时，统一工具卡仍可以从 `ActionPayload` 呈现后台任务；终态输出已进入 transcript 时不重复显示。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`corepack pnpm --dir ui run check`、`corepack pnpm --dir ui run test:run`（122 files / 986 passed）、`corepack pnpm --dir ui run build`、`pwsh -NoProfile -File scripts/check-ipc-events.ps1`、`pwsh -NoProfile -File scripts/check-adr-index.ps1`、`git diff --check`。

## 回滚

可以独立回滚 UI 投影并恢复 ADR 0443 的 Action 时间线卡；若回滚事件映射，应同时停止在 `action:output` 中发送 `source_step_id`。无 schema 或配置重置。
