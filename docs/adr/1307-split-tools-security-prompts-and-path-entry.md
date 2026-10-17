# ADR 1307：拆分 Tools 安全提示与路径入口模块

状态：已接受并实施（2026-10-18）。

## 背景

`crates/tools/src/security.rs` 同时承载授权 DTO/port、唯一 `AuthorizationEngine`、执行期 permit、filesystem policy、路径安全、本地安全矩阵和 permission prompt summary。第一步先迁移边界清楚且有独立调用者的提示/矩阵和本地路径入口；授权决策与执行期策略暂由原 owner 保持。

## 决定

1. 将本地安全矩阵、操作标签和 permission prompt summary 移入私有 `security/prompt.rs`，对应的七项脱敏、操作路由和 bounded prompt 测试一并迁入。`permission_prompt_summary` 保持原 `haven_tools` API。
2. 矩阵及其 case 类型保留 crate 内部使用：提示摘要通过矩阵限制 renderer 可见操作，内置工具回归测试验证覆盖面。仓库没有 crate 外消费者，因此从 `lib.rs` 移除公共 re-export；测试继续经测试态私有 facade 读取。
3. 将 `is_safe_local_path` 移入私有 `security/filesystem.rs`，继续由 `haven_tools` 根 re-export，供 App 的 skills/external command 使用。规范化与 reparse 检查仍调用既有 helper，不创建第二套路径策略。
4. 保持 `AuthorizationEngine`、`SafetyConfig`、`AuthorizationPort`、执行期 policy guard、grant/revoke 生命周期及 MCP/Skill 两种 process-start permit 的 owner 和行为不变。该 ADR 只完成 Tools security Slice 5 的首个子切片；后续拆分仍是 Active。

## 替代方案

- 继续把提示、路径入口和授权 engine 留在同一实现文件：拒绝；这些职责有可区分的模块和调用者，且提示测试可归属到唯一实现。
- 继续公开导出安全矩阵：拒绝；生产提示和回归测试都只在 Tools crate 内使用，公共导出不提供额外消费者价值。
- 复制路径校验或拆分授权策略：拒绝；将造成重复安全边界或第二策略 owner。

## 影响与回滚

无数据库、事件、IPC、配置或运行时安全语义变化，不需要重置。回滚时可将两个模块的实现与测试并回 `security.rs`，并恢复矩阵导出；App 调用的 `is_safe_local_path` 与 permission prompt API 路径保持不变。

## 验收

- Windows `cargo test --workspace --locked` 通过；Tools 975 passed、2 ignored，MCP integration 7 passed。首轮完整运行中一个与本变更无关的 Agent 并行取消用例出现时序失败；单测重跑通过，第二次完整 workspace 测试全部通过。
- `cargo clippy --workspace --locked -- -D warnings` 通过。
- `cargo fmt --all -- --check` 通过。
- ADR index 检查通过：1290 个唯一编号，所有本地 ADR 链接有效。
- 本结构切片未更改 sandbox/process 行为；真实 installer UI 与 HCS quota 超限验收仍按 §5.1 独立 Open Gate 跟踪。