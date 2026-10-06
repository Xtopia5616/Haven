# ADR 0591：为 Memory episode search 结果命名字段

## 状态

已采纳并实施。

## 背景

Memory episode keyword search 合并 FTS 与短词 fallback 两条查询路径，二者都返回 `(entity_id, display_summary, search_haystack, created_at)`。后续计分阶段又以 `(matched_term_count, entity_id, display_summary, created_at)` 保存结果，并通过 `.0`、`.1`、`.3` 排序。字段的用途稳定，但它们的对应关系依赖 tuple 位置。

## 决定

1. 查询候选使用 `EpisodeSearchCandidate { entity_id, display_summary, search_haystack, created_at }`。
2. 计分结果使用 `ScoredEpisodeKeywordCandidate { matched_term_count, entity_id, display_summary, created_at }`。
3. 排序仍按匹配词数降序、创建时间降序、实体 ID 升序；FTS 与短词回退仍对相同 haystack 计分，最终 `EpisodeKeywordHit` shape 不变。

## 替代方案

- 只将 tuple 起一个 tuple alias：拒绝，调用点仍需按索引记字段语义。
- 用最终的 `EpisodeKeywordHit` 替代中间候选：拒绝，中间查询还需携带搜索 haystack 与创建时间，不能与用户可见 hit 合并。

## 影响与验证

- 改动限于 Memory 的私有查询/排序类型，不改变数据库 schema、SQL 筛选排序、公开 API 或召回结果。
- 更新命名路线图；无需数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-memory`、`cargo clippy --locked -p haven-memory -- -D warnings`、`cargo test --locked -p haven-memory`（401 passed / 2 ignored）、ADR 索引与 `git diff --check`。

## 回滚

恢复 `EpisodeSearchRow` 与计分 tuple 并还原按位置排序/解构；持久化数据不受影响。
