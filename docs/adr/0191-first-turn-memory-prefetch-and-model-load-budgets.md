# ADR 0191：首轮记忆预取与主模型负载预算

## 状态

已接受（2026-09-21）

## 背景

新会话和恢复会话在首个 provider 请求之前同步构造完整 system prompt。完整构造会
执行 prompt memory recall；配置 embedding provider 时，向量查询的网络延迟因此会
直接推迟首 token。首轮请求并不需要等待语义记忆，且后续 ReAct 回合已有 MEMORY
fence 增量刷新边界可以承接迟到的结果。

同时，默认上下文回退窗口、输出 token floor、工具观察、工具 schema 数量和 reasoning
回显上限偏宽，会让主模型在没有显式配置时承担不必要的输入/输出负载。

## 决定

- 新会话和恢复会话先构造不执行语义记忆召回的 system prompt，并立即进入 ReAct
  首轮请求。
- `MemoryWorker` 在后台预取一次有界的 prompt memory。每个 session 只允许一个预取，
  全局最多两个并发预取；结果写入 `MemoryService` 的现有有界缓存，完成后复用已有
  `before_step` MEMORY fence patch。会话清理会取消未完成的预取；provider 自身的请求
  timeout 仍是最终时限。预取失败只降级为当前回合无语义记忆，不阻断会话。
- 默认负载预算调整为：compaction ratio `0.65`、reserve `8192`、上下文回退窗口
  `64K`、输出 token floor `32K`、工具观察 `16000` 字符、reasoning echo `1200`
  字符、每次请求最多 `64` 个工具。用户显式配置的 reasoning effort、上下文窗口和
  高于全局 floor 的 endpoint 输出值仍优先；全局 floor 仍按其既有语义抬高过小的
  endpoint 输出值。

## 替代方案

- 首轮同步等待 embedding：实现最简单，但把不可控的 embedding 网络延迟放在首 token
  前，拒绝。
- 首轮完全关闭记忆：虽然消除了延迟，但会丢失可在第二回合复用的语义召回结果，拒绝。
- 按用户任务启发式筛选工具：可能把任务所需能力静默隐藏；继续使用现有分层加载和
  确定性数量上限。

## 影响

首轮 system prompt 的 MEMORY fence 初始可能为 `(none)`；如果后台预取及时完成，
同一轮的 `before_step` 会从缓存完成无网络 patch，否则从后续回合开始可见。持久化
事件、canonical 顺序、工具加载语义和恢复契约不变。默认配置更偏向短 reasoning、较早
压缩和窄工具面；长输出或超大上下文工作负载可在设置中主动提高预算。

## 验证与回滚

已验证 `cargo check --locked -p haven-agent -p haven-common -p haven-tools`、
`cargo test --locked -p haven-agent --lib`（458 tests）以及首轮无记忆 prompt、预取
去重/取消回归测试。回滚只需恢复首轮完整 prompt 构造和旧默认值，不涉及数据库 schema
或配置迁移；已有配置中的显式值保持不变。
