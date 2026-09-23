# ADR 0238：SessionStore 恢复读取端口

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` resume 的 ingress/未锚定消息读取
- 关联：[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0233](0233-session-managed-asset-lease-port.md)

## 背景

resume 同时需要两类不同恢复例外：按 durable ingress cursor 读取之后的消息，以及按既有两天时间窗读取未锚定用户消息。
此前 Agent 直接通过 raw `Database` 拼接这两个查询，容易把事件序号边界和时间窗边界误合并。

## 决定

1. `SessionStore` 提供两个独立只读查询，分别表达 ingress-cursor-after 和 unanchored-user-window 意图。
2. `resume.rs` 通过这两个查询读取，再按既有 message ID 规则合并；排序、时间窗、附件/media 投影和恢复例外不变。
3. 本切片不创建统一 `recovery_candidates` DTO，不改变 session event authoritative writes，也不替代其他仍需 raw Database 的幂等读取/写入。

## 影响与验证

恢复读路径的 boundary 归属进入 Memory Store，Agent 不再为这两类读取拼 SQL；无 schema/IPC 变化。memory 265 项、agent 465 项测试及对应严格 Clippy 通过。

## 回滚

恢复 `resume.rs` 的两个 Database 查询并删除 SessionStore read methods；不涉及持久数据。
