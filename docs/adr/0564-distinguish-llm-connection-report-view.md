# ADR 0564：区分 LLM 连接 wire report 与 renderer view

## 状态

已采纳并实施。

## 背景

生成的 `LlmConnectionReport` 是 `check_llm_connection` 的 wire 响应，`provider` 与 `model` 为必需字符串。UI `llmConnection.ts` 也声明同名类型，但 `normalizeLlmConnectionReport` 接收不可信 `unknown`，对异常 status fail closed，并允许这些展示标识缺失。UI 本地 status 与 failure-reason union 又手工重复了 generated enums。

## 决定

1. 生成类型 `LlmConnectionReport` 继续表示严格 IPC 响应；renderer 投影命名为 `LlmConnectionReportView`。
2. UI normalizer 与提示格式化函数使用 View 类型；shell status 与 view 内的 status/reason 直接引用 generated unions。
3. 保持命令、wire 字段、错误归一、敏感数据过滤与用户提示不变。

## 替代方案

- 让 UI view 沿用 generated wire 类型：拒绝。这样会要求可能缺失的字段必需，并掩盖 boundary normalizer 的输出约束。
- 合并/放宽 generated IPC 类型：拒绝。Rust command 契约拥有严格 wire shape，不应因 UI 对畸形响应的容错投影而弱化。
- 继续重复声明 status/reason union：拒绝。值集合相同，应由生成契约维持单一来源。

## 影响与验证

- 只改 renderer 内部类型名和 enum 类型来源；Tauri wire shape 与运行行为不变。
- 验证：UI `check`、`test:run`、`build`，ADR 索引与差异空白检查通过。

## 回滚

将 `LlmConnectionReportView` 恢复为 UI 本地 `LlmConnectionReport`，并重新在 `llmConnection.ts` 声明 status 和 reason union。
