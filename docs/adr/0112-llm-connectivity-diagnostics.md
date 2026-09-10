# ADR 0112：LLM 连通性探测与可诊断报告

日期：2026-09-10
状态：已采纳

## 背景

启动时的 `check_llm_connection` 只返回 `ready`、`disconnected` 或
`unconfigured`。当 reqwest 把底层 DNS、代理、TLS 或连接错误统一显示为
`error sending request` 时，日志和状态栏都无法说明原因；重复探测还会造成同一错误
反复刷屏。一次实际故障中，模型端点和凭据后来均可正常访问，说明需要区分可恢复的
网络链路故障与认证、服务端等原因，而不是直接引导用户修改配置。

## 决定

1. `check_llm_connection` 改为返回命名 DTO `LlmConnectionReport`，包含连接状态、非
   敏感的 `reason` 分类，以及用于状态提示的 provider/model 名称；不返回 endpoint、
   请求路径、响应正文或凭据。
2. `haven_llm` 保留 reqwest 的错误链，并将连接建立、请求体和传输错误归类为
   `Network`，再映射为稳定的 `network`、`timeout`、`authentication`、`rate_limited`、
   `server`、`request_rejected`、`invalid_response`、`configuration` 或 `unknown`。
3. 后端以结构化字段记录 role、provider、model、endpoint host、reason 和脱敏后的
   错误链；日志中不记录 API key、完整 URL 的路径/查询参数或响应正文。
4. 前端首次看到断开或从 ready 变为断开时弹出带原因的错误 toast；连续探测不重复弹出。
   从断开恢复时弹出 success toast；状态栏悬停标题始终保留当前原因和修复方向。
5. Tauri 命令的 Rust 注册表、TypeScript 注册表和 `docs/ipc-contracts.md` 同步更新，
   作为同一次发布的一体化契约变更。

## 替代方案

- 继续只返回状态：实现改动最小，但用户仍只能看到“已断开”，无法判断网络还是凭据。
- 把原始错误字符串直接返回 UI：诊断信息更完整，但可能泄漏 endpoint、响应内容或内部
  路径，不符合错误边界和敏感信息约束。
- 每次轮询都弹 toast：能保证可见，但会在网络中断时持续打扰用户，拒绝。

## 影响与验证

这是一个跨 crate、跨端的 Tauri 响应 DTO 变更，不涉及数据库、配置文件或凭据格式，
不需要重置用户数据。旧版前后端不能混用，发布时必须同时替换二者。

重点验证：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --locked -- -D warnings
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

## 回滚与重置

回滚对应提交即可恢复旧的字符串响应；无需删除数据库、配置或缓存。回滚时必须同时
恢复 Rust 命令实现和 UI contract，避免一端按字符串读取、另一端发送 DTO。
