# ADR 1311：拆分 Tools 授权 engine 并收敛有序边界检查

状态：已接受并实施（2026-10-18）。

## 背景

在 contract、execution permit、filesystem policy 与 prompt 拆出后，`security.rs` 仍承载唯一 `AuthorizationEngine`、`SafetyConfig` 和全部授权状态/撤权测试。授权检查、receipt 验证与 execution proof 验证还各自重复禁用操作、host-process sandbox、network 与 path 检查；这些入口必须以相同顺序拒绝，避免不同生命周期的授权结果不一致。

## 决定

1. 将 `SessionGrants`、`SafetyConfig`、唯一 `AuthorizationEngine`、engine helper 与授权 engine 测试迁入私有 `security/engine.rs`。`security.rs` 继续 re-export `AuthorizationEngine`，保留现有公共/ crate 内消费路径。
2. `SafetyConfig`、engine 的少量锁/撤权字段和时间/hash helper 仅在私有 `security` 模块内部按需可见，以便 execution permit 继续读取同一配置锁和撤权信号；不新增跨 crate API、配置副本或撤权 owner。
3. 新增 `check_policy_boundaries`，由 `authorize_for_execution`、`verify_receipt` 与 `verify_execution_proof` 共用硬边界检查，首个失败顺序固定为：禁用操作 → WorkspaceWrite 下的 host process → 网络策略 → 路径/sandbox。
4. 入口特有的验证次序和语义保持原样：授权决策先检查共享边界，再应用 Plan、Deny/Allow 与 critical confirmation；receipt 先验证 capability/input/revision/expiry/risk/critical floor，再检查共享边界和 deny；proof 先验证 request binding/expiry，已确认 proof 仍复用 receipt 验证，auto-approved proof 先验证 live revision，再检查共享边界、Plan、deny 与 fresh-confirmation floor。
5. 新增顺序回归测试，构造同时触发多个拒绝条件的输入，逐项断言首个拒绝原因仍遵守固定优先级。现有 engine 状态、grant、receipt/proof、revoke、workspace、MCP/Skill 与路径失败测试随 engine 一同迁移。

## 替代方案

- 仅移动 engine 而保留三份边界检查：拒绝；相同硬边界的次序可能随入口分别漂移。
- 将 receipt 和 proof 的整个验证流程合并：拒绝；二者具有不同的身份绑定、时效、critical-floor 和恢复规则，共用仅限于有序硬边界检查。
- 复制 `SafetyConfig` 或把可变 engine 暴露给其它 crate：拒绝；execution permit 必须持有唯一 live policy lock，调用方仍通过稳定 façade/port 访问能力。

## 影响与回滚

无授权结果、配置、IPC、数据库、用户可见安全行为或 crate 依赖方向变化，不需要重置。回滚时可把 `engine.rs` 内容和 engine 测试并回 `security.rs`，并将 `check_policy_boundaries` 展开回原三处；调用方 façade 路径无需修改。

## 验收

- Windows `cargo test --workspace --locked -- --test-threads=1`：全 workspace 与 doc tests 通过；Tools 为 976 passed、2 ignored，MCP integration 为 7 passed。
- Windows `cargo clippy --workspace --locked -- -D warnings` 通过。
- `cargo fmt --all -- --check` 通过。
- 默认并发与 4 线程 workspace 测试分别出现一次不稳定子进程/SQLite 取消用例失败；`runner_removes_per_invocation_work_files_after_success` 与 Agent SQLite interrupt 用例单独复跑通过，单线程完整 workspace 运行通过。该失败未复现在受影响的 Tools 安全测试。
- installer UI 与通用 release 验收仍由路线图 §5.1 跟踪。
