# ADR 0022：Memory 事实维护持久化边界

## 背景

事实写入、查询和 embedding 编排已经分别收口，但 `repositories/facts.rs`
仍直接实现去重、敏感事实清理、衰减清理、来源引用清理以及矛盾扫描和降权。
这些操作同时涉及批量 SQL、缓存失效和维护候选的证据保留，继续堆在事实模型与
写入外观中会让维护规则和图谱写入规则再次耦合。

## 决定

- 新增内部 `repositories/fact_maintenance.rs`，由 `FactMaintenance` 统一拥有
  事实维护的数据库操作：谓词计数/重写、SPO 去重、敏感数据清理、有效置信度
  清理、来源引用规范化、矛盾扫描/降权和 LLM 仲裁候选读取。
- `repositories/facts.rs` 只保留事实类型、谓词/敏感数据策略、写入相关共享常量，
  以及既有 `Database` 维护方法的稳定转发外观；不新增第二套公开 API。
- Agent 继续拥有维护调度、LLM 请求、提案门禁和并发控制。Memory 只返回持久化
  数据或矛盾候选，不依赖 `haven-llm`，也不解释模型输出。
- 每个维护写路径继续在自身操作后使事实/embedding 缓存失效；不改变 X12 事件
  权威、事实表 schema、ID 格式、provenance 语义或维护顺序。

## 替代方案

- 继续扩展 `facts.rs`：会保留多个职责热点并使维护规则与读写实现漂移，拒绝。
- 把维护调度和 LLM 仲裁下沉到 `haven-memory`：会引入反向依赖并让持久化层承担
  业务推理，拒绝。
- 删除 `Database` 维护方法、要求所有调用方直接使用新模块：会扩大 API 变更面，
  且让 Agent/工具绕过统一外观，拒绝。

## 影响

这是 Memory crate 内部实现拆分。既有 `Database` 方法及 Agent 的矛盾候选类型
通过 re-export 保持不变；维护行为、缓存失效和数据库内容语义不变。没有用户可见
变化，不需要数据库、配置或缓存重置。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-memory
cargo test --locked -p haven-memory --lib -- --test-threads=1
cargo clippy --locked -p haven-memory --lib -- -D warnings
```

新增边界测试覆盖维护门面直接执行去重/标签合并、敏感内容删除和矛盾候选扫描；
既有 `Database` API 测试继续验证清理、衰减、来源和矛盾行为。

## 回滚与重置

代码回滚时删除 `fact_maintenance.rs`，将其实现恢复到 `facts.rs`，并恢复模块
登记与 ADR 入口。由于没有修改 schema、序列化、配置或持久化数据，不需要用户重置。
