# ADR 0457：先归档再移出匹配回复

- 状态：Implemented
- 日期：2026-10-04
- 范围：Messaging inbox 的 request/reply mailbox 提取
- 关联：ADR 0158

## 背景

`take_matching_replies` 原先先从 mailbox 删除匹配回复，再追加 archive。若 archive 持久化失败，方法会返回错误，但回复既不在 mailbox，也不在 archive。mailbox 原地截断重写还可能在进程中断时留下部分内容。

## 决定

1. 先将匹配回复追加到去重的持久 archive；失败时不改 mailbox，回复仍可重试。
2. mailbox 替换使用同步写入的临时文件和恢复步骤：旧 mailbox 尚在时保留旧文件；旧文件已移除时提升临时文件。
3. 每个 mailbox 读写路径都在访问前执行临时文件恢复；archive 仍是有界历史，不改变 mailbox 的常规交付入口。

## 影响

归档失败不会消费匹配回复；mailbox 替换中断可由后续操作恢复。回复匹配规则、sender 校验、archive 去重和存储格式不变，无需重置。

## 验证

使用注入的 archive append 故障确认失败时 mailbox 原文保持不变，随后重试可取回回复；测试还覆盖 Windows 替换窗口中保留旧 mailbox 或提升完整临时文件。既有 mailbox claim/ack 与 archive 恢复测试验证其余交付路径。

## 回滚

回滚时不得恢复“先删 mailbox、后追加 archive”的顺序。若临时文件恢复实现需要回退，应先保证 mailbox 更新具有等价的崩溃恢复边界。
