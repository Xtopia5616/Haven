# ADR 1156：显式区分立即调整与完成后追加

- 日期：2026-10-14
- 状态：已实现

## 背景

原 ingress 主要按 Running 自动选择 steering；actor 只按 steering 优先级 drain，普通工具续轮也会消费 follow-up。因此两队列不能表达「先完成当前工作，再做下一件事」。参考 [Pi 队列契约](https://github.com/earendil-works/pi/blob/main/packages/agent/README.md#steering-and-follow-up)。

## 决定与不变量

- `InputDeliveryMode` 为 `auto` / `steering` / `follow_up`。IPC 可显式选择；省略时继续使用 Auto，适用于语音和 Ask。运行中输入框提供「立即调整」「完成后追加」，Ask 等待时仍按答案提交。
- Steering 在下一个安全 turn 边界注入，不取消当前模型请求或工具。Follow-up 等到模型自然完成（普通文本完成或 final_answer）才注入并开始下一段工作；工具续轮、重试、Ask/确认暂停、预算耗尽、错误或取消都不是工作完成。
- Actor 的 `ContextBoundary` 区分 `Turn` 和 `WorkComplete`，保留 steering 优先级、原有容量上限和 FIFO 批次预算。普通边界允许 Ask 答案绕过尚未到期的 follow-up；显式 follow-up 不得被改写为答案，也不得单独唤醒 Ask。
- Ask batch 收尾通过 actor 原子地准备早到的 steering 答案并读取答案资格，不再用「存在任意 pending context」判断可否恢复；follow-up 和后台结果都不构成 Ask 答案。
- 自然结束路径在最终 assistant transcript 提交后读取完成边界队列。恢复到已完成的普通 assistant tail 时，首次读取也允许 follow-up；该资格仅在启动读取一次，不能根据 live loop 上一条 assistant 响应反复推断，避免 web-search 等续轮提前消费。
- 待投递 marker 固定 `answer` / `steering` / `follow_up`；恢复逐字使用已保存路由和 message_id，不按新状态重算。UserInject 仍经唯一 durable transcript writer 提交后发布 UI。
- 队列继续由单 session actor 串行持有；取消/错误保留未消费项，恢复 marker 只在内存队列为空时重新排队。不新增后台任务、依赖或授权路径。

已知恢复边界：含 provider web-search 元数据的 assistant tail 可能是中断续轮，启动时保守不视为完成；该场景的 follow-up 等待后续自然完成后再消费，避免在尚未完成时提前注入。

## 替代方案

仅重命名 UI、仅调整队列优先级都无法阻止工具续轮消费 follow-up；仅以内存存储选择会在重启后丢失意图，因此不采用。

## 影响与风险

schema 升至 v43，旧数据库按 release-and-reset.md 显式重置，无运行时迁移；config.toml、inbox 不变。主要风险是 Ask 被延后、工具续轮提前消费、重启丢失路由，分别覆盖行为回归。输入文本和附件不新增日志输出，既有安全确认策略不变。

actor 与 session facade 的既有大文件继续分别持有队列热状态和监督器操作，避免拆出第二个队列 owner；tool_batch 的 Ask 收尾属于同一批次状态转换职责。Common types 仅新增实际被跨层使用的纯枚举，仍使用已有类型 owner，不吸收编排。现有集成测试复用所属夹具；请求准备的新夹具与测试独立于大文件。

## 验证与回滚

新增端到端工具续轮测试、显式模式/恢复、Ask 阻塞与 Ask 收尾资格测试，以及输入框与 submission lane 测试。旧 Ask 恢复测试改用显式 answer API，不再将 follow-up 当作答案。

验证通过：`cargo fmt --all -- --check`、Rust workspace check、严格 Clippy 和 workspace tests；UI check、全量测试（1285 项）与生产构建；IPC 事件/生成契约检查（81 个 handler）及 `git diff --check`。覆盖显式投递、恢复、Ask 阻塞与收尾资格、工具续轮和 submission lane。

回滚代码后也须重置 SQLite（v43 与旧二进制不兼容）；无需重置配置或 inbox。
