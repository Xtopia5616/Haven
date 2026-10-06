# ADR 0528：Admin 风险等级使用单一契约来源

## 状态

已采纳并实施（2026-10-06）。

## 背景

ADR 0512 为 20 个 model/native 共用 Admin 操作补齐了风险等级 parity 门禁。当时 `OperationContract` 与 `AdminSurfaces` 的风险值相同，因此决定等重复漂移再次出现再评估合并。复核实现后确认，两份声明分别维护相同的 20 个风险值；parity 测试能发现漂移，但仍要求维护者同步编辑两处。

本切片单独收敛风险等级 owner，不改变 ADR 0512 已建立的完整操作集合与 parity 回归门禁。

## 决定

1. 20 个 model/native 共用 Admin 操作的风险等级以 builtin `OperationContract.risk_override` 为唯一来源。
2. Admin typed metadata 按规范化的 model operation 名称读取该 contract；缺少显式风险时以 `High` fail closed，且现有 parity 测试继续要求每个共享操作显式声明风险。
3. operation contract 仍只提供共享风险值。Admin typed operation 继续拥有自己的 capability、参数解码、幂等性、并发资源和执行逻辑。
4. native-only `mcp_reconnect` 与 `mcp_refresh` 没有 model operation contract，继续由 native metadata 单独定义为 `Medium`，并保留各自测试。
5. ADR 0512 中“仅在 parity 回归或操作集合增加时再评估收敛”的暂缓条件由本 ADR 取代；其完整 parity 测试与风险值不变的决定继续有效。

## 替代方案

- 保留两份风险值声明并依靠 parity 测试：拒绝。测试可以及时发现漂移，但不能消除双重维护。
- 将所有 Admin typed metadata 都迁入 `OperationContract`：拒绝。该 contract 只拥有跨 model/native 共享的风险分类；capability 执行 metadata 与 typed Admin operation 保持共址。
- 调整任一操作的风险等级：拒绝。本次只改变来源，没有新的威胁分析证据支持改变值。

## 影响与验证

- 20 个共用操作的 model 与 native metadata 均从显式 `OperationContract` 风险读取；现有风险值、确认语义、网络分类、操作 schema、IPC、配置和持久数据不变，无需重置。
- ADR 0512 的完整操作集合和风险 parity 回归仍覆盖风险 contract 缺失或运行时映射漂移。
- `mcp_reconnect` 与 `mcp_refresh` 继续保持独立 `Medium` 风险，不进入 model-facing contract。
- 验收通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`（798 unit、7 integration passed；2 ignored）、`cargo clippy --locked -p haven-tools -- -D warnings`、`pwsh -NoProfile -File scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复 `builtin/admin.rs` 中原有的共享操作风险值，并删除本 ADR 与对应路线图更新即可；无数据库、配置、IPC 或用户数据需要恢复。回滚会恢复两份风险声明及其漂移可能。
