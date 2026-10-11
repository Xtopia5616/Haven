# 0934：让 workspace 探测 helper 保持 Common 根 API

## 状态

已接受并实现（2026-10-11）。

## 背景

ADR 0128 已将 `haven_common::discover_workspace_root` 选定为 Common workspace 探测的唯一调用入口。当前源文件组织又通过 `pub mod workspace` 暴露 `haven_common::workspace::discover_workspace_root` 第二条路径，但全仓生产代码与测试没有使用模块路径；只有根路径调用方。

## 决定

- 保留 ADR 0128 的 `haven_common::discover_workspace_root` 根 API。
- 将 `workspace` 源模块改为私有，避免文件布局额外暴露第二入口。
- 不改变查找规则、返回值或路径解析行为。

## 影响

这是未发布 workspace API 的破坏性收口。Common 根函数仍是 Agent/Tools 等调用方使用的稳定共享操作。

## 验证

- 静态搜索确认不存在 `haven_common::workspace::*` 消费者。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- ADR 索引检查。

## 替代方案

- 同时保留根与 module 路径：拒绝，没有调用方需要第二入口。
- 迁移到 `haven_common::workspace::discover_workspace_root`：拒绝，会反转 ADR 0128 已建立的规范 API，而没有领域模型收益。

## 回滚

如果 workspace 子域未来成为包含多个共享 workspace contracts 的真实领域模块，应把 helper 和相关 contract 一起迁移到该单一模块路径，并删除根 re-export；不要恢复双入口。
