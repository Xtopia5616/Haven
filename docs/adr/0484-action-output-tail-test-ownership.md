# ADR 0484：收拢 ActionOutputTail 测试归属

## 状态

已采纳并实施（2026-10-05）。

## 背景

`ActionOutputTail` 与 `ActionTailSnapshot` 的实现由 `action_output.rs` 拥有。ActionService 的测试文件中仍有两个直接测试 `ActionOutputPort` 的用例：大窗口上限与追加截断、以及快照对滑动窗口内容变化的检测。这两项不创建或验证 ActionService。

快照变化用例与 `action_output.rs` 的 `snapshots_advance_in_order_and_detect_sliding_content` 重复：后者已检查未变化时不发新快照、内容追加后的快照，以及达到上限后相同长度窗口发生滑动时的更新。大窗口上限用例则提供有别于现有 UTF-8 小窗口测试的容量边界样本，保留其覆盖并归入实现 owner。

## 决定

1. 将 `test_tail_buffer_bounded_at_exact_char_limit` 移入 `action_output.rs`，保留大窗口超限和后续追加时的上限断言。
2. 删除与 owner 内快照滑动测试语义重复的 `test_tail_snapshot_detects_sliding_window`，不复制到另一处。
3. 保留 `terminal_projection_keeps_final_output_and_releases_live_tail` 在 ActionService 测试中，因为它验证服务终态提交时如何释放 live tail。
4. 不改变生产逻辑、运行时可见行为、crate API 或输出容量默认值。

预期收益是将 `ActionOutputTail` 的单元契约集中到实现 owner，并去掉重复维护的一份快照测试；服务级终态投影仍由 ActionService 测试覆盖。

## 替代方案

- 将两项测试都搬入 `action_output.rs`：拒绝。快照变化语义已由 owner 内测试完整覆盖，复制会继续保留重复维护。
- 将直接 buffer 测试留在 ActionService：拒绝。其断言不涉及服务状态或生命周期。

## 验证与影响

确认大窗口测试只存在于 `action_output.rs`，重复快照测试已删除，而终态投影测试仍在 ActionService；运行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings` 与 `git diff --check`。

没有数据库、配置、IPC、用户数据、生产逻辑或外部 API 影响。回滚可恢复原 service 测试位置；若快照契约以后出现不同输入形态导致独立回归，可在 `action_output.rs` 增加针对该差异的新测试，不恢复重复用例。
