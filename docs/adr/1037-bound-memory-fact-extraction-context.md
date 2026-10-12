# 1037：限制事实抽取上下文并封闭 MemoryQuery 构造

## 状态

已采纳并实施（2026-10-12）。

## 背景

事实抽取会把已有可见事实带入 prompt。`max_known_facts` 只有 JavaScript 安全整数限制，配置值和 Memory store 的调用方都可以请求过大的结果。`MemoryQuery` 虽有校验构造器，但公开字段允许下游绕过文本和 limit 校验；超大 limit 还可能在 SQL 分页换算时溢出。

## 决定

- `MAX_FACTS_IN_EXTRACTION_PROMPT` 统一限制 prompt 中最多包含 64 条事实；默认仍为 40。TOML 超限值仅在加载快照中截顶并记录 warning，原文件不自动改写；Settings 写入超限值则拒绝。
- Memory store 对事实抽取查询独立执行同一上限，不能依赖 Agent 配置或调用方自律。
- `MemoryQuery` 字段保持私有，只通过校验后的构造器和只读 getter/builder 使用，调用方不能构造无效 limit。
- 桌面端 `list_facts` 继续提供完整浏览结果；它不是 prompt context，不套用该 prompt 上限。列表分页另待保持完整浏览/删除能力的 UI 设计，不静默截断。

## 影响

数据库 schema、持久记录和 IPC shape 不变，无需数据库重置或迁移。超限的旧配置不会被写回或丢弃其它值，但当前进程生效值最多为 64。事实抽取结果数有界；为按有效置信度排序，Memory 仍会检查可见候选事实，故本决定限制 prompt 输出量，不声称消除了全表扫描。

## 验证

- Common 测试覆盖 Settings 拒绝超限、TOML 加载截顶且保留原文件。
- Memory 测试覆盖调用方传入无界 limit 时仍最多返回 64 条。
- `cargo test --locked -p haven-common -p haven-memory`
- `cargo fmt --all -- --check`、IPC contract drift check、`git diff --check`

## 回滚

若 prompt 预算需要更大上限，应先更新同一 Common 常量及 UI/生成 contract，并以 prompt token 压力测试说明新值；不得单独放开 Memory store 或允许配置与运行策略分叉。完整 `list_facts` 能力不受回滚影响。
