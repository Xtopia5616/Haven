# ADR 0627：命名 Tools JSON 列表预算结果

## 状态

已采纳并实施。

## 背景

Tools 的 `json_list_within_budget` 构造带列表与总数的 JSON 对象，并裁剪尾部条目以控制序列化长度。它返回 `(Value, bool)`：JSON 的 `truncated` 字段供工具响应使用，布尔值供 `ToolResult` envelope 使用；process、env、system 与单元测试都依赖 tuple 位置解释状态。

## 决定

1. 动作使用 `cap_json_list`，表达对列表输出执行字符预算限制。
2. 返回 `JsonListBudgetResult { value, truncated }`，JSON 输出与 envelope 所需状态通过字段读取。
3. 保留 JSON 的 list key、总数和 `truncated` 形状；超预算时仍从尾部移除条目，非空输入至少保留一项，即使单项本身超过预算。
4. `truncated` 表示条目被省略，不表示仅因单个保留条目超预算而被标记。

## 替代方案

- 让调用方从动态 JSON 中读取 `truncated`：拒绝，`ToolResult` 状态不应依赖反复解析动态 payload。
- 删除 envelope 布尔状态：拒绝，工具输出契约需要同时携带 JSON 字段和标准 `ToolResult` 截断元数据。
- 只将 tuple 解构变量改名：拒绝，调用方仍依赖结果位置。

## 影响与验证

- 这是 Tools 内部 helper 的 Rust API 调整，工具 JSON 与 UI/Agent 输出契约不变。
- 命名审计 §5.7 继续覆盖 Rust 动词与多值结果；其它 crate、UI、IPC、配置和持久名仍待逐域审计。
- 验证：Tools fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 `json_list_within_budget` 与 `(Value, bool)` 返回值，并同步 process、env、system 调用点和测试；无需数据或 wire 迁移。
