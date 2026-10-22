# ADR 1365：为事实记录现实有效时间

## 状态

已采纳 — 2026-10-21

## 背景

ADR 1361 增加的 `fact_history` 记录 Haven 何时保存或改写了某条事实，回答“系统在某个时点知道什么”。这不表示事实在现实世界何时开始或停止成立。将 `created_at` 用作现实起点会把首次观察误当作事实生效时间；模型抽取同样不能从消息时间自行补出有效边界。

## 决定

1. 当前 `facts`、`fact_history` 和 `fact_proposals` 都保存可选 `valid_from` 与 `valid_to`。边界规范为毫秒精度 UTC RFC3339；用户和模型输入可用 RFC3339 或 `YYYY-MM-DD`，日期解释为 UTC 午夜。若两端均存在，开始必须早于结束。
2. 有效区间采用左闭右开 `[valid_from, valid_to)`。缺少一端表示该端未知/开放；两端均缺失表示有效时间未知。未知边界不从 `created_at`、`last_seen_at`、消息时间或核验结果推断。
3. `recorded_at` 是独立的 system-time 起点。更新或删除前的历史快照保留旧 `recorded_at` 与旧有效区间；语义或有效时间改写更新当前行的 `recorded_at`。每个 fact ID 的记录时间按毫秒严格递增，保证紧邻写入仍能表示非空系统历史窗口。
4. 默认 search/list/recall/Prompt/FTS/embedding 继续只读当前 facts，不因有效日期过滤。MemoryTool `history` 查询 system time；新增 `valid_at` 按指定现实时间查询每个 fact ID 最新已知版本的有效区间。valid-time 查询省略两端都未知的行，并保留 Memory 的可见性过滤、subject 限定和最多 50 条结果上限。
5. 单值用户事实以带明确 `valid_from` 的新值替换已知且开放的旧区间时，先把旧 `valid_to` 收束到新开始时间，再由既有历史触发器保存更正前和收束后的版本。旧起点未知或新起点缺失时不制造时间边界。用户明确 `forget`、按 ID 删除、清空或敏感事实清理继续物理清除对应事实历史和提案。
6. Agent 抽取仅在用户文本明确给出时输出有效日期；消息时间不作为后备值。事实证据 verifier 也必须确认候选日期边界受到同一用户原文支持。日期格式错误、区间倒置或时间边界未被核验的候选不得作为已核验事实自动接受；它们进入审核队列或被丢弃。来源核验判断“用户是否这样说”，不等同于外部事实查证，也不把置信度解释为校准概率。
7. Memory Center 手动录入可选起止日期，并在已保存事实及待审提案详情中显示区间。Schema 升至 v59；不做运行时迁移，v58 及更早数据库按发布重置流程备份并重建。

## 考虑过的方案

- 将 `created_at` 当作 `valid_from`：拒绝。创建时间只表示 Haven 持久化事实的时间，未必是其现实起点。
- 两端都缺失时把事实视为无时间限制：拒绝。没有证据的事实不能在历史时点查询中被默认为一直成立。
- 把 `history` 改为接收现实日期：拒绝。knowledge time 与 valid time 是两个独立问题，需要分别命名和查询。
- 让有效日期改变默认 recall：拒绝。本切片仅提供显式 `valid_at`；当前事实召回语义与排序保持稳定。
- 用户忘记后保留历史区间：拒绝。沿用 ADR 1361 的隐私契约，显式删除必须擦除事实及历史副本。

## 后果

- 对用户明确给出的工作、居住、项目状态等变更，可以查询其现实生效区间；没有时间证据的事实仍可正常用于当前召回，但不出现在 `valid_at` 快照。
- 历史查询的精确度依赖来源明确程度。verifier 只能验证表达证据，不能独立验证外部世界是否真实。
- 新增字段跨 Memory、Agent、Tools、App IPC 与 UI 贯通；旧数据库必须重置。
- `valid_at` 返回的是每个事实 ID 的最新已知版本，不是任意历史 knowledge-time 快照。若需要重建“当时记录的现实状态”，应先按 system time 选版本，再按其 valid interval 过滤。

## 验收

- 验证 date-only 与 offset 时间归一化、坏日期和反向区间拒绝、半开边界以及未知时间过滤。
- 验证单值更正收束旧区间，`history` 与 `valid_at` 语义独立，显式 forget/按 ID 删除/clear 清除时间历史。
- 验证普通抽取只保留明确日期，精确原文 verifier 针对事实及日期边界 fail closed。
- 验证 MemoryTool 的 `valid_at` schema、日期记忆写入、App IPC DTO、Memory Center 表单与详情。
- 验证 schema v59 reset、Memory/Agent/Tools/App/UI 测试、workspace strict Clippy、格式、Svelte、IPC 与 ADR 索引；记录不能通过的既有环境门禁。

## 验证记录

- Memory：`cargo test --locked -p haven-memory --quiet` 通过，501 passed、2 ignored；`cargo clippy --workspace --locked -- -D warnings` 与 `cargo fmt --all -- --check` 通过。
- Agent：`cargo test --locked -p haven-agent --quiet` 通过，680 passed、1 ignored。
- Tools：`cargo test --locked -p haven-tools --quiet` 通过，929 passed、2 ignored；MCP integration 7 passed。
- App：`cargo test --locked -p haven-app-binary commands::memory --quiet` 通过，6 passed；workspace App binary suite 267 passed。
- UI：`corepack pnpm run check` 通过，0 errors / 0 warnings；记忆相关 4 个测试文件 130 passed。完整 UI 测试为 1398 passed、4 failed；失败集中在既有 `SessionHeader` / `SessionToolbar` 样式类与工作区 accessible name 断言。
- IPC：`scripts/check-ipc-contracts.ps1` 验证 99 个 handler 的 Rust、生成 TypeScript、安全元数据、文档和 UI disposition 一致。ADR 索引覆盖 1343 条记录且本地链接有效。
