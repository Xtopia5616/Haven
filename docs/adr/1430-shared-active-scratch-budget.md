# ADR 1430：为活动 scratch 共享容量预算

## 状态

已实施；验证结果记录在本 ADR 的验证节。

## 背景

ADR 1428 为 Shell、LSP、Git、MCP stdio 与 Everything 定义了每目录容量和并发上限。每个活动 scratch 独立受限，但活动 Shell scratch 与受管进程 scratch 没有共同字节上限；受管进程到达 16 个后立即失败。重复的目录轮询也缺少可用于调整容量的摘要数据。

## 决定

1. 保留 ADR 1428 中的最小文件访问范围：LSP/MCP 项目读取范围、Skill setup 私有写入目录、Everything 选定搜索根与私有 scratch 均不扩大。确需额外资源时仍走现有资源授权；不新增按工具关闭隔离的开关。
2. 所有活动 Shell、LSP、Git、MCP stdio 与 Everything scratch 共用 4 GiB 聚合字节预算。各 scratch 原有的 256 MiB/64 MiB 与 100,000 项单目录上限继续有效；Shell scratch pool 的 2 GiB 上限继续有效。npm 与 Cargo 可重建缓存仍分别按 ADR 1428 独立限额，不计入活动 scratch 聚合预算。
3. Platform 提供受管目录预算租约。租约在容量轮询时更新该目录的观测值，并对活动 scratch 的观测总量做聚合检查；超过总量时只指定一个当前检查目录为清理责任方，释放租约后重新评估。检查仍是轮询约束，目录在两次检查之间可能继续增长，不能视作硬磁盘配额。
4. 每目录容量观测每分钟最多记录一次 debug 摘要，超限记录 warning；只记录字节数、目录项数和预算，不记录路径或文件内容。
5. 受管进程的 16 个 scratch 槽位满时最多等待 15 秒；等待 future 被取消时立即退出并释放排队状态。Shell 自己的八个并发槽位规则不变。

## 替代方案

- 只提高或分别调整目录上限不会限制多个工具并发时的总 scratch 占用。
- 继续在 16 个槽位满时立即失败会把瞬时峰值直接变成工具错误；无界等待会积累不可控的排队任务，因此采用短暂有界等待。
- Windows 文件系统硬配额、独立虚拟磁盘或额外隔离用户涉及部署与生命周期变化，本次继续使用运行时监测并明确其软限制性质。

## 影响与验证

预算只存在于进程内，不改配置、数据库、IPC 或持久数据，无需旧数据重置。单目录超限、聚合超限、租约释放、缺失目录兼容行为、槽位排队与超时均有测试覆盖。

2026-10-26 验收通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`RUST_TEST_THREADS=4 cargo test --workspace --locked`；UI `check`、`test:run`（170 文件、1519 项）与 production `build`；ADR index、crate dependency、IPC contract（109 handlers）与 IPC event（37 channels）检查；最终 `git diff --check`。

## 回滚

回滚本 ADR 对应代码即可恢复 ADR 1428 的分目录预算和满载立即失败行为；无持久格式需要清理或迁移。
