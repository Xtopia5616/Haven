# ADR 0832：完善内置工具指引并删除重复的 Agent 等待入口

## 状态

Accepted — 2026-10-09

## 背景

部分内置工具的模型可见参数没有说明默认值、分页游标、筛选语义或副作用范围，导致 Agent 需要从错误结果中反推调用方式。`memory.search` 与 `memory.recall`、定时任务的 `tool` 与 `continue` 模式、HTTP 的 GET/POST 也需要更清楚的选择指引。

Agent 协作目录同时暴露 `agent.join` 和 `agent.wait`。二者参数、执行路径、超时和结果完全相同，都调用 `AgentControlOperation::Wait`；底层契约和既有生命周期决策使用 `agent.wait`。

## 决定

- 为 files、HTTP、media、memory、system、schedule 和 agent 的模型可见 schema 补充准确的字段含义、默认值、续读方式与安全约束；对应 operation prompt 明确工具之间的选择条件。
- `files.asset_id` schema 约束为规范的 `asset-{uuid32}` 格式。
- 删除 `agent.join` 别名，保留 `agent.wait` 作为唯一的后代会话等待操作，并从 schema、注册目录、策略矩阵与架构清单移除该别名。
- `agent.profile` 的操作说明明确：省略 profile 字段时读取，提供字段时更新。

## 替代方案

- 保留 `agent.join` 和 `agent.wait` 两个同义入口：拒绝。模型目录会重复提供同一个动作并增加选择歧义，没有独立行为需要保留。
- 仅在 root schema 添加参数说明：拒绝。Operation view 会从 operation 分支提取 schema，必须让分支自身保留字段说明。

## 影响与验证

不改变工具执行逻辑、数据库、配置或实体 ID。`agent.join` 不再被接受；调用方应改用 `agent.wait`。资产引用只接受规范 asset ID。其余变化为模型可见说明和默认值元数据，不改变执行默认值。

验证通过：`cargo fmt --all -- --check`、`git diff --check`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo test --locked -p haven-tools`（795 passed、2 ignored；MCP integration 7 passed）及 `cargo test --workspace --locked`（haven-agent 614 passed、1 ignored；workspace 其余 crate 和 doc-tests 通过）。

## 回滚

恢复 `agent.join` 的枚举、schema 分支、operation view、提示词和策略/架构清单条目；还原本 ADR 记录。无需数据库或配置重置。
