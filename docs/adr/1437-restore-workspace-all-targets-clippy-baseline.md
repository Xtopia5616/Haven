# ADR 1437：恢复 workspace 全目标严格 Clippy 基线

## 状态

已实施 — 2026-10-27。

## 背景

ADR 1436 的标准 workspace 严格 Clippy 与 Agent 全目标严格 Clippy 已通过，额外的 workspace `--all-targets` 检查在 Common、Messaging、Memory 和 LLM 的测试代码中报告 10 项既有告警。它们不涉及已实施的生命周期修复，作为独立维护切片处理。

## 决定

- 默认值与初始覆盖字段在结构体初始化中表达，保留后续边界值修改。
- 单引用切片使用 `std::slice::from_ref`，避免不必要的字符串克隆。
- 键存在性使用 `contains_key`；测试分支直接返回原有 Result。
- 把夹在生产条目之间的测试模块原样移到文件末尾，保持模块内容、可见性与测试名称不变。
- 不新增 lint 豁免，不调整生产行为、网络校验或测试 loopback 边界。

## 影响、重置与回滚

仅整理既有测试代码。无公共 API、配置、持久化、IPC 或事件契约变化，无需重置。文件体量不是本次重构依据；只调整测试模块位置，不拆分生产 owner。回滚为恢复相应测试写法，不影响用户数据。

## 验证

- cargo clippy --workspace --locked --all-targets -- -D warnings：通过，包含所有 workspace crate 的测试目标；无新增 lint 豁免。
- cargo fmt --all -- --check、ADR index（1334 条）与差异检查通过；独立脚本核对流适配器的生产函数及测试模块内容逐字一致，仅移动位置。
- Rust 1.99.0；最终工作树 cargo test --workspace --locked -- --test-threads=4 全部通过，包含 Agent 705、App 278、Common 296、LLM 561、Memory 511、Messaging 75 项通过测试；原有 ignored 保持不变，doc tests 通过。
- 本切片仅改 Rust 测试代码，未重跑未受影响的 UI 门禁；本轮 UI 1539 项测试、Svelte check 和生产构建通过，见 ADR 1435。未生成发布包。
