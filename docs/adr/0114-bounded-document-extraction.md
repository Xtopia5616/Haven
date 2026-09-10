# ADR 0114：受限本地文档抽取与派生表示

日期：2026-09-10
状态：已采纳（阶段 5 的第一条垂直切片）
关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)

## 背景

阶段 3 已让普通附件以 `asset_id` 进入受管 `files` 工具，但 PDF、DOCX、XLSX
和 PPTX 仍只返回二进制提示。把本机路径直接交给模型会破坏 0113 的边界；把
原始文件重新编码成 prompt 也会失去资源上限和派生内容的 provenance。

## 决定

在 `haven-tools::document` 增加一个本地、同步、受限的文档表示步骤。`files`
的完整 `read` 对受管文档调用该步骤，返回结构化派生结果：

- PDF：解析未加密文本内容流，支持常见 `FlateDecode`；
- DOCX/PPTX：只读取 ZIP 包内的受支持 XML 内容部件；
- XLSX：读取共享字符串和工作表单元格，输出 `table_data`；
- 结果带 `provenance=document_extract`、表示类型、段数、大小和
  `untrusted_content=true`，正文用既有派生内容围栏包裹；
- `asset_id` 调用只读，返回结果删除 `path`/`root`/`from`/`to` 等宿主路径字段；
- 没有解析器的格式返回明确的 `unsupported_format` 降级结果；加密 PDF、未知
  压缩过滤器、损坏/空内容和超限文档返回 `success=false` 的
  `document_extract_failed`，不伪造空文本，也不回传原始字节。摘要请求还遵守
  ADR 0117 的静态 system + user/data 边界。

## 资源与安全边界

- 单个文档读取上限为 32 MiB；Office 单个 XML 部件上限为 8 MiB，总解压内容
  上限为 24 MiB，防止 ZIP 炸弹和长时间解析。
- 抽取在阻塞线程执行，调用前后检查取消；错误日志只保留抽取失败原因，不把
  文件正文或受管路径写入模型可见结果。
- XML 不解析外部实体；PDF 仅解析文本操作符，不执行任何嵌入内容、脚本或附件。
- 该步骤不会新增 SQLite 表或改变 snapshot/messages schema。派生正文只作为
  当前工具 observation；持久化仍由既有 X12 投影边界负责。

## 替代方案

- 依赖系统 `pdftotext`/Office：不可重复、Windows 安装不稳定，也会扩大进程边界；
- 把 PDF/Office 原文件上传到 provider：缺少统一 provider file-upload 契约，且
  会绕过 0113 的能力画像和路径隔离；
- 将所有文档视为纯二进制：安全但无法完成附件理解。

## 验证与回滚

单测覆盖未压缩/Flate PDF、加密 PDF fail-closed、PDF 转义、DOCX XML 转义、XLSX
共享字符串/数值单元格、派生围栏和 managed asset 的路径脱敏。回滚只需移除
`files` 的文档分支与 `haven-tools::document` 模块；受管资产 registry 和旧文件
读取路径仍可继续工作，无数据库重置要求。

## 后续

PDF 字体编码、扫描版 OCR、旧式二进制 Office、复杂表格结构和视频 keyframe 不在
本切片承诺内；这些能力必须各自增加 provider/资源矩阵和回归样本后再接入。
