# ADR 0512：Admin model/native 风险等级 parity 回归门禁

## 状态

已采纳并实施（2026-10-05）。

## 背景

Admin 的 model-facing `OperationContract` 与 native `AdminSurfaces::metadata` 分别描述 operation 风险等级。2026-10-05 复核发现当前 20 个共用操作的值一致，没有发现现存风险等级漂移；native-only 的 `mcp_reconnect` 和 `mcp_refresh` 另有两项。

两处策略声明独立维护，且 ADR 0506 记录了 MCP 操作曾在 model/native 网络分类上真实漂移。现有测试分别覆盖若干单项风险，但没有确认所有共用操作都匹配，也没有确认每个 model schema 操作均有对应 typed request。该缺口会让未来策略改动出现一端遗漏时无法及时发现。

## 决定

1. 为 20 个 model/native 共用 Admin 操作增加完整风险等级 parity 回归测试。
2. 测试从五个 model tool 的实际 `oneOf` schema 收集操作集合，并与 typed `AdminRequest` 样例集合比较；新增或漏掉 model-visible 操作时，集合检查必须失败。
3. 对每个配对操作，比较 model tool 实际风险、native `AdminSurfaces::metadata` 风险和 `OperationContract` 中显式声明的风险；缺少显式 model 风险声明也必须失败。
4. 保留 native-only `mcp_reconnect` 与 `mcp_refresh` 的独立测试和 Medium 风险等级，不把它们纳入 model-facing contract。
5. 本切片不改变现有风险等级、授权/确认语义、运行时行为、配置、IPC 或持久数据，无需重置。

## 替代方案

- 立即把 native Admin metadata 全面改为从 `OperationContract` 读取：暂不采用。当前没有值漂移，完整 parity 测试足以防止本轮已证实类型的遗漏；直接合并两条 metadata 构造路径会扩大改动范围。若后续 parity 回归表明独立声明仍反复漂移，再评估统一来源。
- 只断言当前已知风险值：拒绝。单项断言不能证明操作集合完整，也容易遗漏后续新 operation。
- 修改任何 operation 的风险等级：拒绝。本次没有新的威胁分析证据支持改变现值。

## 影响与验证

- 风险等级仍分别由 model contract 与 native typed operation metadata 提供，但测试会逐项将两者与显式共享 contract 声明对齐。
- 操作集合从实际 model schemas 读取；native-only 操作维持独立边界。
- 该切片只触及 `haven-tools` 与路线图/ADR，无数据库迁移、配置变更、IPC 变更或用户数据影响。
- 验收：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

删除 parity 测试并恢复路线图 Candidate 描述即可回滚；没有运行时、配置或持久数据需要恢复。
