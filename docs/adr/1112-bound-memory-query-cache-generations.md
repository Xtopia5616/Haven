# ADR 1112：限制 Memory 查询缓存 generation 并原子失效

## 状态

已接受（2026-10-13）

## 背景

Memory 查询结果 entry 有 LRU 上限，但 `key_generations` 为每个失效 key 永久保留 generation；即便对应 entry 不存在或已被淘汰，长期运行进程访问大量不同 session/fact subject 后仍会持续占用内存。facts mutation 还分两次获取缓存锁，先失效 subject 结果，再失效 `list_facts` 全量结果，两次之间允许读到旧的全量列表。此前 subject key `_facts_{subject}` 与全量 key `_facts_all` 也在 subject 为 `all` 时发生身份冲突。

## 决定

- generation tombstone 上限为 query entry 上限的四倍。新增失效 key 将越过上限时，在同一个锁内推进所有 query domain generation、清空 entry 与 tombstone，再记录本次失效。旧的在途查询因此无法在 reset 后写回；容量触顶时允许发生一次 cache cold flush。
- facts subject 与全量列表使用不同的内部 key namespace：`facts:subject:{subject}` 与 `facts:all`。这些 key 仅供进程内缓存使用，不影响数据库或 IPC 契约。
- 一次事实 mutation 在一个缓存锁临界区内同时递增 subject 与全量结果的 generation 并清空对应 entry；过期查询写回仍由 generation 检查拒绝。

## 替代方案

直接删除 LRU entry 对应的 generation 不安全：该 key 仍可能有尚未结束的查询，删除 tombstone 后旧查询会获得与新查询相同的默认 generation。只限制缓存 entry 数量也不能限制失效过但未缓存的 key。为每种缓存写入单独的锁或异步锁会增加互斥状态，且不能替代同一失效边界。

## 影响

正常命中路径和 facts 查询结果保持不变。超过 generation 上限时会清空全部进程内查询缓存，造成短暂冷缓存；数据库是唯一事实来源，写入结果仍受 generation 检查保护。subject 名称 `all` 不再与全量 facts 查询共享缓存身份。

## 验证

- `cargo test --locked -p haven-memory cache::tests`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- UI 检查、IPC contract 与 ADR index 检查。

## 回滚

不得移除 generation tombstone 上限或恢复可丢弃的单 key tombstone。若调整容量策略，必须保持全局 cache epoch reset 对所有在途 query 的失效保证，并保持 subject 与全量结果 key 分离。
