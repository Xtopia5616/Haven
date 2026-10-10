# 0915：隐藏 SessionCommitted 的存储投影集合

## 状态

已接受并实现（2026-10-11）。

## 背景

`SessionCommitted` 是 Agent 向 Memory `SessionStore` 提交 transcript 的 typed command。Agent 生产路径已经通过 `transcript`、`push_transcript` 和 `project_*` builder 表达事件与物化语义；但 `events`、`projections`、`trailing_events` 字段仍公开可变，且 `SessionProjectionIntent` 与 `SessionCommittedEvent` 从 Memory 根导出。调用方可以绕过 builder 直接拼接内部 projection DTO，绕开 owner 对批次、事件顺序和投影意图的封装。

当前唯一的集合直接访问位于 Agent 的 ReAct 批处理与 UI payload 组装中；前者只是合并已构造的提交，后者只为附带 UI 重建元数据而修改首个 event payload。二者都可以由窄方法或先完成 payload 编码来表达。`SessionCommittedEvent` 与 `SessionProjectionIntent` 没有其他跨 crate 类型消费者。

## 决定

- 将 `SessionCommitted` 三个集合字段设为私有；保留公开的 `transcript`、`push_transcript`、trailing domain event 与 typed projection builder。
- 增加 `append_commit`，在批量提交中按顺序合并事件、投影和 trailing events，避免 Agent 直接访问集合。所有向量分别保持原有插入顺序。
- 让 ReAct 在创建提交对象前完成 UI envelope 编码；`build_session_commit` 变为同步、无失败分支的 builder，不再修改 `SessionCommitted.events`。
- 将 `SessionCommittedEvent` 与 `SessionProjectionIntent` 限为 Memory 内部实现，移除其根导出。`SessionCommitted` 不再因内部字节上限计算而实现公开 `Serialize`；Memory 通过私有同形 view 保留相同字段顺序和精确 JSON 字节计数。
- Memory 仍不依赖 Agent/ReAct 类型。Agent 显式声明需要持久化的 typed domain projection，由 Memory 在事务内解释并写入 SQLite；此次只封装集合及 DTO，不改变上述反向依赖方向。

## 影响

这是 Rust source API 收窄；仓库消费者迁移到 builder 与 `append_commit`，不保留兼容字段或类型导出。transcript payload 的 JSON 内容、event append 顺序、projection 插入顺序、事务回滚、提交后广播与 UI 发布保持不变。`append_commit` 同时合并 trailing events，避免组合提交时遗漏该类 staged 操作。没有 schema、持久格式、IPC、用户可见行为或授权变化，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

新增 Memory 单元测试覆盖 `append_commit` 对事件、投影和 trailing event 顺序的保留；Agent/Memory 全工作区测试覆盖原有 transaction、replay 与发布行为。

## 替代方案

- 保留 public vector 字段并要求调用方遵守 builder：拒绝；封装没有收益时约束会继续依靠约定，调用方仍能绕过投影语义。
- 让 Memory 解析 Agent 的 ReAct transcript 以自行推断消息/步骤行：拒绝；会引入 Memory→Agent 反向依赖，并把版本化 transcript 解释复制进持久层。Agent 保持领域翻译职责，Memory 保持事务和 SQL 投影 owner。
- 单独增加 setter 暴露 event index 与 payload 改写：拒绝；在 Agent 构造提交前完成 payload 编码更直接，不需要存储 DTO 的可变入口。

## 回滚

恢复公开字段、导出 DTO 与 Agent 对集合的访问即可；无需修改或恢复数据。
