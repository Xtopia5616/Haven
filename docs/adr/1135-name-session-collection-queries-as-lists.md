# ADR 1135：将 Session 集合查询统一命名为 list

## 背景

`docs/naming.md` 规定返回集合的读取操作使用 `list_*`。Memory 的旧 session 清理查询和分页查询仍使用 `old_session_ids`、`session_ids_page`；Agent 消费方因此无法仅从方法名看出这些方法返回集合。

## 决定

- 将 Memory 的 `old_session_ids`、`session_ids_page` 和 `session_ids_page_cancellable` 分别改名为 `list_old_session_ids`、`list_session_ids_page` 和 `list_session_ids_page_cancellable`。
- 同步 SessionStore、Agent 调用方与测试名称；查询过滤、排序、分页和取消行为保持不变。
- Memory session 实体操作的参数统一命名为 `session_id`，包括查询、状态/运行结束原因/标题更新和删除；参数与 SQL、错误上下文中的实体含义保持一致。
- 不保留旧别名，避免集合读取 API 同时出现两套命名。

## 替代方案

- 保留旧名作为兼容包装：拒绝。所有消费者都在同一 workspace 内，可以一次性更新；别名会延长已知命名偏差。
- 将所有 session ID 读取收敛为一个分页方法：拒绝。旧会话筛选、高水位标量和分页集合有不同的查询语义，应只统一集合操作前缀，不合并接口。

## 影响与验证

这是 Memory 到 Agent 的 Rust API 命名变更，不改变返回类型、查询结果、持久化字段、IPC 或数据库 schema，不需要数据迁移或重置。验证全 workspace 格式、测试与严格 Clippy，并确认旧方法名无剩余引用。

## 回滚

可将定义和所有消费者恢复为原方法名；不涉及持久数据或运行时状态重置。
