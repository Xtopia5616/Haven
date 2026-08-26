# 架构决策记录（ADR）

本目录保存影响跨 crate 边界、持久化契约、IPC、安全语义或发布兼容性的短决策记录。文件名采用 `NNNN-简短主题.md`，按编号递增且不重用。

每个 ADR 至少说明：背景、决定、替代方案、影响、验证，以及回滚或数据重置方式。被新决定替代的 ADR 保留原文，并在顶部链接到替代记录。

当前记录：

- [0001：固定可重复的质量基线](0001-reproducible-quality-baseline.md)
- [0002：隔离本机工具测试与未配置 OCR](0002-isolated-local-tool-tests.md)
- [0003：会话 IPC 契约边界](0003-session-ipc-contract-boundary.md)
- [0004：删除安全确认模式 `always` 配置别名](0004-remove-confirmation-mode-alias.md)
