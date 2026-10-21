# ADR 1347：合并文件检查与修改操作

## 状态

Accepted — 2026-10-21

## 背景

Files 工具分别提供 `files.inspect`、`files.stat`、`files.hash` 做元数据/哈希读取，以及 `files.edit`、`files.patch` 做精确文本替换。实现和模型目录因此保留了重复入口。

## 决定

- 删除 `files.stat` 与 `files.hash`，统一使用 `files.inspect`。该操作返回存在状态、类型、大小和修改时间；文件在读取上限内时同时返回编码和 SHA-256。超过上限时仍返回基本元数据，`hash` 与 `encoding` 为 `null`。
- 删除 `files.edit`，统一使用 `files.patch`。单处修改以一个 `edits` 元素提交，多处修改仍在同一原子写入中完成。
- 不保留旧 operation alias，也不迁移旧的 per-operation 设置或授权项。旧 `files.stat`、`files.hash`、`files.edit` 配置不再对应可执行入口；无须数据库重置。
- 同步更新 schema、提示、风险矩阵、安全策略和回归测试。历史 transcript 不改写。

## 替代方案

- 保留 `files.stat` 作为超大文件的元数据入口：拒绝。`files.inspect` 在无法安全计算哈希时改为返回元数据即可覆盖该用途。
- 保留 `files.edit` 作为单处修改的快捷入口：拒绝。单元素 patch 已表达相同操作，额外入口会重复 schema、权限和维护路径。
- 为旧 operation 名保留别名或自动迁移设置：拒绝。项目测试版本允许删除旧内部契约，旧配置项不再获得执行入口。

## 影响与验证

`files.inspect` 现在对超出哈希读取上限的文件成功返回基本元数据。`files.patch` 成为唯一的精确替换入口，风险仍为 Medium，并继续使用相同路径授权、大小上限、expected-hash 检查、dry-run 与原子替换语义。无数据库 schema 或事件格式变化。

验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo test --locked -p haven-tools`（913 个单元测试通过、2 个忽略；7 个 MCP 集成测试通过）。

## 回滚 / 重置

回滚代码即可恢复旧 operation。期间旧 operation 设置不会自动重新应用；需要恢复时由用户重新配置对应入口。数据库无需重置。
