# ADR 1345：按需搜索直接关联会话的 transcript

## 背景

ADR 1342 为压缩后的当前会话提供原始 transcript 搜索，ADR 1343/1344 为当前会话提供关联资产搜索。显式关联的其他会话也可能包含本轮需要的用户输入、工具调用或结果；把这些内容常驻注入 prompt 会增加上下文成本并混淆来源。

## 决定

- 新增只读、幂等的 `session.linked_history_search` 与 `session.linked_history_read`。搜索当前会话所有仍存在的直接关联 session；关系是对称的，因此包含从任一端建立的 link。搜索不包含当前 session 本身，也不递归遍历二度或更远关系。
- 搜索结果包含来源 `session_id`、本地 `sequence` 和有界匹配片段。全局分页 cursor 由 `created_at`、`session_id`、`sequence` 组成，匹配按这三项稳定排序。
- Read 操作要求来源 session 仍与当前 session 直接关联，并在同一读事务中检查关联和读取事件，防止该能力成为任意跨 session 的 transcript 读取入口。
- 每次请求最多检查 32 个直接关联 session；全部 session 共享 50,000 个事件、64 MiB 扫描字节和 5 条返回结果上限。超限时明确失败；取消沿用 blocking SQLite 操作的取消边界。
- 延续当前 history 搜索的 lineage rollback 过滤、compaction root 与 provider reasoning 字段排除、结果截断及低信任处理。关联历史不自动进入 prompt，模型仅在需要时调用搜索与读取。

## 替代方案

- 将每个关联 session 的历史直接拼入当前 prompt：增加常驻 token 与不相关上下文，拒绝。
- 递归搜索所有传递关系：会扩大隐私与扫描边界，且每层关系不能证明与当前任务直接相关，拒绝。
- 允许读取任意指定 session：会绕过用户建立直接关联的边界，拒绝。
- 分别调用单 session 搜索而不设置总扫描上限：关联数乘以每 session 预算会造成无界放大，拒绝。

## 影响

Memory 增加关联历史查询 DTO、分页 cursor 和有界跨 session 查询能力；Tools 增加搜索与受关联校验的读取 operation；Agent 工具提示说明按需检索方式。持久事件与 `session_links` schema 不变，无数据迁移或重置要求。

## 验证

- Memory 测试覆盖对称直接关联、来源 ID、稳定分页、回滚 lineage 过滤、非传递搜索、读取 link 校验及 32-session 上限。
- Tools 测试覆盖 operation 输入输出、来源 ID 和跨 session 读取约束；工具目录、授权元数据和 Agent prompt 同步登记。
- 按跨 crate 变更门禁运行 Rust workspace 格式检查、locked 编译、严格 Clippy 与全量测试。

## 回滚 / 重置

回滚代码提交即可移除新增查询入口；事件、session links 与数据库 schema 不变，无需迁移或重置数据。
