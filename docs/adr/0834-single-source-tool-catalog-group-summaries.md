# ADR 0834：统一工具目录的分组摘要来源

## 状态

Accepted — 2026-10-09

## 背景

Agent system prompt 和 `tool_catalog` 都要说明 `ToolCatalogGroup` 的用途，但两处分别维护摘要文本。相同分组可能逐渐出现措辞、范围或 MCP/Skill 配置来源上的差异。

## 决定

- 在 `ToolCatalogGroup` 上提供短摘要，作为 Agent 能力索引与 `tool_catalog` family 描述共用的来源。
- 两个消费者只负责选择分组和呈现，不再各自维护分组文案。
- 操作 schema、工具加载流程、权限和执行行为继续由现有 owner 管理。

## 替代方案

- 在 Agent 与 `tool_catalog` 各自保留一份摘要：拒绝。它们表示同一分组，复制文案会让模型目录出现语义漂移。
- 从 operation 描述动态拼出 family 摘要：拒绝。operation 描述粒度不同，无法稳定概括整个分组。

## 影响与验证

只增加纯展示方法并调整两个调用点，不改变序列化、provider schema、IPC、配置或数据库，无需数据重置。验证包括 Common、Agent、Tools 的测试与严格 Clippy、相关 Rust 格式检查和 `git diff --check`。

## 回滚

恢复各 consumer 内部的分组摘要映射并移除此 ADR。无需数据重置。
