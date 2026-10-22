# ADR 1363：以原文证据核验推断事实

## 状态

已采纳 — 2026-10-21

## 背景

ADR 1362 将 transcript 与 compaction summary 抽取的事实都送入待审核队列，避免抽取模型把
推测当成用户事实。但即使一条事实直接来自用户原话，全部要求人工确认仍增加摩擦；而抽取
模型给出的 `confidence` 也不能说明原话是否支持这个完整的 SPO 事实。

个人记忆中的私密经历与偏好不能靠公共网络验证。本项目需要验证的是“用户是否明确说过这条事实”，
并保留能让用户复核的原文证据；这不等价于验证公共世界中的客观真伪。

## 决定

1. Agent 对普通 transcript 候选执行单独的一次结构化 source-grounded 核验请求。它使用现有配置的
   fast-chat 模型，不是独立 provider，也不声称消除模型自检偏差。候选只携带其规范 `msg-*` 来源的
   原始用户文本；紧邻的 assistant 问句只能帮助理解简短回答，不能作为证据。summary
   候选没有原始证据，保持 `unverified` 并进入现有审核队列。
2. 核验输出限定为 `supported`、`uncertain`、`contradicted`，并要求 `supported` 返回不超过 120
   字符且在原始用户文本中逐字出现的连续引文。Agent 校验来源身份、引文子串、长度和敏感内容；Memory
   在写事务中再核对来源行是持久化的 user message 且原文包含引文。模型输出错误、冲突索引、缺证据、
   调用失败或隐私检查拒绝一律 fail closed 为 `unverified`。不额外做公开网搜索或外部事实查证；
   这仍是对用户表达的核验。
3. 只有原始用户消息明确支持完整 SPO、且通过引文与来源校验的候选才能在 extraction 事务内自动
   经 FactGraph 写入；其置信度最少设为 0.9。其余 verdict 留在有界审核队列并展示核验状态和来源。
   confidence 是记忆排序/更新使用的内部强度值，不是经过校准的统计概率；准确性仍可由用户修改或删除。
4. SQLite 仍由 Memory 持有 proposal 与 FactGraph 写入；事实、提案删除/新增和 extraction marker
   在同一事务内提交。schema 升至 v58，旧数据库不迁移，按发布重置说明重置。

## 考虑过的方案

- 对私人事实做联网搜索：拒绝。网页无法证明用户个人偏好/经历，且会把私密候选不必要地发送给外部。
- 继续让单次抽取模型自行给 confidence 并按分数入库：拒绝。分数不是来源核验，也不能可靠说明事实是否被原话蕴含。
- 自动接受所有模型标记为 supported 的候选：拒绝。持久层不信任模型标签；写入调用还必须携带规范来源和有界证据引文，Agent 与 Memory 分别校验证据关联。
- 对所有摘要/未核验候选提高分数或自动入库：拒绝。它们仍显示为待审核，未经核验的模型分数不触发接受。

## 影响与验证

提案记录核验状态与可选证据引文，Memory Center 展示状态；确定受原始用户话语支持的普通 transcript
事实现在可自动入库。误判风险由双阶段抽取/核验、原文精确引文、来源身份、敏感文本检查、事务原子性
和用户可修正性共同限制。验证结果：

- `cargo test --locked -p haven-memory`：494 passed、0 failed、2 ignored；新增 12 项提案/证据测试全部通过。
- `cargo test --locked -p haven-agent`：680 passed、0 failed、1 ignored；
  `cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check` 和 ADR 索引检查通过。
- IPC checker 通过，99 个 handler 的 Rust、生成 TypeScript、security metadata 和文档一致。
- `corepack pnpm run check`：0 errors、0 warnings；记忆 UI 定向测试 18 passed；生产构建成功。
- 全量 UI 测试为 1397 passed、4 failed；4 项仍是未修改的 SessionHeader（3 项）与 SessionToolbar（1 项）断言。
