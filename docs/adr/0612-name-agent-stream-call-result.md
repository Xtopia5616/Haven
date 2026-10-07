# ADR 0612：命名 Agent streamed LLM call 结果

## 状态

已采纳并实施。

## 背景

`stream_llm_call` 同时返回 provider `LlmResponse` 与供 Agent usage 记录使用的耗时毫秒数。主响应、compaction retry、结构性 retry 和测试都按位置解构 `(LlmResponse, u64)`，第二个值的含义需要从实现注释推断。

## 决定

1. 内部流式调用结果使用 `StreamedLlmCall { response, duration_ms }`。
2. 响应解析、retry identity、取消、usage 记录和耗时测量仍由原有 owner 负责；调用方按字段读取结果。

## 替代方案

- 保留 tuple，只在调用点改成长变量名：拒绝，函数契约仍要求每个调用方按位置区分 response 与 duration。
- 把 duration 合并进 `LlmResponse`：拒绝，provider 响应和本地 wall-clock 执行时间属于不同 owner 与生命周期。

## 影响与验证

- 仅改变 Agent 内部 Rust 结果类型；provider 响应、耗时值、usage 持久化、事件和 retry 行为不变。
- 命名路线图 §5.7 继续保持 Active；其他 crate API 与跨层名称仍需复核。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(LlmResponse, u64)` 及主调用和 retry 调用点的 tuple 解构；无需数据或 wire 迁移。
