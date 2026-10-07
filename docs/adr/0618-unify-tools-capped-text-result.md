# ADR 0618：统一 Tools 限长文本结果

## 状态

已采纳并实施。

## 背景

`OutputBudget::cap_text` 返回 `(String, bool)`，用来传递按 Unicode 字符数限长的文本和截断状态。文件摘要另有 `cap_chars` 包装该方法；媒体处理的 `bound_text` 则重复实现相同逻辑。三个入口含义一致，调用方仍靠 tuple 位置识别正文与截断状态。

Common 的 `truncate_output` 虽也返回文本和截断状态，但会在文本尾部追加说明省略字符数，语义和限长方式不同。

## 决定

1. `OutputBudget::cap_text` 返回公开具名结果 `CappedText { text, truncated }`。
2. 删除文件摘要的 `cap_chars` 和媒体处理的 `bound_text`，二者直接调用 `OutputBudget::cap_text`。
3. 保留 Common `truncate_output` 的带省略 marker 行为及独立 API。
4. 字符上限、UTF-8 边界和截断时的工具结果状态保持不变。

## 替代方案

- 只把两个 helper 改成同一个名字：拒绝，它们最终仍返回位置 tuple，调用方无法从类型看出字段含义。
- 合并到 Common `truncate_output`：拒绝，该函数会追加 marker，限长结果可能超出指定字符数，和 Tools 当前静默截断契约不同。
- 为两个 feature 各定义一个结果结构：拒绝，两个 feature 调用的是同一个 Tools 输出限长策略，不应复制结果 owner。

## 影响与验证

- 这是 `haven-tools` 的 Rust source API 返回类型调整；所有调用方使用具名字段，输出字符、截断标记和工具结果语义不变。
- 命名审计 §5.7 保持 Active；其他 crate、UI、IPC 和配置/持久名称仍待完整盘点。
- 验证：Tools fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 `OutputBudget::cap_text` 的 tuple 返回值和两个 feature helper；无需数据或 wire 迁移。
