# ADR 0979：统一 ToolRun 持久化时间戳

## 状态

已采纳并实施。

## 背景

`ToolRunStore` 是 Memory 对 Tools 暴露的持久化边界，但其生命周期时间和定时 `due_at` 只按字符串写入。调用方传入的格式不统一会进入数据库；`due_at` 还按文本排序，不同 UTC offset 的 RFC3339 值可能违背实际时间顺序。进程重启恢复路径另用 SQLite `datetime('now')` 写 `finished_at`，与其它生命周期写入格式不同。数据库生成的 `created_at` 也采用 SQLite UTC 字符串，并会在缺少 `started_at` 时作为运行时间 fallback。

## 决定

1. `ToolRunStore` 对外写入的 `started_at`、`finished_at` 和定时 `due_at` 必须是 RFC3339；落库前统一转换为 UTC。completion payload 的可选 `started_at` 与必需 `finished_at`、quarantine 的 `finished_at` 执行同样校验。
2. 定时依赖触发以空 `due_at` 表示；Store 将仅包含空白的值收敛为空字符串，并要求此时存在 `watch_tool_run_id`。计时触发必须提供有效 `due_at` 且不能同时提供 watch ID。
3. ToolRun 的 typed read rows 与 completion payload 时间字段统一投影为 UTC RFC3339。读取仍兼容既有 SQLite `YYYY-MM-DD HH:MM:SS` UTC 时间；无效的 waiting `due_at` 保留原值，让 Tools 服务隔离并 quarantine，而不是令整批恢复失败。
4. `mark_interrupted_tool_runs` 用 RFC3339 UTC 写恢复失败终态。SQLite 内部 `created_at` 默认格式不变；Store 在读取时将该字段转换为 RFC3339，数据库列和 schema 不变。

## 替代方案

- 只要求输入看起来像 RFC3339、不转换 offset：拒绝；数据库文本排序不保证与时间顺序一致。
- 读取到任意坏时间都拒绝整批：拒绝；坏 waiting `due_at` 必须能到达现有 quarantine 路径。
- 修改数据库默认时间格式或引入 schema 迁移：拒绝；typed Store 可以在边界转换 legacy `created_at`，无需改变 schema。

## 影响与验证

- ToolRun lifecycle、completion 与查询投影使用同一 UTC RFC3339 形状；既有 SQLite UTC 时间在 read boundary 转换，坏 `due_at` 仍可被 quarantine。
- Memory 测试覆盖 UTC offset 归一、无效写入拒绝、watch trigger 空值、旧 SQLite 时间读取和重启终态时间。
- 不改 schema、数据库版本、ToolRun 身份、事件顺序或用户可见生命周期语义。

## 回滚

恢复 `ToolRunStore` 的原样时间字符串写入、SQLite 恢复时间表达式与旧测试 fixture，并同步回退命名规范、路线图和 ADR 索引；无需数据库迁移。
