# ADR 1325：统一领域动作动词与分阶段入口命名

## 状态

已接受

## 背景

命名规范已区分一部分查询、恢复、投影和持久化动作，但常用的持久写入、终态、输入入口和冲突维护动词仍有遗漏。代码中出现 `save_*` 与 `persist_*` 表达后端持久化、`complete_tool_step` 以布尔值包装 `finish_tool_step`，以及 `process_*` 横跨用户提交、Agent 准入和 Memory 事件消费的情况。`resolve_contradictions` 只会降级一部分确定的冲突项，名称容易让调用者误以为全部冲突已经消除。

## 决定

- 后端领域存储动作统一使用 `persist_*`；`commit_*` 专用于原子提交一组变更，`append_*` 专用于追加式日志，`write_*` 专用于底层字节或文件 I/O。`save` 表达用户可见的保存意图，不再替代后端持久化边界。
- 生命周期进入终态统一使用 `finish_*` 并传递具名 outcome；`complete_*` 只用于成功完成的动作。删除 `complete_tool_step(step_id, observation, success: bool)` 包装，统一使用 `finish_tool_step(step_id, observation, ToolStepOutcome)`。
- 输入链按 owner 和阶段命名：Tauri 用户提交为 `submit_transcript`，Agent 接纳并路由为 `ingest_user_input`，Memory 消费已提交事件为 `consume_session_event` / `consume_live_event`。提交返回类型统一命名为 `TranscriptSubmissionResult`，UI 投影为 `TranscriptSubmissionResultView`。
- 事实维护入口改为 `demote_obvious_contradictions`，准确描述确定性规则只降级明显冲突项、仍保留待裁决冲突组的行为。
- 从候选状态选动作使用 `select`，将宽松输入规整为封闭值使用 `normalize`；对包含成功或失败结果的终态流程使用 `finish`，不使用暗示成功的 `complete`。
- `docs/naming.md` 中的函数动词表是动作语义的唯一规范。不同 crate 可以重复使用同一个动词，但必须遵循相同可观察语义；调用阶段和副作用不同的入口则在名字中体现其 owner 与阶段。

## 考虑过的方案

- 保留 `save_*`、`persist_*` 和 `complete_tool_step` 并仅补充注释：拒绝。调用者仍需记忆别名及布尔参数的隐含状态映射。
- 把所有 `process_*` 机械替换成同一个动词：拒绝。IPC 提交、领域准入和已提交事件消费是不同阶段，统一成一个动词会隐藏责任边界。
- 将所有 `resolve_*` 改名：拒绝。解析候选目标、处理交互决议等现有动作仍可准确使用 `resolve`；只有部分降级冲突事实的入口需要按实际副作用命名。

## 影响

- Tauri command `process_transcript` 改名为 `submit_transcript`；UI command adapter、generated IPC contract、命令 owner 检查和当前 IPC 文档同步更新。此运行时命令不保留兼容别名，前后端随同一应用版本发布。
- `ProcessResult` 改名为 `TranscriptSubmissionResult`；只改变 Rust/IPC 类型名，不改变 enum variants、字段或 transcript 持久格式。
- Memory、Agent 和配置写入调用点统一迁移到 `persist_*`；工具步骤不再提供布尔值的 test-support 完成别名；失败也会到达终态的调用方法改为 `finish_*`。
- SQLite schema、durable session event payload、用户配置字段、ID 格式及持久化数据含义均未改变；无需数据库或配置重置。

## 验收

- Rust workspace locked tests、严格 Clippy 与格式检查通过。
- IPC contract generator 的 `--check` 和 IPC contract ownership/drift 检查通过。
- UI type check、单次测试和生产构建通过。
- 搜索确认当前代码、规范和 IPC 文档不再引用旧入口名；历史 ADR 中的名称作为当时决策记录保留。
