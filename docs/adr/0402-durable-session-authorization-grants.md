# ADR 0402：会话授权持久化与生命周期

- 状态：Accepted
- 日期：2026-09-29
- 关联：ADR 0009、0163、0188、0333、0351；X12 session event 写入契约

## 背景

确认 UI 的“本对话允许”此前只写进 `AuthorizationEngine` 的进程内 map。重启会清空授权，即使对应会话仍在历史中，并且重新打开同一会话后用户必须再次确认。会话授权需要与其拥有者拥有相同的数据生命周期，同时保留 ADR 0188 的期限、目标和效果三个决定维度。

## 决定

1. `haven_memory` 在 schema v31 新增 `session_authorization_grants` 表。每行包含 `session_id`、规范化 `capability_key`、`permission_scope`、`permission_target` 和 `effect`；`permission_scope` 必须为 `session`，目标只允许 `operation`、`group`、`tool`，效果只允许 `allow`、`deny`。主键为 `(session_id, capability_key)`，对同一会话和能力的新决定原子替换原效果与目标。
2. `session_id` 外键引用 `sessions(id) ON DELETE CASCADE`。单会话删除、清空历史和 retention 删除会话时同步删除授权；授权不放进全局 `config.toml`，不使用 `kv_store` 或 transcript event 作为第二真源。
3. `SessionStore` 是读写该表的 typed persistence boundary。确认命令解析 renderer 提交的目标类型后，仍由后端从当前 capability 的祖先计算规范 key。Agent 先成功持久化会话授权，再更新共享 `AuthorizationEngine`；数据库写失败时不应用授权并返回错误。
4. 新建或重载 session actor 前，Agent 读取并完整校验该会话的授权集，再恢复进 `AuthorizationEngine`。读取或校验失败时不安装 actor，防止带部分或缺失授权的状态继续运行。Security 配置 apply 仍会让 AuthorizationEngine 清空进程内 map 并递增 policy revision；apply 完成后 Agent 从 `SessionStore` 重新恢复所有仍有效会话的 grants。新的 policy、hard boundary、deny-first 规则和 receipt 校验仍在每次授权中生效。
5. 正常结束会话和应用关闭只清进程内 grant，数据库行继续有效；会话重开或应用重启后恢复。Error session 若保留 idle actor 供 `Continue` 使用，`ensure_session_loaded` 在继续前也会从 DB 重新加载该会话授权。永久规则与会话 grant 的撤销、重置范围见 ADR 0425：`revoke_permission` 不改变 session grants，`revoke_session_permission` 只删除给定 session+capability；永久与会话规则分别通过 `reset_permissions` 和 `reset_session_permissions` 管理。删除会话、清空历史和历史 retention 通过外键清理。X12 transcript rollback 只截断事件/投影 timeline，不撤销授权。

## 安全与替代方案

- 沿用 deny-first：永久 deny、会话 deny 优先于所有 allow；session grant 只对带有同一可信 session id 的请求生效。无 session 请求不会查询 session grant。
- 不把 session grant 写到全局 config，否则一次会话决定可能泄漏到其他会话，并与 Always 授权混为一谈。
- 不把授权决定写入 `session_events`：事件流是 X12 transcript/recovery 的 append-only 权威，而权限生命周期以 session FK 和 explicit revoke/reset 为准；rollback 不应改变信任状态。
- 整体策略 apply 的运行时 map 清空与 durable grant 恢复按顺序完成。恢复失败保持空 map 并向 apply 调用方报告失败；grant 写入失败不会有内存授权 fallback。

## 兼容、重置与回滚

数据库升至 schema v31，不对旧库运行时迁移。升级前按 `docs/release-and-reset.md` 删除 `haven.db`、`haven.db-wal` 和 `haven.db-shm`；保留 `config.toml` 不受影响。回滚到不支持 v31 的二进制也必须先恢复匹配旧二进制的完整数据库备份，或按文档重置数据库。

## 验证

回归覆盖 typed grant round-trip、同会话同 capability 替换、session scope / FK 校验、精确撤销和 reset、会话删除与 retention 级联、Agent persistence failure fail-closed、跨 supervisor 重载恢复、同 supervisor Error actor continue 前恢复、无 session 隔离及 Security apply 后恢复。相关检查使用内存数据库和隔离测试，不读取真实用户数据。
