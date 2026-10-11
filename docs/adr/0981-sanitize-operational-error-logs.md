# ADR 0981：运维错误日志统一清洗并移除文件名

## 状态

已采纳并实施。

## 背景

`docs/conventions.md` 要求任意错误文本进入日志前调用 `sanitize_error_text`，并禁止记录本机路径、用户输入和完整工具输出。Compaction、memory embedding maintenance、启动/恢复、clipboard history 和 managed-media cleanup 仍有路径直接格式化错误对象的日志。Media GC 还记录上传批次名和生成文件 basename；上传名称由用户文件名派生，可能暴露本地文件信息。

## 决定

1. 上述 Agent、App 与 Tools 路径中的错误日志统一对错误的 `Display` 文本调用 `haven_common::error::sanitize_error_text` 后再写入结构化 `error` 字段。
2. embedding batch 失败仍可记录稳定 entity type/id 与错误摘要；不记录 embedding 输入文本。
3. 上传批次与生成媒体清理日志只记录动作和清洗后的错误摘要，不记录 basename 或目录项名称。文件名仍可用于本地筛选与路径操作。
4. 不改清理行为、错误恢复策略、命令结果或数据持久化。

## 替代方案

- 只在 `app_state` cleanup 汇总日志清洗：拒绝；compactor、embedding maintenance、clipboard 和单目录项 cleanup 会绕过该汇总边界直接写日志。
- 保留文件 basename 便于逐项排查：拒绝；它可能包含上传文件的原始名称，计数和通用失败类别已能定位维护路径。

## 影响与验证

- 错误摘要保持既有诊断语义并经过统一单行、限长、脱敏处理。
- 目录项日志不再暴露用户上传文件名或生成文件尾缀。
- Rust workspace 测试与 Clippy 验证此实现；无数据库、IPC、配置或用户可见行为变化。

## 回滚

回退本 ADR 涉及的日志字段和 managed-media basename 移除，并同步回退路线图与 ADR 索引；无需数据恢复。
