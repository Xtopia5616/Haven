# ADR 1313：拆分 PDF 与 OpenXML 文档解析器

## 背景

`crates/tools/src/document.rs` 同时负责受管文件读取、取消/大小限制、page cursor 和文本归一化，以及 PDF stream/token 与 OpenXML ZIP/XML 的两套解析算法。后两者各自有独立格式细节、解压上限和错误路径；共享部分则是 facade 的字节快照与解析编排。

## 决定

1. 将 PDF stream/token 解析和 PDF 专属解压限制迁至私有 `document/pdf.rs`；将 DOCX/XLSX/PPTX ZIP/XML 解析和 Office 专属解压限制迁至私有 `document/open_xml.rs`。
2. `document.rs` 保持受管输入 facade，继续拥有 format selection、文件字节上限、取消检查、page cursor、输出截断与文本归一化；两种 parser 都接收已验证的字节快照和同一个 cancellation token。
3. 两项直接验证 PDF token 解析与 PDF 解压上限的测试随 parser 放入其私有模块；跨格式入口、页游标、DOCX/XLSX、加密拒绝和取消行为测试继续留在 facade。
4. 不新增依赖，不改变公开 extraction API、格式映射、限制数值、输出文本或错误契约。

## 替代方案

- 继续将两套解析算法放在 facade：拒绝；格式解析逻辑已有各自稳定的内部 owner，后续针对 PDF 与 Office 的安全修正可单独定位与验证。
- 为每个 Office 格式新增 parser crate 或公开 trait：拒绝；Docx/Xlsx/Pptx 共享 ZIP/XML 读取层，当前没有独立消费者或依赖边界收益。
- 将所有 reader/cancellation/size policy 移入格式 parser：拒绝；输入边界和统一页/截断策略属于 facade，不应重复。

## 影响与回滚

这是 Tools crate 内部职责整理，不改变 IPC、数据库、durable event、文件持久格式或用户可见行为，无需重置。回滚时将两个私有 parser 文件并回 `document.rs`，保持 facade 调用不变。

## 验收

- `cargo test --locked -p haven-tools document::`：11 passed；包括 PDF/DOCX/XLSX 解析、压缩上限、加密拒绝、字节快照与取消检查。
- `cargo clippy --locked -p haven-tools -- -D warnings` 通过。
- `cargo fmt --all -- --check` 与 `git diff --check` 通过。
