# ADR 0813：流式展示未完成的工具调用参数

## 背景

Provider 会分片返回工具调用名称和参数。参数分片在生成过程中通常不是完整 JSON，当前界面只能等到 ReAct 解析出完整调用后才显示工具卡片，用户无法看到模型正在准备什么调用。

## 决定

- OpenAI Chat Completions、OpenAI Responses、Anthropic 与 Gemini 适配器通过 `StreamToolCallUpdate` 转发参数增量或快照。
- Agent 将它们发布为临时 `agent:tool_call_chunk` 事件。事件只供当前运行的 UI 预览，不写入 `session_events`、transcript、数据库或崩溃恢复数据，也不参与工具解析、授权或执行。
- 预览按工具索引维护身份；每步最多保留 32 个预览，每个参数最多保留 8 KiB，并以至少 50 ms 间隔合并 UI 更新。达到上限时省略新增预览或截断参数，最终完整 `ToolCall` 仍按现有路径处理。
- 前端完整 JSON 使用现有参数视图；暂时无法解析的片段以转义文本展示，并标为“生成中”。完整 `agent:tool_call` 到达后按 `(step_number, run_id, tool_index)` 替换预览。流重置、回合结束、错误和会话恢复会清理临时预览。
- 不记录参数文本到日志。预览事件只在应用的本地 Tauri IPC 中传输。

## 替代方案

- 只展示完整调用：实现简单，但仍无法满足流式反馈需求。
- 尝试修补或执行不完整 JSON：片段可能缺字段、截断或改变结构，会把展示状态误当成权威输入，因此不采用。
- 将预览写入 transcript：会增加事件、投影、恢复和回滚契约；预览可由实时流提供，持久化没有必要。

## 影响

这是 `haven-llm`、`haven-agent`、Tauri 事件边界和 UI reducer 的临时事件契约变更。数据库 schema、配置与持久化事件不变，无需数据库重置或数据迁移。

## 验证

- 通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、严格 `cargo clippy --workspace --locked -- -D warnings`、UI `check` / `build` / Prettier、IPC generator `--check`、IPC contract 与 event directory 检查。
- 未运行测试。

## 回滚与重置

回滚时删除临时事件、provider 增量转发和 UI 预览处理，并从生成的 IPC 目录移除事件名。没有持久化数据需要清理或重置。
