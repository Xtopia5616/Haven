# 1143：原子恢复 partial checkpoint 并保留 ingress 顺序

## 状态

已采纳并实施。

## 背景

流式 assistant 文本先进入 `partial_messages` scratch checkpoint，结束或启动恢复时再提升为 canonical message。仅按内容或存在性判断会错误提升已被后续 ingress supersede 的旧 checkpoint；先删除 checkpoint 再插 message 又会在插入失败时丢失用户已看到的内容。

## 决定

- `partial_messages.ingress_seq` 保存 checkpoint 写入时该 session 的最新 canonical message ingress sequence。
- promotion 在 `BEGIN IMMEDIATE` 事务中读取 checkpoint 与最新 message sequence；若后来已有 message，则删除过期 checkpoint但不提升。
- 未被 supersede 的非空 checkpoint 在同一事务中写入 canonical assistant message 并删除 scratch row。任一步失败都回滚，checkpoint 保留以便重试。
- startup orphan finalizer 也选择有 checkpoint 的 error session，使失败的 promotion 能在后续启动重试。
- 删除仅供测试调用的单独“读取最后 message 时间” helper 和 `take_partial_message`；恢复使用共享投影水位，并且 checkpoint 只能通过完整 promotion 生命周期消费。
- schema 版本提升至 v42；旧版本不运行时迁移，按 `docs/release-and-reset.md` 删除数据库重建。

## 影响

恢复依据为 ingress 顺序，不依赖内容相等；scratch checkpoint 不成为 transcript/history 真源。v41 及更早数据库需要显式重置，其他配置与 inbox 不变。

## 验证

Memory repository 测试覆盖顺序 supersede、promotion 原子性、写入失败保留 checkpoint 和 orphan startup retry；运行 workspace 测试、严格 Clippy 与 schema/reset contract 检查。

## 回滚

回滚代码前需备份并重置 v42 数据库至回滚版本支持的 schema；不尝试把 ingress sequence 合并到旧 checkpoint shape。
