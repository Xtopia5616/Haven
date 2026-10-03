# ADR 0433：并行工具结果逐项提交与发布

## 状态

已接受（2026-10-03）。

## 背景

工具批次使用 `buffer_unordered` 并发执行，但之前先将所有 `CompletedTool` 放入计划索引槽，
直到最慢调用完成后才把整批 `ToolResult` 一次性提交到 `session_events` 并发布 UI。一个长时
运行或卡住的工具因此会隐藏其它已完成工具的 observation。

## 决定

1. 每个工具完成后，立即单独提交对应 `ToolResult` 事件及其物化投影；只有 SessionStore
   transaction 成功后，`CommittedUiPublisher` 才按该事件的 durable sequence 发布 Observation。
   assistant 在该批工具调用前输出的完整文本先提交并发布；每个 Action 卡在对应工具获得执行
   许可、即将启动时逐项发布，校验失败或需要确认的调用则在对应结果/等待状态前发布。并行执行
   仍保留，Observation 按各工具完成顺序逐项发布。
2. Durable event sequence 按实际完成与提交次序递增。`step_number + action_index` 是同批结果的
   稳定关联身份；批次完成后，进程内 canonical transcript 依 assistant 原始调用顺序更新。
3. 恢复投影把同一步的 ToolResult 按 `action_index` 排序。若进程在批次中途退出，发送前的
   `sanitize_canonical` 将实际结果与原调用身份配对，并在缺失槽位插入 Interrupted 结果，仍按
   assistant 调用顺序生成合法 provider history。
4. 通知、ask 与失败重试聚合最终按 `action_index` 规范化，避免并发完成时序改变后续控制决策。
   工具用量仍在批次排空后统一写入。
5. 不改变 schema、IPC payload、Action 卡片身份或确认门禁。单项结果分别争取 SQLite 写锁，
   换取完成结果的低延迟可见性；批次总量仍受 64 项上限与 8 个并发 future 限制。

## 替代方案

- 保持整批原子提交：写锁次数少，但 UI 被最慢调用拖住，保留原故障表现。
- 在 durable commit 前发临时 Observation：进程崩溃或投影失败时 UI 会显示未提交结果，违反
  committed-only UI 契约。
- 让 canonical 顺序跟随完成时序：恢复与 provider 输入受工具运行竞态影响，拒绝。

## 影响与验证

无数据库重置或外部契约变化。回归覆盖一个慢工具未结束时另一个已完成 Observation 可见、
完成顺序与 `action_index` 顺序分离，以及中断后部分批次按调用顺序修复。

## 回滚

回滚逐项 transcript commit、canonical/recovery 排序、相关测试与本文档即可；无需重置数据库。
