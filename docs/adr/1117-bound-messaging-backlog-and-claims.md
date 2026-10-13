# ADR 1117：限制 Messaging 累计容量并分批排空 backlog

## 状态

已接受（2026-10-13）

## 背景

新写入 envelope 已有字段和序列化预算（ADR 1095），但每个 recipient 的 mailbox 与 `.processing` 可无限累积。claim、ack、history、find 和 selective reply wait 曾整体物化 mailbox；大积压会同时占用内存并长时间持有 inbox 全局锁。单纯限制返回条数仍会在锁内读写整份旧文件，不能限制旧 backlog 的单次工作量。

## 决定

- 新写入采用每 recipient 16 MiB 的逻辑未读预算。预算包含 mailbox 字节和 backlog 游标之后的源字节；`.processing` 是当前 backlog batch 的副本，不重复计数。expired 行在 claim 游标越过前仍占预算。容量超限时 delivery 返回明确错误，不覆盖或截断旧行。该值不是全局 inbox 磁盘配额。
- claim 将旧 `.processing`（优先）或 mailbox 通过同目录 rename 变为不可变 `.jsonl.backlog`，并建立 `.jsonl.backlog.state.json`。状态文件是唯一 cursor/active-batch 权威，带 version、source、phase、byte cursor、active end 和 attempt offset。`preparing` 在 rename 前落盘，允许重启完成提升；`active_end` 在写 processing 前落盘，允许重启从原始区间重建同一批；attempt offset 与 processing 中的逐条 `delivery_attempt` 对账，避免副本丢失时重置次数。
- 每批最多扫描 100 个 JSONL 行和 1 MiB，首行可占满整个 1 MiB。`.processing` 仅持有当前仍未 ack 的批次。部分 ack 不推进 cursor；全部 ID ack 后，先清空 processing，再原子地推进 cursor。崩溃发生在两步之间时允许同一批 at-least-once 重投，不跳过未确认行。旧 processing 先排空，随后才提升 mailbox；claim 期间新 delivery 继续追加到新 mailbox。
- archive 在 claim 时记录当前 source range；它不是 ack 状态。archive 去重扫描有界的完整 4 MiB archive，因此一个批次在 archive 成功后、游标推进前重试不会产生重复记录。archive 写失败时 active range 和 source 均保留。
- history 的直接 service 调用和工具调用共用 100 条结果上限，并从 archive、未确认 backlog 与 append mailbox 的有界 tail 合并稳定 `message_id`。find、last-received 与 selective reply 都使用有界行读取；selective reply 只从当前 mailbox 取最多 100 条，绝不移除已经进入 active processing 的邮件。broadcast 会将失败 recipient 和净化后的错误显式返回，容量拒绝不再静默消失。
- 已有 canonical JSONL backlog 无需重置：旧 `.processing` 与 mailbox 在首次 claim 时惰性提升，正文不复制或丢弃。单条历史行超过 1 MiB 时 claim/read 会 fail closed，保留原 backlog 与 cursor，不分配无界内存，也不静默丢弃；若用户需要恢复该队列投递，当前人工恢复路径是备份后按 `docs/release-and-reset.md` 重置整个 inbox。此举影响所有 peer、未处理消息、archive 与 deletion tombstone；备份只供离线检查，不会自动重新导入。旧非 canonical registry/JSONL 也仍按该 reset 流程处理。
- session tombstone 删除 `.jsonl.backlog`、cursor state、active processing、mailbox、archive 及所有 temp 内容。数据库 schema 与 IPC shape 不变，无数据库重置要求。共享 inbox 的进程必须在同一版本运行；不要与不认识 backlog sidecar 的旧进程同时操作。

## 替代方案

- 只限制 claim 返回条数仍会整体读取、解析并重写无限大的 processing 文件，且持锁时间随 backlog 增长。
- 通过 `.processing` 与 mailbox 整体拼接保留历史顺序，但每次有新消息都产生 O(backlog) 文件 I/O。
- 游标独立于 active batch 更新会在崩溃/部分 ack 时跳过未确认 ID；因此二者存放于同一原子状态记录，并只在整批 ack 后推进。
- 拒绝所有已存在超限 pending 数据会让升级用户必须重置 inbox。改为只拒绝新写入，让旧数据按小批次排空。

## 影响

每个收件人的逻辑 pending data 有上限，最旧 backlog 按稳定行序逐批确认；agent `inbox` 每次最多返回 100 条，最多 1 MiB JSONL 源数据。history 直接调用不再接受超过 100 的返回量。单个超过 1 MiB 的历史行会阻塞该 source 的后续 claim，正文仍原样保留。历史 mailbox 大于 16 MiB 的已有数据不会被截断；delivery 会继续拒绝，直到旧 pending 降到容量内。全局 `.lock`、archive 4 MiB 上限、稳定 message ID 与 duplicate consumer 可见性保持不变。

`crates/messaging/src/inbox.rs` 超过 800 行，本 ADR 暂不拆文件。`InboxBus` 的 mailbox、processing、backlog、archive 与 deletion tombstone 共享同一跨进程锁；状态迁移要求 `active_end`、processing 副本、archive 与 cursor 更新保持同一恢复协议。把文件写入/恢复 helper 拆成可独立调用的模块会让“持锁期间完成整段迁移”这一前提更难表达和审查。当前没有独立 storage owner 或稳定内部事务接口，故保留私有实现和同文件测试；若后续引入清晰的 storage transaction boundary，再按该边界拆分，不为行数本身拆分。

## 验证

- Messaging 测试覆盖累计预算拒绝、并发 append、100 行/1 MiB 批次、部分 ack、同 ID 重投、archive 失败重试、超大历史行保留、最大新 envelope 的 last-received、history 结果上限与 tombstone sidecar 清理。
- Tools 测试覆盖 broadcast 容量部分失败显式返回。
- 运行 workspace tests/check/clippy、IPC contract/event 检查、ADR index、Rust format 与差异检查；依赖真实 Windows 文件替换/关闭流程的发布验收仍由 release gate 完成。

## 回滚

旧 binary 不理解 backlog cursor sidecar；回滚前须停止所有共享 inbox 的 Haven 进程，并通过当前支持的 Messaging 版本把 backlog 排空，或按显式授权重置整个 `inbox/`。不得只删除 cursor state 或 backlog 文件，否则会破坏 source 与 processing 的 ack 对应关系。数据库无需回滚。
