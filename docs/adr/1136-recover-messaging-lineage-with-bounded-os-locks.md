# ADR 1136：用有界恢复与 OS 文件锁保护 Messaging 删除谱系

## 背景

Messaging 的跨进程锁此前依赖锁文件年龄判断失效。一次合法的长时间删除操作可能超过 stale 阈值，导致另一个进程删除仍被持有的锁并并发修改收件箱。删除谱系文件的临时副本此前也会在缺少 canonical 文件时直接提升，损坏的首次写入会阻塞所有 Messaging 操作；替换 canonical 文件时先删除旧文件还会留下恢复空窗。谱系映射同时没有读取和写入容量上限。

## 决定

- `.lock` 使用操作系统文件锁，等待上限为 15 秒；锁文件保持存在，释放由文件句柄关闭完成。不得单独删除 `.lock`；重置 inbox 前必须退出所有共享该目录的 Haven 进程。
- `deleted_sessions.json` 的序列化状态最多 64 MiB。读取和写入都执行同一上限；超限时 fail closed，不清除当前数据。
- 替换谱系文件时先完整写入并同步 `.tmp`，再把已提交版本保存在 `.bak`，安装新 canonical 文件后才移除备份。启动恢复会校验 canonical、temp 和 backup；有效 temp 可完成提交，损坏 temp 在有 backup 时恢复旧谱系，损坏的首次 temp 可清理后重试。
- tombstone 仍在消息正文和会话 registry 清理之前持久化；归档消息正文继续随删除操作一并移除。此决定不改变用户选择的删除语义。

## 替代方案

- 继续以文件年龄打破旧锁：拒绝。文件年龄不能区分崩溃进程与仍在执行的长操作。
- 只校验 temp、无 backup：拒绝。canonical 文件移走后的损坏 temp 可能抹去此前删除谱系。
- 无界读取 tombstone map：拒绝。该全局持久状态会在所有 Messaging 读写时加载。

## 影响与验证

这是 inbox 内部持久恢复格式的扩展，不修改 SQLite schema 或 JSONL envelope；现有 canonical tombstone 文件继续读取，不需要升级前重置。超过 64 MiB 的已有 canonical 文件会 fail closed。当前支持的人工恢复路径是先备份整个 `inbox/`，再按 [发布与数据重置](../release-and-reset.md) 执行完整收件箱重置；该操作会清除所有 peer 的队列、archive 与 tombstone，但不删除 SQLite transcript。

验证覆盖有效中断 temp 提升、损坏首次 temp 重试、替换中断后恢复旧谱系、OS 锁互斥与句柄释放；运行 Messaging locked tests、严格 Clippy 和格式检查。OS 文件锁依赖共享目录的所有 Haven 进程使用本锁协议。

## 回滚

没有数据库迁移。回滚到旧版时应先完全退出所有 Haven 进程；旧版可能把新留下的 `.lock` 视为 stale 文件并移除。若需要跨版本回滚，先备份整个 inbox；不要并发运行不同版本访问同一个 inbox。
