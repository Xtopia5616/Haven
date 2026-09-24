# ADR 0270：用量链路的 typed cache accounting

- 状态：Accepted
- 日期：2026-09-24
- 范围：LLM usage runtime input 与 durable-memory 写入边界
- 关联：[ADR 0223](0223-usage-runtime-ownership.md)、[ADR 0246](0246-router-result-projection.md)

## 背景

`inclusive`、`exclusive`、`unknown` 原本在 LLM、Agent 和 Memory 的 usage wrapper 之间以
`String` 传递。这样同一业务事实可以在多个层次被拼写、比较和转换，调用方也能意外写入任意
字符串；真正需要字符串的地方只有 SQLite 的 `llm_usage.cache_accounting` 列和 IPC usage
payload。

## 决策

将 `CacheAccounting` 放在 `haven-common` 作为跨层运行时类型，`haven-llm` 继续重导出它以
保持现有 LLM API 导出路径。`LlmCallUsageInput` 和 Agent 的 `UsageUpdate` 使用该 enum；
provider-facing `Usage` 继续沿用同一共享类型。SQLite 写入和 `LlmCallUsageInput` 的边界用
`as_str()` 转换为既有 snake_case 字符串；已有 `LlmCallUsage` durable projection 与 IPC
payload 继续保留字符串形状，旧数据库行不做迁移。新的 runtime 输入若从外部文本恢复，统一
通过 `CacheAccounting::parse`，未知值按 `Unknown` 处理。

这只收敛 cache accounting 的值域，不改变 provider adapter、token 计算、缓存计费规则、
IPC wire shape 或数据库 schema；`call_kind` 等其他字符串枚举另行处理，避免把多个契约变更
混在一个切片中。

## 验证

- Memory 与 Agent 的 usage test constructors 改用 typed enum；
- SQLite mixed-provider accounting 测试继续覆盖 inclusive/exclusive 行为；
- 通过 workspace check、focused test、fmt、严格 Clippy 和完整 workspace test。
