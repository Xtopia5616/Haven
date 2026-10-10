# ADR 0870：共用有界文件逐行读取

## 状态

Accepted — 2026-10-10

## 背景

`file_read.rs` 为 `FilesTool` 的 line-mode read 与 `file_summary.rs` 提供 `read_line_bounded`；`file_outline.rs` 又维护了一份几乎相同的 `fill_buf` / `consume` 循环。两份实现都只保留单行上限内的字节并报告是否超限，Outline 额外消费超长行剩余字节，否则下次迭代会从同一行中段开始，破坏行号与解析边界。

## 决定

- 在 `builtin::file_line_reader` 建立唯一的 `read_line_bounded` owner，最多缓冲 `cap` 字节，并在超限时消费到换行或 EOF。
- File read 与 summary 保留现有调用策略：检测到超限后立即返回原有工具错误，因此额外消费尾部不改变返回值或后续副作用。
- File outline 继续解析保留的前缀，超限状态不改变 outline 的 symbol budget / pagination 语义；读取器在下一次调用前已位于下个逻辑行。
- 不在 helper 中加入取消策略；各调用循环继续在各自的工作边界检查 cancellation。

## 替代方案

- 保留两份循环，只修正 Outline：拒绝，会继续复制同一限长读取规则并允许边界行为漂移。
- 由调用方选择“保留尾部”或“消费尾部”：拒绝，当前消费者的可观察结果在超限时都会终止或需要继续到下一行；统一 reader cursor 契约更易审查。
- 直接读取完整行后再截断：拒绝，会让单行极大的文件突破内存上界。

## 影响与验证

- 删除 FileOutline 私有重复循环与 FileRead 原实现，将唯一实现提取到 `file_line_reader.rs`；FilesTool、summary、outline 的输入字节上限、错误/输出契约和取消边界不变。
- `cargo fmt --all -- --check`、`cargo check --locked -p haven-tools --tests` 与 `cargo clippy --locked -p haven-tools -- -D warnings` 通过；按项目指示仅编译测试目标，没有运行测试套件。
- 无 IPC、配置、持久化或安全契约变化，无数据重置要求。

## 回滚

如未来消费者需要保留超长行尾部，应为独立行为定义具名策略并验证 cursor 契约；不要重新复制限长读取循环。无持久数据需要重置。
