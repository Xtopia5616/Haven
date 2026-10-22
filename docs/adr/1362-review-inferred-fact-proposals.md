# ADR 1362：推断事实先审核再写入

> 普通 transcript 中经原文证据核验的直接事实已由 [ADR 1363](1363-source-grounded-fact-validation.md) 改为可自动写入；summary 与未核验事实仍按本 ADR 进入审核队列。

## 状态

已采纳 — 2026-10-21

## 背景

后台 MemoryWorker 会把模型输出的事实直接写进长期 `facts`，并按既有规则更正单值谓词。
解析和安全过滤可以拦截格式错误与凭据样式内容，却不能确认模型是否把推测、过期说法或
上下文误读成用户事实。ADR 1358 已区分 `profile`、`semantic` 与 `procedural`；ADR 1361
提供了更正前历史，但两者都不能代替写入前的人为确认。

## 决定

1. 普通 transcript 与 compaction summary 的 LLM 抽取都生成待审核提案，不直接改变 `facts`。
   用户显式 `remember`、UI 手动添加仍走现有直写路径；模型推断不自动接受。
2. Memory 持有持久化提案 owner。提案保留经过现有 sanitize/敏感值过滤后的 SPO、置信度、
   标签、durability 和可用的来源片段。默认 search/list/recall/Prompt/FTS/embedding 只读
   已接受 `facts`，提案只显示在专用审核队列。
3. 提案使用 `fprop-*` 身份。普通 extraction cursor、summary completion marker 与对应提案
   写入在同一 SQLite 事务提交；若保存失败，cursor/marker 不前进。相同待审核 SPO 只保留
   一条；待审核最多 500 条，容量耗尽时整批拒绝并保留待处理 cursor，不静默丢弃或自动接受。
   UI 列表以最多 50 条的 keyset page 读取。
4. Accept 和 reject 都是按 `fprop-*` 执行的 compare-and-set 一次性操作。接受在同一事务中
   复用 FactGraph 的 inferred upsert：历史快照、单值更正、已有事实 reinforcement 和
   user-source 权威规则保持原 owner。提案在该事务写入完成后移除；拒绝只移除提案，不改写
   现有事实。重复或过期审核请求不产生第二次事实写入。
5. Memory Center 增加待审核区，展示来源与置信度，并提供显式接受/拒绝操作；事实列表仍只
   展示已保存 facts。Tauri 使用 typed command DTO；不加入新的事件通道或 UI 侧事实真源。
6. Schema 升至 v57；旧数据库不迁移，发布前备份并按重置流程删除数据库。已处理提案随
   接受/拒绝立即删除，因此审核表只保存有界 pending 工作。

## 考虑过的方案

- 继续自动写入，只给 fact 添加 `confidence` 或来源徽标：拒绝；分数不能表达用户是否认可，
  也不能阻止推断更正单值事实。
- 按置信度自动接受一部分：拒绝；不引入未经用户指定的置信度阈值作为批准代理。
- 仅在 Prompt 中要求模型谨慎：拒绝；模型输出仍不是持久化写权限。
- 在现有 `facts` 行上增加 `pending` 状态：拒绝；这会让未审核内容进入事实列表、索引和已有
  召回路径，要求每个读者正确过滤；独立 proposal 表维持当前 facts 的单一语义。

## 后果

- 新抽取出的推断不会进入 Prompt 或普通召回，直到用户接受；需要人工审核的事实可先在
  Memory Center 查看来源，再决定是否写入。
- extraction cursor 与提案批次共同提交，崩溃后重放不会跳过提案或重复批准。队列满会让
  extraction 保持 retryable，用户审核释放容量后再重试。
- 用户接受错误提案后，可通过现有事实删除/修正和 ADR 1361 历史查询恢复审查上下文；拒绝
  提案不会删除此前已接受的同类 facts。
- 无独立终态审计表；接受结果由正式 facts 与 fact history 表达，拒绝的候选和来源片段随拒绝
  一起删除，减少个人对话数据的留存。

## 验收

- ordinary 与 summary 抽取只写 pending proposals；重复 cursor、marker 和批次不生成重复提案。
- proposal 写入失败时 cursor/summary marker 回滚；队列达到 500 项时不前进 cursor 且不部分写入。
- 接受仅一次地调用现有 inferred upsert；接受/删除与 fact-history trigger 同事务，拒绝不触碰
  facts；冲突 user fact 继续权威。
- 普通搜索、列表、召回和 Prompt 不返回 proposal；Sensitive 输出不会进入提案或 UI。
- UI 分页有上限并可接受、拒绝、刷新；过期/重复命令安全；命令 DTO、IPC 检查及 UI 测试通过。
- schema reset、Memory/Agent/App/Tools 影响面门禁、workspace Clippy、UI 检查/测试/构建与
  ADR 索引通过；未通过门禁记录准确结果。

## 验证记录

- cargo test --locked -p haven-memory：489 passed，2 ignored；覆盖 proposal schema、存储、分页、事务、accept/reject 与事实历史联动。
- cargo test --locked -p haven-agent memory_worker::tests：71 passed；抽取路径只生成 proposals，cursor/summary marker 与失败重试行为通过。
- 记忆/UI 定向测试：23 passed；包含 PendingFactProposals、MemoryView、memory command 与 DTO 校验。
- corepack pnpm run check：0 errors，0 warnings；cargo fmt --all -- --check、cargo clippy --workspace --locked -- -D warnings、ADR index 检查均通过。
- IPC contract 检查通过：99 个 handler 与生成 TS、metadata 和文档匹配。
- corepack pnpm run test:run：1397 passed、4 failed（1401 total）。失败均在未修改的 SessionHeader.test.ts（3 项 md-btn 样式/工作区 combobox accessible name 断言）及 SessionToolbar.test.ts（1 项 md-btn 样式断言）；本切片新增和相关记忆 UI 测试通过。
- 风险/限制：workspace 与全量 UI 套件仍包含上述不相关失败；strict Clippy、格式、Svelte check、IPC 与新增功能定向测试均通过。
