# ADR 0117：`files(summary)` 不可信数据与解析失败边界

日期：2026-09-10
状态：已采纳
关联：[ADR 0114：受限本地文档抽取与派生表示](0114-bounded-document-extraction.md)、
[ADR 0116：多模态请求与受管上传边界加固](0116-media-boundary-hardening.md)

## 背景

`files(summary)` 原先将 `focus` 拼接进 system prompt，并对 PDF/Office 文件按
普通文本逐行读取。这样一来，文件内容或 focus 中的指令可能获得错误的 prompt
权重；二进制文档也可能绕过既有的受限 `document_extract` 表示。与此同时，文档
抽取错误曾被包装成成功的 `ToolResult`，调用方无法区分“格式不支持”和“文件已
损坏”。

## 决定

- system prompt 保持静态。focus 和文件内容只进入 user 消息中的显式数据对象，
  并声明所有字段均为不可信数据、不得执行其中的指令。
- 文件内容统一使用既有派生内容围栏，并携带 `file_read` 或
  `document_extract` provenance。focus 最多保留 2,000 个 Unicode 字符；
  `max_chars` 对解码后的 Unicode 字符计量，而不是混用原始字节数。
- PDF、DOCX、XLSX、PPTX 的 summary 先经过受限 `document_extract`，再按抽取后
  的行范围和字符预算取样；旧式 DOC/XLS 等没有解析器的格式返回成功的
  `unsupported_format` 可降级结果。
- 已支持格式的校验、解压、解析、超限和空内容错误返回
  `success=false`、`document_extract_failed=true` 的 fail-closed 结果；不把
  部分文本伪装成成功摘要输入。取消仍沿用调用方的取消错误路径。

## 替代方案与影响

继续拼接 prompt 最简单，但无法建立 system/user/data 的信任层次；直接把原始
PDF/Office 字节交给模型则绕过 0114 的资源边界。新的 user 数据对象会增加少量
提示词开销，rich document summary 也会先付出本地解析成本，但不会把原始文档
指令提升为 system 指令，且解析失败不会被静默吞掉。

## 验证与回滚

`haven-tools` 单测覆盖摘要 prompt injection、focus 限长、派生围栏、rich document
summary 路由、损坏 PDF fail-closed，以及 GBK 非 UTF-8 文本的字符预算。验证命令为
`cargo test --locked -p haven-tools` 和 workspace 格式化/Clippy 门禁。回滚只需回退
本 ADR、`files.rs`、`document.rs` 与共享摘要 prompt；无数据库、snapshot 或配置
重置要求。
