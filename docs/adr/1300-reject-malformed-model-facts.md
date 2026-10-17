# ADR 1300：拒绝错型模型事实条目

状态：已接受并实施（2026-10-17）。

## 背景

事实抽取把模型输出的 number、boolean、object、array 和 null 宽松转换为字符串；null 变为空字符串，后续清洗可能把它当成普通无效字段，而 object/array 会被 JSON stringify 后写入事实候选。数组中一个字段错型时，原解析器又会使整个 batch 失败，无法保留其它合法事实。事实合并与矛盾裁决 proposal 也使用同一宽松字段转换。

## 决定

1. `LlmFact` 的字符串字段和 `tags: Vec<String>` 使用 Serde 原生严格类型校验；predicate、object 保持必填，subject/tags/confidence 的现有缺省语义保留，`durability` 与 `message_index` 仍可省略。
2. 先将顶层 JSON 解析为数组，再逐项反序列化 `LlmFact`。错型条目跳过，合法条目继续进入既有清洗、来源身份验证和持久化策略；顶层不是合法 JSON 数组时，整次抽取失败，不写入任何候选。
3. 错误诊断只记录被拒绝条目数，不记录模型原文或字段值。
4. Predicate merge 与 contradiction demote proposal 的身份字段也使用严格字符串反序列化。当前维护逻辑遇到结构错误时拒绝该次 proposal batch，既有确定性 gate 不变。

## 替代方案

- 把错型值强制转换为字符串：拒绝。会让错误结构进入用户长期记忆，尤其是 null、嵌套对象和数组。
- 一个错项导致整个事实 batch 丢弃：拒绝。逐项解析可以隔离模型格式错误，同时保留经过同一安全与身份校验的合法事实。
- 静默忽略错项：拒绝。记录计数便于发现模型输出质量问题，同时避免输出敏感模型内容。

## 影响与恢复

不改变 Memory schema、持久记录、配置或 IPC，无需数据库重置。之前由宽松 coercion 写入的事实不会自动回溯清理。回退本地实现可恢复旧解析行为；回退不改动已有事实。

## 验收

- 覆盖有效字段、既有可选字段默认值，以及 number、boolean、object、array、null 和错误的 confidence/durability/message_index 类型。
- 混合有效与错型条目的 batch 仅产出有效事实；事实维护 proposal 的非字符串身份字段反序列化失败。
- `cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings` 均通过。
- `cargo test --locked -p haven-agent` 通过：674 项单元测试通过，1 项单元测试忽略；另有 2 项手动性能 profile 忽略。
