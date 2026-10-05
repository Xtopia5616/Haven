# ADR 0478：SessionUsage 累计计数饱和契约

## 状态

已采纳并实施（2026-10-05）。

## 背景

`SessionUsage`、ReAct 的 `CumulativeUsage` 和 `AgentUsage` 事件累计字段均使用 `u32`。live `UsageTracker` 对会话累计 token 使用 `saturating_add`，因此 live 事件会封顶于 `u32::MAX`。持久 summary 的增量 SQL 原先用普通 `INTEGER +`，允许存入 DTO 无法读取的值；回滚/删除后重建则对 `i64 SUM` 使用 `as u32`，会回绕。例如两条各 `3,000,000,000` token 的调用在增量投影中得到 `6,000,000,000`，重建后却得到 `1,705,032,704`。恢复 session 时读取超出 `u32` 的 summary 也会失败。

这使 live 统计、持久 summary、resume seed 和 rollback rebuild 对同一批 usage detail 产生不同结果。累计计数应服从现有 `u32` tracker/event 契约，而不是扩展成新的 wire 类型。

## 决定

1. 每 session 的累计 prompt、completion、total、cached、cache-creation 与 cache-miss token 计数封顶为 `u32::MAX`；cost 与最新 context snapshot 继续使用各自既有语义。
2. 增量 SQL upsert 在累加时饱和于 `u32::MAX`，与 live `UsageTracker` 一致。
3. summary read 与 detail-row rebuild 对历史超范围累计值进行非负 `u32` clamp；detail `llm_usage` 仍保留每次调用的 `u32` 原值，rollback/discard rebuild 后与 live summary 保持相同的饱和结果。
4. 不改 schema、事件/IPC DTO、生成 TypeScript、单次 usage detail、SessionStore 写入 owner 或重置契约。

## 替代方案

- 扩展累计字段到 `u64`：会改变 SessionUsage、ReAct 累计 tracker、Agent event DTO、生成 TypeScript 与 UI 合约，而现有 live tracker 已明确定义为饱和 `u32`。
- 保持 SQL 非饱和并只改读取：存储投影与 live event/rebuild 仍会分歧，超范围旧值可能继续影响后续运算。
- 保持 `as u32` 回绕：会令 rollback/rebuild 的累计统计突然变小，不符合累计值语义。

## 影响与验证

session summary 现在与既有 live `u32` 累计行为一致；发生过超范围累加的 summary 在读取时安全封顶，重建会从 detail rows 得到相同封顶值。无 schema、wire、配置或用户数据格式变更，无需数据库 reset。

回归测试覆盖多项累计字段跨越 `u32::MAX` 的增量写入、detail 重建与旧 summary 超范围读取。验证命令如下：

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-memory -- repositories::usage::tests`（19 passed）
- `cargo test --locked -p haven-agent -- react::usage::tests`（7 passed）
- `cargo test --locked -p haven-memory`（389 passed，2 ignored）
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`（通过）
- `git diff --check`

## 回滚

回滚必须一并恢复原始读取、增量 SQL 与 rebuild 算术，并撤销边界测试；否则实现与本契约会再次分歧。无需数据 reset，detail rows 仍保留原始单次调用记录。
