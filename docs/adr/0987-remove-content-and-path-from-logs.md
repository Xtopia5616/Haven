# ADR 0987：运维日志只记录内容分类与大小

## 状态

已采纳并实施。

## 背景

仓库日志规范禁止记录用户正文、模型正文、完整工具输出和本机路径，但复扫发现少数执行路径仍记录完整 transcript、stream rule 命中文本、工具错误、事实谓词、MCP server warning 与可执行文件路径。这些内容来自用户、模型、工具或外部 server，可能含私人数据、凭据、文件名或提示内容。

## 决定

1. `tracing` 日志不写入用户 transcript、模型匹配片段、工具错误原文、提取事实文本或 MCP server warning、协议协商元数据原文。
2. 排障需要时只记 byte/数量、执行阶段、稳定类别和受控实体身份；不以 `sanitize_error_text` 替代对正文的移除。
3. Skills 与 MCP 的进程/文件路径不写入普通生命周期日志。MCP fallback 只记录所用启动方式，不记录配置命令或 `.cmd` 路径。
4. 错误继续沿原有 `Result` 返回路径传递；本 ADR 只收紧日志字段，不改变用户可见错误或执行结果。

## 影响与验证

- ingress 只记 transcript 字节数；stream rule、事实过滤、工具失败、provider retry 和远端 warning 只记分类/大小。
- MCP handshake 和 reconnect 日志不记录 server 提供的协议版本、server metadata 或原始错误文本；Skills 目录扫描日志不记录文件系统错误原文。
- Skills 与 MCP fallback 日志不含绝对路径或可执行命令。
- 已对受影响 crate 运行 workspace 测试与严格 Clippy，并复核相关日志字段。

## 回滚

恢复日志原文和路径字段即可；无需数据或配置迁移。回滚会重新暴露日志规范禁止的内容。
