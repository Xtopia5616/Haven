# ADR 0019：Memory 事实图谱写入边界

## 背景

`repositories/facts.rs` 同时包含事实插入、用户事实权威规则、推断事实
upsert、查询排序和搜索。随着记忆图谱规则增加，新的写路径容易绕过节点
关联、来源 provenance、缓存失效或单值/极性冲突规则。

## 决定

1. 新增 `repositories/fact_graph.rs`，由内部 `FactGraph` 门面集中实现
   `memory_edges` 的事实写入、节点关联、来源映射、用户覆盖、upsert 和删除。
2. `Database` 的既有事实写入方法保留为稳定外观，但只转发到 `FactGraph`；
   调用方不需要改变，且不新增第二套写入 API。
3. `facts.rs` 暂时继续拥有事实读取、搜索/排序和维护任务；后续切片分别
   抽取查询排序与维护策略，不在本次混合推进。
4. `FactGraph` 不改变表结构、ID 格式、缓存失效语义或
   `ReActSnapshot.events` 的 X12 权威关系。

## 替代方案

- 继续向 `facts.rs` 增加写入分支：会扩大热点文件并允许规则漂移，拒绝。
- 让每个调用方直接写 `memory_edges`：会绕过节点、provenance 与缓存不变量，拒绝。
- 立即把整个 facts repository 拆成多个公开 trait：会扩大内部 API 和生命周期
  传播面，本切片只建立内部稳定边界，暂不采用。

## 影响

这是内部模块拆分，对外 `Database` API、数据库 schema、快照和 IPC 均不变，
不需要用户重置。新的图谱事实写入必须通过 `FactGraph` 的实现路径；事实
查询仍由 `facts.rs` 负责。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-memory
cargo test --locked -p haven-memory --lib -- --test-threads=1
cargo clippy --locked -p haven-memory --lib -- -D warnings
```

新增测试覆盖事实写入同时建立 subject/object 节点，以及用户单值事实不能
被推断值覆盖；既有 facts repository 测试继续覆盖完整行为。

## 回滚与重置

代码回滚时删除 `repositories/fact_graph.rs`，并恢复 `facts.rs` 中的写入
实现即可。由于本次不修改 schema、数据内容或序列化格式，不需要数据库、
配置或缓存重置。
