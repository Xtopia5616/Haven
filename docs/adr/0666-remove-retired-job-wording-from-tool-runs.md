# ADR 0666：统一 ToolRun 源码中的退役 Job 词汇

## 背景

ToolRun 配置字段已经采用 `background_tool_run_*` / `tool_run_terminal_ttl_secs`，但字段注释仍列出已退役的 `JOB_TAIL_MAX_CHARS`、`JOB_OUTPUT_EMIT_INTERVAL` 和 `TERMINAL_JOB_TTL`。ToolRunService 注释也引用旧 TTL 名，Windows board 测试仍称其行为为“lists all jobs”。这些都把同一工具运行实体写成旧泛名。

## 决定

- 删除当前配置注释中的退役常量名脚注。
- ToolRun store 注释引用当前配置字段 `tool_run_terminal_ttl_secs`。
- 将 board 测试名改为 `test_board_lists_all_tool_runs_by_session`。
- 将 `ToolRun` 定为 app 能力持久运行实体的唯一名称；Windows `Job Object`、外部供应商异步 job 与 Memory extraction job 是不同概念，不改名。
- 无运行时、wire、配置 key 或持久化行为变化，无需重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-common -p haven-tools --all-targets`
- `cargo clippy --locked -p haven-common -p haven-tools -- -D warnings`
- `scripts/check-adr-index.ps1`

## 回滚与重置

回滚只恢复注释和测试标识符，不涉及持久数据或用户配置。
