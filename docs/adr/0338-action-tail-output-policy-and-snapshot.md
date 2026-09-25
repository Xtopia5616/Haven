# ADR 0338：Action tail output policy and snapshot ownership

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools` foreground shell live preview、background action live preview 与 `action:output` App projection
- 关联：[ADR 0215](0215-action-board-typed-view.md)、[ADR 0275](0275-action-service-typed-agent-projections.md)、[ADR 0305](0305-action-service-action-store-port.md)、[ADR 0325](0325-action-completion-transport-ownership.md)、[ADR 0334](0334-action-terminal-persistence-retry-policy.md)

## 现状与不变量

foreground shell 与 background shell 已共用 `read_stream_capped` 的 stdout/stderr drain、增量 UTF-8 解码、滑动 tail append 和按内容比较逻辑，但 `LiveOutputHub` 与 `ActionService` 各自缓存 tail 字符上限，并让多个消费者直接共享 `Arc<Mutex<String>>`。这使相同的 tail policy 有两份运行时状态，也让消费者和输出缓冲区的关系只能靠裸字符串约定。

本切片保持以下语义：

1. stdout/stderr 仍并发读取到 EOF；terminal byte cap、溢出标志、lossy decode、ANSI/PowerShell 格式清理、stdout/stderr 合并顺序及既有 terminal output/log/store 内容不变。
2. foreground tool card 的 `agent:tool_output` 仍按 `session_id + step_id` 发布；background board 的 `action:output` 仍按 `action_id` 发布。两种 cadence、增量快照时机和终态 `action:finished` 事件保持。
3. Background terminal 状态、`action_result_id`、completion outbox 事务、durable transcript 后 ack、scheduled fire/claim、取消与 shutdown 顺序不变。Scheduled action 不持有输出 tail。
4. 应用配置中的 `background_job_tail_max_chars` 在 command 启动时采样；运行中的 tail 保留启动时上限。Tail 限制按 Unicode 字符计数并保留末尾字符；超限读流仍继续 drain。
5. Tail 只服务于受限 live preview。`ActionTailSnapshot` 不实现 `Debug` 或 `Serialize`，不被持久化或直接记录；terminal output 继续只通过既有状态、日志文件和 completion 路径处理。

## 决定

新增 crate-private `action_output` 模块：

1. `ActionService` 持有唯一 `ActionOutputPort` policy；`ToolRuntime` 将其只读 handle 交给 `LiveOutputHub`。配置更新仍由 ActionService 接收，foreground hub 只保留自己的展示 cadence。
2. `ActionOutputPort` 在启动时创建带有 policy 快照的 `ActionOutputTail`。读流只通过 tail append 解码文本；ActionService status/board、background emitter 和 foreground emitter 只取得 `ActionTailSnapshot`。
3. Snapshot 的 value comparison 继续捕获同长度窗口滑动。Tail append 按 Unicode 字符上限截断，避免多字节字符把有效 preview 不必要地压短。
4. App 对 `action:output` 使用窄 projection，只输出 `id/kind/status/output`；即使内部 payload 多带 command、tool args、日志路径或 stderr，也不随 preview event 传播。`ActionEvent` Tauri wire 字段集合不变；foreground `agent:tool_output` 的三字段 payload 不变。
5. Terminal commit、rollback 和取消/shutdown 的既有清理点继续将 `ActionEntry.tail` 设为 `None`；foreground tail 随命令 future 结束释放。Terminal snapshot 仍来自 final collected output，而非 live preview buffer。

不把 foreground tool card 和 background action event 合并成一个 IPC DTO：它们使用不同的实体身份、消费者和生命周期。也不把完整 stdout/stderr、ActionStore、LLM 或 completion ack 迁入输出 port。

## 验证

- `action_output` 单测覆盖 Unicode 截断边界、零上限、增量顺序、重复快照抑制和同长度窗口滑动。
- Tools 测试覆盖 stdout/stderr 读流 tee、切分 UTF-8 字符、共享 policy、terminal final snapshot、取消后 tail 清理；既有 completion receiver 测试覆盖 lag 后继续接收与 closed 返回 `None`。
- App bridge 测试确认 `action:output` 只投影 bounded preview 字段，忽略 command、dynamic tool args、log path 和额外 stderr 字段。
- 既有 ActionService/outbox/Agent tests 继续覆盖 durable background ack、scheduled fire、取消、shutdown 与 completion 身份。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`git diff --cached --check`。

## 未完成工作与回滚

此 ADR 只统一 tail policy 与 live snapshot 内部边界。trigger/execution 分离、跨 kind 的完整 Job lifecycle、action-level 执行 timeout、独立 claimant owner token、lease renewal 和完整 background/scheduled UI projection 仍未完成。Agent peer status inspection 与 completion delivery 不持有 live tail，继续由各自 owner 管理。

没有 schema、IPC、配置字段或用户数据迁移。回滚时恢复 `ActionService` / `LiveOutputHub` 各自的 tail 字段和原 shared-string helper，并回退本 ADR、架构/路线图记录与对应测试。
