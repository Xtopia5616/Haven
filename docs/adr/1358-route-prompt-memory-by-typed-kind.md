# ADR 1358：按记忆类型分路进行 Prompt 召回

## 状态

已采纳 — 2026-10-21

## 背景

Profile、Semantic 与 Procedural 已是 facts 上独立的闭合类别，且 Memory recall 已实现类别过滤和一跳图扩展。Agent prompt 仍将所有 fact kinds 合成单个 keyword/vector 候选池：用户画像数量较多时，语义事实和低频流程容易在 SQL candidate limit 或最终 15 条上下文预算前被挤出。Prompt keyword 与 vector 还绕过统一的 `retrieve`，使它们没有共享图扩展路径。

## 决定

1. Prompt 按 `profile`、`semantic`、`procedural` 三路检索，每路候选上限分别为 8、8、4。三路复用同一 session query 和同一次 query embedding；每路使用对应 `MemoryRecallKind` 过滤。
2. 三路都调用 Memory 的 `MemoryRecallStore.retrieve`，统一执行 keyword/vector 合并、类别过滤、可见性校验和图邻居扩展。没有向量配置时使用同一 keyword + graph 降级路径。
3. 最终最多 15 条事实先按现有分数为每类各保留最多两个候选，再以原有分数从所有类别填满剩余空间。类别不足时剩余槽位由其它类别填充；原有事实总字符/token budget、敏感过滤和稳定排序不变。
4. Episodic recall 继续独立使用原 query、排除当前 session 并限制为 5 条。用户明确维护的事实仍可通过现有 top-40 seed 进入候选；类别路由补齐该 seed set 未覆盖的事实。
5. 不增加 LLM 意图分类器、数据库字段或 IPC shape；prompt memory cache key 仍绑定 normalized query、embedding model、memory revision 与 session exclusion。

## 考虑过的方案

- 所有 facts 共用一个 limit：拒绝。数据量较大的单一类别会在候选生成阶段占满预算，后续排名无法恢复已截断的类别。
- 使用 LLM 预测单一记忆类别：拒绝。多类事实可同时帮助一个任务，classifier 还会引入额外延迟、提示词版本、失败回退和类别漏判。
- 每类独立生成 query embedding：拒绝。query 文本相同，重复 provider 请求只增加成本和失败面，不增加信息。

## 后果

- Prompt 最多发起三路 fact recall，但只生成一次 embedding；每路 query/候选仍有硬上限，episode 与整体 context budget 不变。
- 类型槽位属于 prompt 层的召回分配规则，不改变持久化类别与用户手动 recall 行为。
- 无数据库、配置、IPC 或数据重置要求。

## 验收

- Profile 候选数量远多于其它类型时，Semantic 与 Procedural 仍可进入 prompt candidates；无某类型候选时其空槽由其它高分事实填充。
- Prompt fact recall 使用 unified retrieve，使由 query/vector seed 命中的图邻居可进入候选；类别间不串入其它类型。
- 同一 query embedding 被三路复用；没有向量时 keyword + graph fallback 正常。
- 最终 prompt 仍至多 15 条 facts、5 条 episode，并通过既有敏感过滤、session exclusion 与字符/token 上限。
- 相关 Agent/Memory 测试、workspace 编译与 strict Clippy 通过；未执行或失败门禁写入验证记录。

## 验证记录

- `cargo test --locked -p haven-agent`：675 passed，1 ignored。
- `cargo test --locked -p haven-memory`：480 passed，2 ignored。
- 两条新增回归测试分别验证了类别分路与图邻居候选、最终 Prompt 类型配额；均通过。
- `cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、ADR 索引检查与 `git diff --check` 均通过。
- 本切片未修改 UI、IPC 或 schema，因此未运行 UI 测试，也不需要数据库重置。
