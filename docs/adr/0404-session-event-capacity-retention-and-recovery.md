# ADR 0404：Session 事件容量、保留与写入失败恢复

- 状态：已采纳（2026-09-29）
- 关联：ADR 0196（Session event sourcing）、ADR 0374（Session retention typed port）、ADR 0389（Session 事件保留与容量边界）、ADR 0395（阶段 9 容量验证）

## 背景

`session_events` 是会话恢复和回滚的唯一 durable authority。事件在所属会话仍保留期间完整 append-only；compaction 和 rollback 不裁剪历史。transcript event、messages/session_steps 投影在同一个 SQLite 事务中提交，只有成功提交后才发布有序 live event。

现有 session retention 是按 `sessions.created_at` 计算的整场会话年龄策略：默认 90 天，配置为 `0` 时禁用；应用启动后安排一次清理，随后每日清理。删除会话会级联删除其事件。它没有按累计文件大小或最近活动时间保留的含义。

ADR 0395 的固定长度数据只说明指定 JSON 文本形状在 Windows 文件型 SQLite/WAL 上的观测增长。它不能推导用户事件分布、未来单会话长度或所需磁盘空间。本 ADR 补充一组按生产 transcript record 字段构造的合成混合样本，并定义失败反馈与可重试的事务边界，不将样本当作用户容量保证。

## 决定

1. **不设置固定字节上限或容量告警阈值。** 当前样本不能支持可靠数值；不承诺单个 session、整个数据库或 Haven 数据根目录的最大大小，也不承诺最低磁盘需求。应用会按 SQLite 返回的 `SQLITE_FULL` 和磁盘 I/O 错误给出不同程度的写入失败恢复提示。
2. **保留现有整场 session 年龄策略。** 截止时间按 `created_at`，默认 90 天，`0` 禁用；清理在启动后执行一次并每日执行。用户显式删除同样以整场 session 为单位。保留期内不得从 `session_events` 单独裁剪事件。
3. **保留 SQLite 可复用页，不在常规 retention 中自动 `VACUUM`。** 删除 session 后 checkpoint 可清空 WAL，数据库主文件可能维持原高水位；释放页可供后续写入复用，但删除或 retention 不保证释放操作系统可见的磁盘空间。`VACUUM` 需要重写数据库并可能额外占用磁盘，因此不是低容量自动恢复手段。
4. **SQLite 事务失败不发布部分事件或投影。** Event store 的事务体或 `COMMIT` 失败时都尝试 rollback；只在 `COMMIT` 成功后广播。原始 SQLite 错误保留在 error chain，以便将 `SQLITE_FULL` 归为明确空间不足、将 SQLite 磁盘 I/O 错误归为原因未定的存储失败。rollback 失败会记日志，不覆盖原始失败。
5. **向会话错误界面给出可执行恢复提示。** `SQLITE_FULL` 提示用户先在数据库所在磁盘释放空间，再点“继续生成”；SQLite I/O 错误提示检查磁盘空间和可用性。提示说明删除会话不保证缩小数据库文件。SQLite I/O 错误不是磁盘已满的证明；物理盘 ENOSPC 的具体 Windows VFS 错误、桌面显示和恢复步骤仍需 disposable Windows profile/VM 验收。
6. **容量 profile 使用合成、可复现的生产形状混合，不采集用户数据。** 混合比例和文本长度是工程探针场景，不能代表 Haven 用户的实际事件大小分布。测试会输出 payload 分布、数据库增长、批次边界 WAL 峰值和 retention 后 freelist；本轮观测值见 ADR 0395。

## 替代方案

- 固定单会话/数据库字节上限或剩余空间阈值：当前没有真实事件分布、磁盘并发峰值或 ENOSPC 恢复证据，无法为具体阈值负责。
- 按 compaction root 裁剪历史：破坏完整恢复/回滚权威，不接受。
- 用 retention 或用户删除来承诺立即腾出系统磁盘空间：SQLite freelist 可供后续写入复用，但主文件不一定缩小，删除本身也可能需要写入，不接受。
- 自动 `VACUUM`：低容量时全库重写可能因额外空间失败，不作为自动清理操作。
- 将每次 SQLite I/O 错误都标记为磁盘已满：SQLite I/O 类包含其他底层故障，会误导用户；只对 `SQLITE_FULL` 给出确定的空间不足描述。

## 影响与验证

- 不修改数据库 schema、session event payload、配置格式或 retention 默认值；无需重置现有数据。
- 提交/回滚辅助函数和 SQLite 错误分类保留在 `session_events.rs` 的 SessionStore 所有权边界内：它们必须围绕同一连接事务，并与提交后的 cache invalidation 和 live broadcast 同步。该核心仓储文件已有较大体量，本切片不为少量事务错误处理代码另造模块，以免把一个原子边界拆到多个 owner。
- `SQLITE_FULL` 注入验证事件、消息投影和 live broadcast 的失败原子性，移除测试页数限制后同一 transcript 可以以 sequence 1 重试。
- 另以 deferred foreign key 让 `COMMIT` 失败，验证 event、projection、broadcast 均未泄漏，连接恢复 autocommit 后可以重新提交。
- 文件型容量 profile 在合成混合 event 上记录 payload 分布、主库增长、批次边界观测到的 WAL 峰值及 whole-session retention 后的 freelist；详见 ADR 0395。它不测试操作系统物理盘耗尽。
- 尚未以 disposable Windows profile/VM 填满物理卷或验证 GUI/安装流程。该发布验收继续开放，不应将 SQLite `max_page_count` fault injection 写成 ENOSPC 实测。

## 回滚

恢复前，必须先确认 `session_events` 的 durable append 和投影事务仍具有失败原子性；不能撤销事务失败 cleanup 而把未结束事务留给连接池复用。移除容量专用提示时，仍需保留安全、可操作的普通 session error 反馈。无 schema/config 需要回滚或重置。
