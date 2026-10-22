# ADR 1309：将 Tools 执行期授权与进程启动许可迁入私有模块

状态：已接受并实施（2026-10-18）。

## 背景

`security.rs` 的唯一 `AuthorizationEngine` 管理 policy revision、workspace generation 和 revoke；派生的执行期 permit 与 MCP/Skill 子进程入口实现散落在同一文件。它们依赖 engine 的 live `SafetyConfig`，但有各自不同的消费接口和生命周期，需要按执行边界归档，同时继续由唯一 engine 决策。

## 决定

1. 将 proof-derived `ExecutionPolicyAuthorization`、持有 live config read guard 的 `ExecutionPolicyPermit`、MCP stdio process-start authorization 和 Skill process-start adapter 迁入私有 `security/execution.rs`。
2. `security.rs` 仍 re-export crate 内既有 permit 类型供当前 Tools/App composition 路径使用；不公开新增模块，也不新建 engine、配置锁或撤权注册表。
3. `ExecutionPolicyAuthorization::acquire` 继续在操作期间持有同一个 config read guard，并复核 proof expiry、policy revision 与 execution workspace generation。现有全局、tool-settings 与 session-trust revoke 信号及取消 token 语义不变。
4. 保持两个不同的进程启动许可：MCP stdio permit 校验 process-boundary revision、workspace generation、transport/sandbox 组合与 `NetworkPolicy::Open`；Skill permit 由 proof-derived execution authorization 验证执行期策略。两者不合并成通用 permit。
5. 测试构造器和 MCP acquire 测试入口仅在 crate 测试/安全 facade 内可见；process-start 与撤权测试继续依赖唯一 engine fixture。

## 替代方案

- 将 MCP 与 Skill 许可合并：拒绝；前者授权一个 manager-owned MCP server generation，后者授权一个 proof-bound 工具 attempt，风险边界和失效信号不同。
- 复制 config 快照或另建执行撤权 owner：拒绝；会允许策略更新与 side effect 之间出现第二份状态。
- 将进程 permit 暴露给其他 crate 直接构造：拒绝；调用方只经 Tools 内部 adapters 消费窄 capability。

## 影响与回滚

无授权结果、IPC、数据库、配置或外部 trait 形状变化，不需要重置。回滚时把执行 permit/types/impl 并回 `security.rs` 并恢复原 crate 内导入。

## 验收

- Windows `cargo test --locked -p haven-tools -- --test-threads=4`：975 passed、2 ignored；MCP integration 7 passed。
- `cargo clippy --locked -p haven-tools -- -D warnings` 通过。
- `cargo fmt --all -- --check` 通过。
- 实际 installer UI 与通用发布验收不受该模块移动影响，仍由路线图 §5.1 跟踪。
