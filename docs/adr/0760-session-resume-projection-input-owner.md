# ADR 0760：Session resume 投影输入归属 builder

## 状态

已采纳并实施。

## 背景

`contracts/sessionHistory.ts` 已通过 generated `SessionResumeResponse` 表达 `get_session_for_resume` 与 `get_latest_session_for_resume` 的完整 wire response。该模块还额外声明了 `SessionResumeInput`，供 `buildResumeMessages` 接收容错行数据；类型同时列出 `session`、`usage`、`llm_usage` 和 `interactions`，但 builder 实际只读取 `messages` 与 `steps`。Chat session controller 测试又复用它模拟完整 command response，使 projection input 与 command response 的 owner 混在一起。

## 决定

- 完整恢复命令响应继续由 generated `SessionResumeResponse` 拥有，`contracts/sessionHistory.ts::SessionResumeResponse` 只作为其语义别名。
- 将容错输入改为 `ResumeTranscriptProjectionInput` 并放在 `resumeMessages.ts`，仅包含 builder 实际读取的 `messages` 与 `steps`；行字段从 generated `SessionResumeMessage` / `SessionResumeStep` 派生。
- `buildResumeMessages` 接受该窄输入的结构扩展，因此调用方可直接传完整 resume response；不构造第二份 response shape。
- controller 测试用本地 `resumeResponseFixture` 表示模拟响应，不再导入 production projection type 来冒充 response contract。

## 替代方案

保留 `SessionResumeInput` 会让纯投影函数继续暴露无消费者字段，并暗示它与 command response 是同一种输入契约。为 builder 预先复制 `messages` / `steps` 又会增加无意义的临时对象和重复映射。

## 影响与验证

只调整 UI 内部 TypeScript 类型 owner 与测试 fixture 命名，不改变 IPC、恢复顺序、transcript 投影、数据库或持久化。验证 UI type check、`test:run`、Prettier 和 staged diff 检查。

## 回滚

将 `ResumeTranscriptProjectionInput` 恢复到 contracts 并恢复旧输入字段即可；无 IPC、数据或持久化迁移。
