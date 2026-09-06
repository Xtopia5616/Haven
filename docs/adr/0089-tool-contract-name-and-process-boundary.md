# ADR 0089：内置工具契约名称与进程启动边界

## 背景

静态审查发现三类契约漂移：运行时定时任务工具名已经是 `schedule`，安全矩阵和权限路由仍使用
`scheduled_action`；文件工具运行时名称是 `files`，UI 与测试仍保留 `file`、`file_search`、`search`
分支；`audio` 的参数类型包含未接入 TTS 的 `text`，而实际 schema 不接受该字段。与此同时，
`process.launch` 以 fire-and-forget 方式启动进程，与能返回 `action_id`、受统一生命周期管理的
`shell.background` 重复。

## 决定

1. `schedule` 是唯一模型工具名和权限 key 根；`scheduled_action` 继续只作为 Rust 模块、内部事件或
   领域术语使用。
2. `files` 是唯一文件聚合工具名。删除 UI 中旧的 `file`、`file_search`、`search` renderer/label
   分支；搜索结果仍由 `files` 根据结果 shape 选择搜索 renderer。
3. 删除 `audio` 的 `text` 参数和不可用 TTS 路径。若未来需要主动播报，另行设计独立的 `speak` 工具。
4. `audio.play.file_path` 纳入统一 `allowed_paths` 收集器和负向回归测试。
5. 删除 `process.launch`、相关 schema、风险矩阵和测试；进程启动统一使用 `shell` 的
   `background: true`，通过 `actions` 管理生命周期。
6. 配置加载时一次性把 `[tool_settings.file]` 转为 `[tool_settings.files]`，把
   `scheduled_action` 权限 key 前缀转为 `schedule`。如果新旧名称同时存在，新名称优先；运行时和
   后续保存不再暴露旧名称。数据库中旧定时任务的 `tool_name` 不做隐式改写，避免改变已持久化调用语义。

## 替代方案

- 保留旧名作为永久兼容别名：拒绝，会使工具注册、权限、恢复和 UI 再次出现双真源。
- 在 `audio` 中保留 `text` 并返回运行时错误：拒绝，schema 与实现继续漂移。
- 保留 `process.launch`：拒绝，脱离 `actions` 的进程无法获得统一结果、取消和恢复语义。
- 立即拆分 `system`、`audio`、`files` 为更多顶层工具：本 ADR 不处理，待独立边界设计和调用数据验证后再做。

## 影响与验证

- 这是模型工具名、权限 key、UI renderer 和 `process` schema 的破坏性内部契约变更；旧模型调用应重新生成。
- 旧配置别名无需手工迁移；加载时会被规范化，保存后只写正式名称。旧数据库定时任务引用删除工具名时需取消重建或重置数据。
- 必须通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-common`、
  `cargo test --locked -p haven-tools`、`cargo check --locked -p haven-agent -p haven-app-binary`、
  `cd ui; corepack pnpm run check; corepack pnpm run test:run`。

## 回滚

回退本 ADR 对应提交即可恢复旧工具契约。配置迁移是单向的；回滚到旧版本前应使用升级前的完整配置备份，
否则旧版本可能无法识别已经写出的 `files` 或 `schedule` 名称。
