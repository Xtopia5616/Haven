# ADR 1350：扩展独立网页搜索与来源内容提取

## 状态

Accepted — 2026-10-21

## 背景

独立网页搜索只返回 provider 规范化后的标题、链接与摘要，无法按单次调用控制结果数量、时效、地区、语言和域名，也不能在摘要不足时读取来源页面。通用 HTTP 工具只解析 HTML 页面；JSON、RSS/Atom 与 PDF 来源不能以一致的可读文本返回。

## 决定

1. `web_search_api` 接受 `max_results`、`recency`、`country`、`language` 和 `domains`。筛选由 `haven-llm` 按 provider 映射；未配置支持的 provider 字段会在结果 `filters.ignored` 中列明，`filters.applied` 列出已生效字段。调用级 Perplexity 筛选覆盖对应配置值；未指定的筛选继续使用配置值。
2. `fetch_top_results` 默认为 0，最大为 3。仅显式请求后才会读取排名靠前的来源页；页面内容按结果附在 `page_fetch` 下，整体抓取摘要放在 `fetch_summary`。单次调用并发和同一工具实例的跨调用并发均限制为 3，页面正文最多返回 4,000 字符。
3. 来源页抓取复用 HTTP 的公开目的地检查、DNS 地址固定、同源重定向限制、响应体上限和取消路径；每页超时 15 秒。抓取参数会把工具风险提升到 Medium。抓取失败不抹掉搜索结果，并以通用错误信息返回。
4. HTTP 响应按格式提取：HTML 优先提取 `main` / `article` 正文；JSON 格式化输出；RSS/Atom 提取最多 20 个条目；PDF 复用既有文档解析器。超出 HTTP 响应体限额或无法读取的 PDF 以可读说明返回；扫描版/加密/不支持的 PDF 不做 OCR。
5. 搜索与页面正文均为不可信外部内容。工具描述及来源面板明确禁止把网页文本当作指令执行；页面内容使用既有工具输出上限与 UI 文本渲染。

## Provider 筛选支持

| Provider | `max_results` | `recency` | `country` | `language` | `domains` |
|---|---|---|---|---|---|
| Perplexity | 支持 | 支持 | 支持 | 支持 | 支持 |
| Alibaba AI Search | 支持 | 支持 day/week/month/year；hour 忽略 | 忽略 | 忽略 | 忽略 |
| DeepSeek Search | 支持 | 忽略 | 忽略 | 忽略 | 忽略 |
| Mistral Web Search | 支持 | 忽略 | 忽略 | 忽略 | 忽略 |
| Zhipu Web Search | 支持 | 忽略 | 忽略 | 忽略 | 忽略 |

## 替代方案

- 默认抓取搜索结果：拒绝。隐式访问额外来源会扩大网络请求量和授权范围；按需抓取可让用户和模型明确控制。
- 为各 provider 暴露不同 schema：拒绝。单一筛选 schema 让模型调用稳定，并可显式报告 provider 能力差异。
- 新建 PDF 解析器或为 RSS 引入大型抽取依赖：拒绝。PDF 复用已有文档 parser，RSS/Atom 使用现有 `quick-xml` 依赖。

## 影响、验证与回滚

新增字段只扩展动态工具参数和 JSON 结果，不改变持久化、IPC DTO 或 transcript 身份。抓取沿用现有网络安全策略；取消时释放并发许可，单次最多抓取三页，正文、响应字节、耗时和 provider 响应均有界。用户可见变化包括来源页面正文、格式标签和未生效筛选提示。

验证（2026-10-22，Rust 1.99.0、Node.js 24.21.0、pnpm 12.12.1）：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`pnpm run check`、`pnpm run test:run`（165 个文件、1408 项通过）和 `git diff --check` 均通过。回滚时移除筛选和来源页扩展、恢复 HTML-only HTTP 输出及对应 UI/文档即可，无需数据重置。
