# ADR 0623：命名 Tools capped stream 结果

## 状态

已采纳并实施。

## 背景

Tools process output 有两个阶段：byte-level reader 保留至多 `max_bytes`，继续 drain 剩余管道数据，并返回 bytes、overflow 状态和可能的 I/O error；text-level reader 将保留 bytes 解码为文本并可同步输出 tail。两者原分别返回 `(bytes, overflowed, error)` 与 `(text, overflowed)`，调用方按位置绑定。byte-level helper 名 `read_stream_capped_with` 也没有表达它完整 drain 流并把每个 chunk 交给 observer 的行为。

## 决定

1. byte-level 结果命名为 `CappedStreamRead { bytes, overflowed, error }`，helper 改名 `drain_stream_with_byte_cap`。
2. decoded text 结果命名为 `CappedStreamText { text, overflowed }`，helper 改名 `read_stream_text_capped`。
3. SkillRunner 从 `CappedStreamRead` 字段读取 bytes、overflow 和 error，并将退出状态与 stdout/stderr 收拢为 `SkillProcessOutput`；Shell 与 background ToolRun 从 `CappedStreamText` 字段组装输出。
4. 保持 byte cap、超限后继续 drain、tail tee、解码、SkillRunner error handling 和 Shell/ToolRun best-effort read handling 不变。

## 替代方案

- 只把 tuple 解构变量改得更长：拒绝，返回值的阶段含义仍不在类型中表达。
- 让 text reader 返回 `CappedStreamRead`：拒绝，text reader 会解码并仅暴露面向 Shell/ToolRun 的文本和 overflow；SkillRunner 使用的 byte-level error 不该混进该消费结果。
- 把 text/bytes/error 合并成一个全阶段对象：拒绝，读取、解码和消费者错误策略分属不同阶段。

## 影响与验证

- 这是 `haven-tools` crate 内部 Rust API 调整，子进程读取上限、预览和错误处理行为不变。
- 命名审计 §5.7 保持 Active；其它 crate、UI、IPC 与配置/持久名仍需逐域审计。
- 验证：Tools fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复两个 tuple 返回类型及 `read_stream_capped_with` helper 名，并同步 Shell、ToolRun、SkillRunner、测试与 crate re-export；无需数据或 wire 迁移。
