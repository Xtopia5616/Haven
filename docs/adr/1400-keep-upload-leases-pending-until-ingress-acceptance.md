# ADR 1400：上传租约在输入接受后再绑定会话

## 状态

已接受并验证 — 2026-10-25。补充 ADR 1399 的失败保留/清理边界，以及 R08 的受管资产交接契约。

## 背景

普通附件在写入 `uploads/<batch>/` 后，必须由 process-local pending/session lease 保护，直到消息引用可由持久会话查询。此前上传入口在已选会话下立即创建 session lease；`submit_transcript` 只有在没有活动会话时才释放失败请求的 pending lease。因而活动会话附件提交失败后，GC 会一直把未被消息引用的上传路径视为受保护资产，直到会话结束或进程重启。

## 决定

1. 上传写入完成后统一持有 pending lease，不在 transcript ingress 结果出来前直接绑定活动会话。pending lease 与上传目录写锁共同保护提交到清理扫描之间的窗口。
2. ingress 返回 `SessionCreated` 时，将 pending lease 绑定到实际返回的 session ID；返回 `Supplemented` 且请求指定了会话时，绑定到该会话。这样也覆盖活动 session 已过期、Agent 实际新建会话的情况。
3. ingress 失败时释放本请求的 pending lease。`Supplemented` 没有目标 session 时也释放；若持久消息已经引用路径，基于 SessionStore 的 durable reference 仍保护文件。
4. 清理必须先成功读取持久附件引用；无法读取时 fail closed。重启后 registry 为空仍可由 durable references 保留消息附件，并回收真正孤立或重复的上传副本。
5. 不增加持久资产索引或新状态表；沿用 pending/session lease、消息引用及现有 GC 锁。

## 验证

- `test_failed_existing_session_submission_releases_upload_for_gc` 覆盖已有会话请求使用 pending lease、失败分支释放后下一次 GC 删除孤立上传的租约顺序。
- `test_accepted_existing_session_submission_binds_upload_lease` 覆盖成功接受后绑定 session lease、会话终止释放租约后未被 durable reference 引用的文件可清理。
- 两项测试覆盖注册表和 GC 生命周期原语；`submit_transcript` 的失败/成功分支按上述顺序调用这些已有 lease 操作，未声称此处具备完整 Tauri command E2E。
- 既有测试覆盖上传与 GC 并发、重启后 registry 为空、消息引用保留、重复孤立批次清理、失败批次回滚及 reparse point 拒绝。
- `cargo test --locked -p haven-app-binary --lib`：275 项通过。`cargo clippy --locked -p haven-app-binary -- -D warnings` 与 `cargo fmt --all -- --check` 通过。
- 真实 Windows 文件选择、消息提交故障及 renderer/应用重启的端到端流程仍归 §5.1 验收，不由本组单测声称通过。

## 影响与回滚

不改变消息、session event、schema、配置或 IPC，无需重置数据。回滚时必须保留“失败请求不留下永久 session lease”这一行为；不得将上传直接绑定当前 UI 选择后又在失败时只释放 pending lease。
