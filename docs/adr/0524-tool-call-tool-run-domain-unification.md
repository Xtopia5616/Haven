# ADR 0524：统一 ToolCall 与 ToolRun 领域命名

- 状态：已采纳
- 日期：2026-10-06
- 范围：Agent 工具调用、持久后台/定时执行、Memory schema、App IPC 与 UI
- 关联：ADR 0393、0445、0477、0523

## 背景

系统把模型提出的一次同步工具调用称为 Action，把脱离当前 turn 的持久后台/定时工作单元也称为 Action/task。二者有共同的工具入口，却有不同的生命周期 owner：前者属于 Agent ReAct transcript 与 SessionToolRunner；后者由 Tools 的生命周期服务管理，持久化在 Memory，并通过 App event 与 UI 展示。旧命名把“调用”和“可持续运行体”混为一谈，也让表、ID、配置、命令、事件和 UI 类型分别暴露 action/task/tool 等名称。

## 决定

1. **ToolCall 表示一次调用。** Agent/ReAct 使用 `ToolCall` 与 `agent:tool_call` 表示模型在一次响应中请求执行工具。前台调用等待结果，并按既有 transcript/step 契约提交。provider 的 `tool_call_id` 保持 provider 格式。
2. **ToolRun 表示持久运行体。** 可脱离当前 turn 继续、取消并产生生命周期事件的工作统一称为 `ToolRun`，使用 `toolrun-{uuid32}` ID、`tool_runs` 表、`ToolRunStore` 与 `ToolRunService`。后台与定时是 ToolRun 的 kind：`background` 与 `scheduled`。
3. **前台/后台是执行方式，定时是触发方式。** Shell 输入使用 `execution_mode: foreground | background`；前台执行产生普通 ToolCall 结果，后台执行创建 ToolRun。定时触发仍由 `schedule` 工具创建并管理，不把定时触发伪装为新的执行方式。
4. **生命周期 owner 保持单一。** `ToolRegistration::ToolRun` 把 detached 运行绑定到所属 session；ToolRunService 继续拥有子进程、取消、输出尾部、恢复、终态与 completion outbox。此命名重构不新增通用 Job executor，也不把前台调用写入 ToolRun 状态表。
5. **管理入口采用正式工具名。** Agent 通过 `tool_runs.*` 管理后台/定时运行；Tauri commands、事件 `tool_run:*`、前端 contracts/store/card 与运行结果字段使用 ToolRun 名称。
6. 数据库升至 schema v37；旧 `actions` 表及关联字段不运行时迁移。旧的 `act-` ID、`actions.*` 工具名、权限 key、Shell `background` 参数和 action/task 配置 key 均不提供兼容映射。升级按 [发布与重置说明](../release-and-reset.md) 删除旧数据库文件与 `config.toml` 后重新配置。

## 替代方案

- 只改 UI 显示文案，保留数据库、Rust API、IPC 和配置中的 action/task 名称：拒绝。跨层维护仍需同时理解同一个工作单元的多套命名。
- 把前台同步调用也持久化成 ToolRun：拒绝。普通工具结果会错误地进入可取消、可恢复的后台生命周期，破坏 ReAct transcript 与 SessionToolRunner 的 owner 边界。
- 把后台与定时分成不同 entity：拒绝。二者共享 ToolRun 生命周期、状态、存储与管理界面；触发方式由 kind 表达即可。
- 引入通用 Job/Executor 模型：拒绝。本次只统一领域命名和 ToolRun 分类，不改变执行策略与故障语义。

## 影响

- 数据库、Shell 参数、工具名、权限 key、事件名、命令名和 UI contracts 均有破坏性重命名；没有隐式历史兼容或迁移。
- ToolRun ID 前缀为 `toolrun-`；SessionStep 保留其稳定 step ID，但字段明确表示 ToolCall。
- UI 会将来源明确的后台 shell 状态投影回原 ToolCall 卡；ToolRun Center 管理后台与定时运行，二者继续使用同一 ToolRun 数据契约。
- 历史 ADR 保留当时使用的 Action 名称，作为历史决策记录；当前结构以本 ADR 与路线图为准。

## 验证

以下门禁均通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- UI：`corepack pnpm run check`、`corepack pnpm run test:run`（986 tests）与 `corepack pnpm run build`
- IPC：`scripts/check-ipc-contracts.ps1`（79 handlers）与 `scripts/check-ipc-events.ps1`（40 channels）

## 回滚

回滚代码、事件、配置和生成契约时需同步恢复旧 ToolRun 名称与 Shell 参数契约。v37 数据库不被旧二进制接受；只有恢复升级前完整数据根目录备份才能恢复旧版本数据，否则需按旧版本的 reset policy 重新建库。不得把 v37 数据库直接交给旧版本。
