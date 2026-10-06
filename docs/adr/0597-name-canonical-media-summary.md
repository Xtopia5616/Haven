# ADR 0597：命名 canonical media summary

## 状态

已采纳并实施。

## 背景

Agent 的 `canonical_media_summary` 为当前 canonical projection 计算两项关联元数据：按 image/audio/video 分类的 modality requirements，以及 raw media part 数。函数原返回 `(MediaRequirements, usize)`；`ReActState` 与 `RequestContext` 又分别以两个字段保存这对值，初始化、增量 append、替换和 capability 检查都必须维持隐含配对。

## 决定

1. 增加 `CanonicalMediaSummary { requirements, media_part_count }`，作为 summary helper 的返回类型。
2. `ReActState` 与 `RequestContext` 将相关双字段改为一个 summary 字段；media append 仍逐项累加 count 并 OR modality flags。
3. 保留 `media_requirements()` policy accessor；part count 通过 `media_summary()` 投影读取，删除不再有生产消费者的独立 count accessor。

## 替代方案

- 只给函数返回 tuple 增加注释：拒绝，state 与 request context 仍需要并列字段配对。
- 将 part count 合并进 `MediaRequirements`：拒绝，modality requirement 与数量属于不同数据角色，合并会让单独使用能力要求的消费者承担多余含义。

## 影响与验证

- 仅重组 Agent 进程内 canonical/request projection 的派生元数据，不改变 provider routing、MediaPlan、fallback、持久化或 IPC 行为。
- 命名路线图仍保持 Active，其他 Rust crate、UI、IPC/event 与配置持久名继续逐域审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`、`cargo test --locked -p haven-agent`、ADR 索引及 staged diff 检查。

## 回滚

恢复 tuple 返回和 `ReActState` / `RequestContext` 的双字段缓存；无持久化迁移。
