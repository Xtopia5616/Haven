# ADR 1076：为日志尾部和配置备份身份返回具名结果

## 状态

已接受（2026-10-13）

## 背景

Common 的日志 tail helper 以位置 tuple 返回展示行和完整行数，调用方还要立即再拼成另一个多字段 tuple。配置备份文件名解析也以 `(timestamp, process_id, sequence)` tuple 返回稳定身份，排序和保留策略依赖调用者记住字段次序。日志读取还存在只在 Common 内部使用的 `(bytes, truncated)` tuple。

## 决定

- `read_tail_lines_with_count` 返回具名 `LogTailLines { lines, total_lines }`；读取的原始尾部字节通过私有 `TailBytes { bytes, truncated }` 表达。
- Admin service 使用具名 `ResolvedLogTail { path, tail }`，避免在 blocking task 和 renderer projection 之间使用多字段位置 tuple。
- 配置备份解析命名为 `parse_automatic_backup_identity`，并返回 `AutomaticBackupIdentity { timestamp_secs, process_id, sequence }`；按具名 identity 排序，`TimestampedBackup` 保存排序 identity 和路径。

## 替代方案

- 保留 tuple 并依赖函数文档说明顺序：不采纳，因为读写两端仍需要依靠位置解释稳定语义。
- 仅在调用点立即解构 tuple：不采纳，因为那没有让接口本身表达结果角色。

## 影响

Common Rust helper 的返回类型与 Tools 内部投影类型改变；日志 tail 的行数、脱敏、错误和配置备份保留顺序保持不变。没有持久化或 IPC shape 变化，无需重置数据。

## 验证

- Common 配置备份保留测试和日志/管理员相关 workspace tests 通过。
- `cargo test --workspace --locked`、Clippy 与格式检查通过。

## 回滚与数据重置

无持久化变化，不需要重置。若 Common log helper 的跨 crate shape 回滚，需同步回滚 Tools 消费端与具名投影结构。
