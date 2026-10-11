# 0961：校验 Memory fact extraction 的持久身份

## 状态

已接受并实现（2026-10-11）。

## 背景

Memory fact extraction 使用 `kv_store` 保存 session cursor、pending marker、attempt timestamp、summary 状态和 session event cursor，并将 compaction summary 写入 `memory_items`。这些入口此前只拒绝空字符串或直接拼接 key；错误前缀的 session、message 或 episode ID 仍可能进入持久层。读取普通 extraction cursor 时也没有验证其中的 message identity。

项目 ID 规范已规定 session 使用 `ses-*`，message 与 memory item（包括 summary episode）共用 `msg-*`。持久化 owner 应在构造 KV key、写行和消费 cursor 时守住该约束。

## 决定

- Memory extraction 的 session 边界统一校验 `ses-*`；普通 transcript cursor 校验 `msg-*`。
- Summary episode 写入及 done/pending marker 使用 `ses-*` 与 `msg-*` 的组合；无效身份在写行或 marker 前拒绝。
- Shared attempt timestamp、ordinary cursor、transcript load、fact extraction pending marker 与 memory event cursor 通过 owner 内的校验 helper 构造 key。
- 读取持久化 ordinary cursor 时校验 cursor value 为 `msg-*`；畸形值报错，不转换、跳过或猜测身份。
- 保持既有 KV key 名称、表结构、生成策略和合法数据行为不变。拒绝仅针对违反既有 canonical ID 契约的输入或持久值。

## 替代方案

- 仅校验非空：无法阻止不同实体 ID 混用，KV key 也会保存错误身份。
- 为旧格式生成 fallback ID：会破坏 cursor 与 transcript/episode 的实体关联，并引入第二身份。
- 读取时跳过畸形 cursor：会让 extraction 静默重复或遗漏，无法给出可诊断的失败。

## 影响

合法生产路径继续使用原有 `new_id` 结果和 KV key。数据库 schema 与 IPC contract 均未改变，不需要数据库重置。非规范的已有 cursor 会在读取时显式报错；它不属于受支持的持久数据形状，应由上层错误处理暴露并按既有 release/reset 流程恢复。

## 验证

- 新增写入与读取拒绝用例，覆盖 session/message/episode 前缀和失败时无部分写入。
- `cargo fmt --all`
- `cargo test --locked -p haven-memory`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

若发现合法 production ID 使用了不同前缀，应先校对其实体类型与 owner，再修正调用方或本 ADR 的校验位置；不恢复空字符串检查或静默接受任意持久身份。
