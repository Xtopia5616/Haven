# ADR 0625：命名 Common 截断文本结果

## 状态

已采纳并实施。

## 背景

`haven_common::encoding::truncate_output` 被文件读取、剪贴板、文档、HTTP 与 Shell 工具路径共同使用。它按 Unicode 标量值截断并追加省略字符数标记，原返回 `(String, bool)`，调用方按 tuple 位置绑定两个值。Tools 的 `OutputBudget::cap_text` 也返回文本和截断状态，但不追加 marker，且由不同 owner 管理、遵循不同预算策略。

## 决定

1. Common truncation returns `TruncatedOutput { text, truncated }` so callers identify both values by field.
2. Keep `truncate_output` as the action name: it already describes the operation and the new result makes the helper's output explicit.
3. Update Common tests and all production consumers to read named fields.
4. Keep the marker text, character count, Unicode boundary behavior and all caller truncation decisions unchanged. Keep this Common helper distinct from Tools `CappedText`.

## 替代方案

- Merge the Common result with Tools `CappedText`: rejected because their marker behavior and ownership differ.
- Infer truncation from whether the marker appears in the output: rejected because that couples consumers to presentation text and may misclassify literal input containing the marker.
- Keep the tuple and rename destructured locals: rejected because the shared return contract remains position-based.

## 影响与验证

- This changes an internal workspace API shared by Common and Tools; no serialized, Tauri or persistence contract changes.
- Naming audit §5.7 remains Active; the rest of the Rust, UI, IPC, configuration and persistence inventory remains to be reviewed.
- Validation: workspace fmt, locked check, strict Clippy, workspace tests, ADR index and staged diff checks.

## 回滚

Restore the `(String, bool)` result and update Common tests plus all truncation consumers; no data or wire migration is needed.
