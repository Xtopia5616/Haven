# ADR 1328：隔离并恢复单条超限 Messaging inbox 行

## 状态

已接受并实施（2026-10-19）。本决策细化 ADR 1117 的单行超限人工恢复路径；旧格式切换仍遵守 ADR 1296 的完整 inbox reset 要求。

## 背景

Messaging 以 1 MiB 有界读取 JSONL backlog。单行超过上限会 fail closed，保留源 backlog 和 cursor，但该行会阻止同一 recipient 后续消息投递。此前唯一支持的恢复方法是备份并删除整个 inbox，会同时清除所有 peer 队列、archive 与 deletion tombstone。

## 决定

1. 在 `haven-messaging` 增加 operator-only `haven-inbox-recovery` binary，提供 `inspect <session_id>` 与 `quarantine <session_id> <offset_bytes> <sha256>`。不增加模型可见的 `agent` operation，不允许按任意偏移跳过一行。
2. `inspect` 只查看当前 durable backlog cursor 对应的下一行，流式计算其长度和 SHA-256，不物化正文。目标必须确实超过既有 JSONL 行读取上限；单批仍有未确认消息时拒绝恢复。
3. quarantine 在共享 OS lock 内再次检查 offset、整行 SHA-256 和源边界。先把精确原始行以有界分块写入独立证据文件、flush/sync 并 rename，再经现有原子 backlog state helper 推进 cursor。若在两步间崩溃，证据文件名由 session/offset/hash 确定；同一请求校验已有证据后可幂等完成 cursor 更新。文件内容变化或文件写失败时不推进 cursor。
4. 每行证据最多 16 MiB，inbox 中所有 quarantine 文件总计最多 64 MiB。超限、无法确定行边界或损坏 cursor 继续 fail closed，提示备份并完整 reset。隔离证据按 session 删除，并纳入全 inbox session 枚举和清理。
5. Quarantine 只针对当前格式下阻塞 backlog cursor 的一行；它不是旧格式迁移，不更改 envelope contract，也不将该消息写入 archive 或 transcript。用户需要显式查看 inspect 输出，并将精确 offset/hash 传给 quarantine。

## 替代方案

- 继续只支持完整 inbox reset：会在一条坏行前连带丢弃无关 peer 队列与 tombstone，恢复影响面过大。
- 增加 `agent.quarantine`：拒绝。该操作会跳过持久消息，属于破坏性的管理动作，不应暴露给模型或由提示词触发。
- 按 message ID 删除：超限行无法经规范 envelope reader 解析，ID 不可信且不可用作唯一目标校验。
- 修改 backlog 原文件并重写后续字节：拒绝。会扩大崩溃恢复面并改变已有 byte cursor 的含义；保留 immutable source，推进 cursor 的现有模型更易恢复和审计。

## 影响与验证

- 新增 sidecar namespace `.jsonl.quarantine.<offset>.<sha256>.row`；证据与源行逐字节相同，正常 claim 不读取证据文件。
- 操作不会清空其它 peer，也不会丢失被隔离行的原始 bytes；它会有意跳过该行的投递。证据保留到 session 删除或 inbox reset，包含消息隐私，应按用户数据保管。
- 正常使用前应退出共享 inbox 的 Haven 进程，避免后台消费者在操作后立即继续处理后续消息。所有操作仍使用相同 `.lock`，并对符合当前锁协议的进程串行化。
- 旧二进制不识别 quarantine 文件；若回滚，先用当前版本删除相应 session/证据，或完整备份并重置 inbox。数据库无需重置。
- 负向与恢复测试覆盖 stale digest、有效行/活动 claim 拒绝、原始 bytes 保留、cursor 推进、后续消息可 claim，以及 session 删除清理证据。

## 回滚

不涉及数据库迁移。回退代码前停止共享 inbox 的 Haven 进程；quarantine 文件本身保留源行备份，但旧版本不会自动清理它们。按需先恢复或导出被隔离行，然后使用支持 quarantine 文件名识别的版本清理，或按 `docs/release-and-reset.md` 备份并重置整个 inbox。
