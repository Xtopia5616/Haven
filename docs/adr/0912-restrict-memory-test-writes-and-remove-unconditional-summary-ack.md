# 0912：限制 Memory 测试写入 API 并移除无条件 summary ack

## 状态

已接受并实现（2026-10-11）。

## 背景

`MemoryFactStore::persist_inferred_batch` 在默认生产 API 中公开，但源码调用图显示只被 Memory/Agent 测试使用。该写入不提交 ordinary extraction cursor，也不提交 summary completion marker；生产调用若采用它，会绕开“事实与抽取进度同事务”的恢复保证。生产 ordinary 与 summary 流程分别使用 `commit_ordinary_extraction` 和 `commit_summary_extraction`。

`MemoryOutboxStore::clear_summary_extraction_cancellable` 也没有生产调用者，只有 store 单测使用。它无条件删除 summary marker，而生产 `MemoryOutbox` 已按读取到的 key/value generation 调用 `clear_summary_extraction_if_current_cancellable`，避免旧任务确认时删掉新一代 marker。

## 决定

- 将批量事实测试写操作改名为 `persist_inferred_batch_for_test`，仅在 Memory 单测和非默认 `test-support` 构建中编译。
- Agent 的 direct batch fixture 使用该显式测试入口；生产提取继续通过带 cursor 或 completion marker 的原子 commit 方法写入。
- 删除无生产消费者的 `MemoryOutboxStore::clear_summary_extraction_cancellable`，并将 Database 层对应的无条件 ack helper 限制为 Memory 单元测试。
- 保留 generation 条件确认 API，不改变扫描、重试、取消、事务或 marker 格式。

## 影响

默认生产 API 不再提供绕过 extraction cursor 的 batch write，也不再提供可无条件删除 summary marker 的 store 能力。测试语义继续覆盖事实批量 upsert、失败回滚与 marker 状态；生产成功/失败、重试恢复、schema、IPC 和用户可见行为不变，无需数据重置。API 变更不保留旧名称或兼容入口。

回滚时恢复测试方法的生产可见性和无条件 ack helper 即可，无持久数据恢复步骤。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 继续公开 `persist_inferred_batch` 并依赖调用方避免使用：拒绝，因为 API 本身可绕过 production extraction cursor/marker 事务。
- 保留无条件 summary ack 作为便捷方法：拒绝，因为 marker 可被并发新任务替换，确认必须比较当前 generation。
- 将 Agent/Memory 批量写入测试改成 raw SQL fixture：拒绝，因为这会停止验证 typed store 的事务路径；显式 test-support 保留所需覆盖而不扩大默认 API。
