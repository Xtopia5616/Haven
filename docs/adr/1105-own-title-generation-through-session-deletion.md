# ADR 1105：将标题生成纳入会话删除生命周期

## 状态

已接受（2026-10-13）

## 背景

`AgentLayer` 为新会话启动的自动标题生成任务使用 detached `tokio::spawn`。会话删除不会取消或等待该任务；如果 provider 在删除期间返回，SQLite 对已删除 session 的 `UPDATE` 原先仍会以零影响行数报告成功，任务随后可能更新运行态并发布 `TitleUpdated`。

此外，`SessionSupervisor::running_tool_runs_list` 没有消费者，且实际返回的是运行中 actor 的 `session_id`，名称错误地表示 ToolRun 身份。

## 决定

- 删除无消费者且实体身份命名错误的 `running_tool_runs_list`。
- `AgentLayer` 持有每个自动标题任务的取消令牌与 `JoinHandle`。删除单个 session 前先取消并等待该 session 的标题任务，持久删除成功后再次 drain，以覆盖与已开始创建流程交错的任务；清空历史前后分别 drain。
- 标题生成在提交前通过 SessionSupervisor 生命周期 gate 串行化，并重新确认 session 仍存在且未被关闭；title 持久写入、运行态投影与 `TitleUpdated` 发布处于同一生命周期临界区。取消期间不继续投影或发布。
- 删除失败时，若确实取消了标题任务且 session 仍存在、未关闭并且没有标题，则恢复该 best-effort 标题任务。
- `Database::update_session_title` 必须确认恰好更新一行；目标 session 已不存在时返回 not-found 错误，不得将零行更新解释为成功。

## 影响

已删除 session 不会被迟到的标题任务更新运行态或发布 `TitleUpdated`。缺失 session 的标题更新会显式失败。没有数据库 schema、配置或 IPC shape 变更，无需重置持久数据。

## 验证

- Agent 回归使用阻塞 provider：等待生成开始后删除 session，确认删除会取消并 join provider future；之后释放 provider，并断言 session 已删除且没有对应 `TitleUpdated`。
- Memory 回归确认缺失 session 的 title update 报告 not found。
- 运行 `cargo test --locked -p haven-agent deleting_session_cancels_and_joins_blocked_title_generation` 与 `cargo test --locked -p haven-memory update_session_title_nonexistent_returns_not_found`。

## 回滚

不得恢复无 owner 的标题生成任务，或将缺失 session 的零行标题更新视为成功。若生命周期实现需要调整，必须继续保证 session 删除与标题持久写入/事件发布有序，并保留阻塞 provider 回归。
