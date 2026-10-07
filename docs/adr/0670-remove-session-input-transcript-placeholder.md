# ADR 0670：移除 Session 输入 metadata 的 transcript placeholder

## 背景

ADR 0199 已定义 `sessions.input_text` 只保留初始输入/列表摘要用途；`session_events` 是恢复和回滚的 transcript 权威，`messages` 是 UI 与搜索使用的物化投影。新 Session 的首条用户 seed 在 dispatch 前通过 `persist_ingress_user_seed` 写入 `messages` 和 pending marker；成功提交 `UserInject` 后再确认该 marker。

UI 的 `buildResumeMessages` 却在 `messages` 和 `session_steps` 投影为空时，从 `session.input_text` 合成 `placeholder-{session_id}` 用户气泡。它能显示已回滚或未进入当前 transcript 的旧初始输入。这个 synthetic ID 又被 submit reducer、live merge、回滚上下文菜单单独识别，形成 transcript 实体以外的并行身份路径。

## 决定

- 删除从 `session.input_text` 合成 resume message 的 fallback。resume UI 只展示已有的 `messages`/`session_steps` 投影；两者为空时 transcript 保持为空。
- 删除 `isDisplayOnlyMessageId`、`placeholder-*` 过滤，以及 submit/reducer/merge/rollback 菜单中只为 placeholder 服务的分支。
- 删除依赖这条 fallback 的测试；保留空投影不从 Session metadata 产生 transcript 的断言。
- `session.input_text` 仍用于列表摘要与未命名 Session 标题；不改变事件日志、消息表、配置、IPC shape 或数据库 schema，无需重置。
- 不额外把 `UserInject` 派生成第二份 UI seed DTO：首条输入已有 crash-safe message row，后续写入由 event stream 和既有消息投影承载。

## 验证

- `corepack pnpm run check`
- 尝试对改动 UI 文件运行 Prettier 检查；改动前的 HEAD 版本对这 5 个文件也全部不通过，未在本 ADR 中扩大格式化范围
- `scripts/check-adr-index.ps1`
- 上述 Svelte 类型检查和 ADR 索引检查通过；`git diff --check` 通过。未运行测试，本轮只执行静态/类型门禁。

## 回滚与重置

恢复 metadata fallback 与对应 placeholder guards 即可回滚。无持久化、配置、IPC 或用户数据变化，不需要重置。
