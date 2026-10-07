# ADR 0635：统一 Agent SessionRun / ReActRun 术语并删除空壳预算类型

## 状态

已采纳并实施。

## 背景

ADR 0616 已将 Agent crate-root 的 `RunEngine` 明确为 `SessionRunEngine`，但相邻类型仍叫 `RunHandler`、`RunAdmission`、`RunPermit`、`DirectRunLease`；actor 的准入 claim 和 ReAct loop 的输入、重放、输出也使用不带 owner 的泛名。Agent 同时有 `ToolRun` 这个持久工具执行实体，离开局部模块后这些名称不足以辨别概念边界。

结构检查发现这些类型不是同一结构的旧版与新版：admission 持有并发上限和活动计数，permit 代表一个 RAII 容量槽，direct lease 在清理完成前保留单个会话的直接运行权，actor claim 表达 actor 是否接受该次运行，而 ReAct loop 数据只承载调用输入、transcript replay 与循环结果。它们有不同 owner、生命周期和释放点，没有应当合并的重复状态。

另一个公开类型 `RunBudget` 在 Agent crate root 导出并派生 serde，但全仓没有读取、构造、持久化或发送它的消费者。唯一相关集成测试 `pause_snapshot_includes_run_budget` 只断言 Ask 后 session 进入 Paused，并未观察 budget 或 snapshot；实际 step budget 计算已有 loop unit tests 覆盖。ADR 0220 第 3 项曾为兼容性保留该类型；live loop 使用的私有 `RunBudgetConfig` 已在本 ADR 改名为 `ReActStepBudget`，snapshot sidecar 和 ReActSnapshot 已删除。

## 决定

1. 将会话 supervisor 一次执行与准入资源统一命名为 `SessionRun`：`SessionRunHandler`、`SessionRunAdmission`、`SessionRunPermit`、`DirectSessionRunLease`、`DirectSessionRunWaiterGuard`、`DirectSessionRunWaiters` 和 `SessionRunClaim`。相应 supervisor/actor 方法、命令、字段和测试名同步使用 `direct_session_run` / `claim_session_run`。
2. 将 ReAct loop 自身的调用数据统一命名为 `ReActRunInput`、`ReActRunReplay`、`ReActRunOutput` 与 `ReActRunBoundary`；活动循环类型为 `ActiveReActRun`。一次运行的 step 预算内部类型叫 `ReActStepBudget`，不把 step 上限误称为通用资源预算。
3. 删除无消费者的公开 `RunBudget` 定义及 crate-root 导出，不留旧名 alias；删除只检查暂停状态、名字却声称验证 run budget snapshot 的过时集成测试。ADR 0220 关于该类型的兼容性保留决定由本 ADR 部分替代；其余 sidecar 决定仍有效，真实 step budget 计算与 Ask pause 行为继续由现有测试覆盖。
4. 保持 admission 算法、permit RAII、direct lease 的清理/取消行为、actor claim 和 ReAct loop 行为不变。`SessionRun`、`ReActRun` 不是新持久实体；`ToolRun` 仍独立表示可脱离当前 turn 持久运行的工具工作单元。

## 替代方案

- 把 admission、permit、lease 和 claim 合成一种 session-run state：拒绝。它们分别拥有全局并发上限、单槽释放、直接运行清理保留和 actor 内接受结果，合并会混淆权威状态与释放时机。
- 全部改成 `ReActRun*`：拒绝。SessionSupervisor/dispatcher 的准入资源属于会话运行边界；只有 ReActEngine 的循环调用数据属于 ReAct loop。
- 保留 `RunBudget` 或改名为 `SessionRunBudget`：拒绝。当前没有消费者、持久化字段或 wire 用途，保留只形成空的公共 API；实际 step budget 由 `ReActStepBudget` 表达。

## 影响与验证

- 这是 `haven-agent` Rust source API rename，并删除一个未使用的公开类型；无配置、数据库 schema、事件、Tauri IPC 或序列化数据迁移。
- 现有执行和 admission 逻辑仅改符号名；当前源码不再导出旧名。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`，并使用 `target` 下全新隔离 APPDATA；ADR 索引、diff 与 staged diff 检查。

## 回滚

恢复原 Rust 符号名并重新导出 `RunBudget`。无需数据库或运行数据重置；不保留兼容 alias。
