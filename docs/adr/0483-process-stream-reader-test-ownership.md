# ADR 0483：将进程流读取测试归入实现 owner

## 状态

已采纳并实施（2026-10-05）。

## 背景

`process::read_stream_capped` 与 `read_stream_capped_with` 负责读取子进程 stdout/stderr、限制保留字节数、在超限后继续 drain，并把分块内容送入有界 live tail。该模块被 ActionService、Shell builtin 与 Skill runner 共用。

六项直接验证这些读取契约的测试此前放在 `action_service_tests.rs`。它们不创建或检查 `ActionService`，也不依赖其共享 fixture；将测试放在 service 测试文件中，容易让进程流行为看起来属于 ActionService。ActionService 生命周期测试仍需要验证 background 与 scheduled 共用的状态、终态及清理不变量，应继续由服务级测试统一持有。

## 决定

1. 将六项进程流 cap/drain/tail/UTF-8 测试移入 `process.rs` 的私有 `#[cfg(test)]` 模块，保持用例与断言不变。
2. 从 `action_service_tests.rs` 删除这些用例以及对 process helper 的导入；不移动涉及 ActionService 终态投影、数据库状态或 session 清理的测试。
3. 不改变生产代码、可见性、crate API、进程策略或输出上限。

预期收益是测试位置直接对应被测实现和共享消费者边界；通过导入与 fixture 依赖消失，可复核地确认测试归属转移成功。没有构建、运行时或性能收益目标。

## 替代方案

- 按文件行数把所有 ActionService 测试拆到 background/scheduled/views 子模块：拒绝。近期跨后台/定时任务的竞态测试验证共享服务不变量，按生产文件分散会掩盖其 owner。
- 将这六项继续留在 ActionService 测试模块：拒绝。测试不依赖该服务或其 fixture，且被测实现由共享的 process 模块拥有。

## 验证与影响

确认所有六个测试名仍存在于 `process.rs` 测试模块，且不再出现在 `action_service_tests.rs`；运行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings` 与 `git diff --check`。

没有数据库、配置、IPC、用户数据、生产逻辑或外部 API 影响。回滚只需把六项测试移回 service 测试文件并恢复对应导入；若未来 service 级调用行为需要测试，新增独立 ActionService 测试，不将 process 单元测试复制回来。
