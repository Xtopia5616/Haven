# ADR 0474：以架构依赖表校验 Cargo crate 边界

## 状态

已采纳、实施并验收（2026-10-05）。

## 背景

`docs/architecture.md` 的依赖图和表、`scripts/check-crate-dependencies.ps1` 的 allow-list、各 crate 的 `Cargo.toml` 分别表达内部依赖边。本轮 crate 边界审计发现两处漂移：架构表漏列 `haven-agent → haven-messaging` 与 `haven-tools → haven-messaging`；脚本只拒绝 allow-list 以外的边，因而仍允许实际不存在的 `haven-app-binary → haven-skills/haven-mcp`。

这些差异会削弱 crate 拆分审查所依赖的基线。Cargo metadata 应是实际边的观测值，架构依赖表应是设计期望；校验必须同时报告新增未记录边和文档中已不存在的旧边。

## 决定

1. `docs/architecture.md` 的依赖表是内部 crate 直接依赖的唯一期望清单；表中使用完整 crate 名称，并按实际依赖补齐 Agent/Tools 到 Messaging 的边及 `haven-messaging` 行。概览图与表同步。外部依赖不属于内部边集合。
2. `check-crate-dependencies.ps1` 从该表读取期望集合，检查每个 Cargo workspace crate 恰有一行，并与 `cargo metadata --no-deps --locked` 得到的内部非 dev 依赖做精确集合比较。缺少、额外或过期的表行/依赖边都应失败；脚本不再另存一份 allow-list。
3. 本次只校准依赖文档与门禁，不改变 Cargo 依赖、crate owner 或运行时架构。Common/Tools 拆分缺少新边界和消费者收益证据；SessionStore 写侧仍需作为 append、projection 与 rollback 的事务 owner，暂不拆分。后续结构候选继续按路线图 §5.5 的准入标准审查。

## 验证与影响

- 运行 `pwsh -NoProfile -File scripts/check-crate-dependencies.ps1`，确认架构表与 Cargo metadata 的内部边集合一致。
- 运行 `git diff --check`；不涉及 Rust、UI、IPC、持久化或运行时行为，因此不运行相应功能测试。
- 无 schema、配置、数据、wire 或重置影响。

## 回滚

可回滚架构表和脚本解析器变更。回滚会恢复重复 allow-list 与不能发现文档旧边的检查缺口，因此若仍保留 crate 依赖门禁，后续应先修复其同步来源。
