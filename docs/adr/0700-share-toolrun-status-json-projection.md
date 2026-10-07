# ADR 0700：共用 ToolRun 状态 JSON 基础投影

## 状态

已采纳并实施。

## 背景

Tools 的 `ToolRunStatusView::to_json` 与定时结果 completion notification 的 `render_status_json` 分别把 `ToolRunState` 映射到 JSON。两处重复构造 waiting、running、completed、failed、cancelled 的基础字段；完成状态的 `output`、时间、exit code、truncation 与 log path 尤其容易在查询和结果交付间漂移。

两条路径的外层契约仍不同：background status 可以带 `background_wait`、`kind` 和 `source_step_id`；scheduled status 还带 schedule metadata；completion notification 按自己的 delivery envelope 关联 session 和 result ID。

## 决定

- `ToolRunStateView::from_runtime_state` 是运行态到共享状态投影的唯一映射；`from_entry` 只补充 running background ToolRun 的 command、shell 和 live output。
- `ToolRunStateView::status_json` 是 waiting/running/terminal 状态基础字段的唯一 JSON 构造入口。
- Background 查询在基础 JSON 之外按需添加 `background_wait`、kind 与 source step；completion notification 使用同一基础 JSON，scheduled status 保留其独立 schedule projection。
- 不把不同调用者的 envelope 合并成一个通用 DTO；它们的消费者、附加字段和交付时机不同。

## 替代方案

- 保留两份状态 match 并靠测试比较：拒绝，测试只能提示漂移，不能消除重复 owner。
- 把 background wait、scheduled metadata 和 completion delivery 都放入同一状态 JSON：拒绝，这会混合各调用者的职责与生命周期。

## 影响与验证

状态 JSON 的字段、条件字段、background wait 文案和通知时序保持不变；无 IPC、数据库、配置或用户数据格式变化，无需重置。验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`（800 passed，2 ignored；7 个 MCP integration passed）、`cargo clippy --locked -p haven-tools -- -D warnings`、ADR 索引检查与 `git diff --check`。

## 回滚

恢复各调用者原有的状态 match，移除 `from_runtime_state` 与共享 `status_json`，并同步撤回本 ADR、路线图及命名规范索引；无数据迁移。
