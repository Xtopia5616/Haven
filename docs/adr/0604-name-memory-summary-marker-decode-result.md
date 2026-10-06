# ADR 0604：命名 Memory summary marker 解码结果

## 状态

已采纳并实施。

## 背景

Memory 从 `kv_store.value` 解码 summary extraction marker，得到 `session_id`、`attempt` 与 `next_attempt_at_ms`。分页读取路径将 session ID 与 key 中的 session identity 核对，再向查询结果暴露重试状态；CAS 更新路径只需恢复 session ID 后写回新的 attempt/deadline。私有解码器原返回 `(String, u32, i64)`，两个调用点均按位置读值。

## 决定

1. 私有结果改为 `DecodedSummaryExtractionMarker { session_id, attempt, next_attempt_at_ms }`。
2. 分页查询按具名字段校验 identity 并构造 `SummaryExtractionMarkerState`；CAS 更新按 `session_id` 编码新 marker。
3. legacy marker 的 `attempt=0`、`next_attempt_at_ms=0` 默认值以及拆分 session ID 时从右向左解析的行为保持不变。

## 替代方案

- 保留 tuple 并给闭包变量起别名：拒绝，解码器的两个消费者仍依赖序号表达字段语义。
- 复用外层 `SummaryExtractionMarkerState`：拒绝，该类型只表示查询投影中的 retry 状态，不包含由持久值解出的 session identity。
- 修改 kv marker 编码：拒绝，此切片只命名内存解析结果，不需要持久格式变更。

## 影响与验证

- 仅改变 Memory crate 私有 decoder 返回类型，不改 `kv_store` 持久值格式、查询 DTO、retry policy 或 CAS 比较条件。
- 命名路线图 §5.7 继续保持 Active。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-memory`、`cargo clippy --locked -p haven-memory -- -D warnings`、`cargo test --locked -p haven-memory`、ADR 索引及 staged diff 检查。

## 回滚

恢复 decoder 返回 `(String, u32, i64)` 和两个调用点的位置解构；无需数据库重置。
