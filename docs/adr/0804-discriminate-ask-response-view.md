# ADR 0804：区分 Ask answer 与 ignored response

## 状态

已采纳并实施（2026-10-08）。

## 背景

`AskResponseView` 虽已成为 interaction/reducer/chat 的共同类型 owner，但两个字段都可选，允许 `{}` 和 `ignored: false` 这类不表达任何已解决结果的值。生产 resolve path 只产生 `{ answer: string }` 或 `{ ignored: true }`。

## 决定

将 `AskResponseView` 改成互斥判别联合：答案分支要求 `answer` 字符串并只允许 `ignored: false`；忽略分支要求 `ignored: true` 且禁止 `answer`。现有消费者按 `ignored` 选择展示/提交路径，无需再转换或强转。

## 影响与回滚

只收窄 UI 内存 view 类型和合法测试 fixture，不改变 IPC/event、持久化或 Ask 交互行为。无需迁移或重置。

## 验收

运行 UI 类型检查、完整 UI 测试和 ADR 索引检查；现有 answer 与 ignored 两条路径 fixture 覆盖联合成员。
