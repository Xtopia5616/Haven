# ADR 1051：共享媒体资产身份校验

## 状态

已接受（2026-10-12）

## 背景

Agent 在 durable transcript 中校验 `MediaInput` 的资产身份，但把规则实现为本地循环。Memory 的消息持久化会把 `MessageAttachment` 转为 `MediaInput` 并保存，却没有校验顶层资产 ID 或 `ManagedFileRef` 中的资产 ID；因此公开的 SessionStore ingress API 可以把格式错误的引用写入消息与媒体投影。

## 决定

- 将 `MediaInput::validate_asset_identities` 放在 Common，由一个实现校验 `asset.asset_id` 和每个 `ManagedFileRef.asset_id`。
- Agent transcript 校验和 Memory 消息写入口共用该方法。
- Memory 在打开 SQLite 写事务前校验媒体输入；无效身份不得写入消息、媒体投影或 ingress cursor。

## 影响与兼容

通过 `new_id("asset")` 生成的身份不变；缺省附件资产身份仍由转换函数生成。只拒绝格式错误的 Haven 自有资产身份。provider 值、文本表示、合法引用、数据库 schema 与 IPC shape 不变，无迁移或重置要求。

## 验证

- Common 单测覆盖顶层资产 ID 与嵌套 `ManagedFileRef` ID。
- Memory SessionStore 测试证明两类格式错误都在落库前拒绝，且消息行与 ingress cursor 均未变化。
- Agent、Memory、Common crate 测试及全工作区格式、check、clippy 门禁。
