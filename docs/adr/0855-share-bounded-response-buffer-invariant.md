# ADR 0855：共享有界响应体字节累积不变量

## 状态

Accepted — 2026-10-10

## 背景

LLM HTTP transport 与 MCP Streamable HTTP transport 各自实现了响应体的 `Content-Length` 超限预拒绝、容量预分配和逐 chunk 字节上限检查。三段检查共同定义一个纯字节不变量，重复实现会让安全上限或边界算法发生漂移。

完整读取循环却不属于同一职责：MCP 读取必须响应取消并有独立 body deadline，LLM 读取没有相同的生命周期契约；两侧还将流错误映射为不同协议错误。文本与 JSON 解码也应留在各自 transport 边界。

## 决定

- 在无内部依赖的 `haven-common` 中引入 `BoundedBytes`，统一 Content-Length 预拒绝、有界容量预分配与追加每个 chunk 前的容量检查。
- LLM 与 MCP transport 继续拥有网络流读取、取消、deadline、文本/JSON 解码和协议错误映射；超限错误由各自 adapter 映射为原有错误类型及文案。
- Common 不增加网络、异步或协议依赖，也不拥有响应体生命周期、重试或日志。
- 不改变各调用点的字节上限、完成条件或可观察错误语义。

## 替代方案

- 合并两侧完整的 `read_bytes_bounded` 包装：拒绝。MCP 的取消与独立 deadline 以及两侧错误类型不同，放入 Common 会将传输生命周期和协议语义下沉到共享层。
- 保留两份独立容量检查：拒绝。同一逐块字节不变量会继续存在多个权威实现。
- 引入带 reqwest/stream 类型的通用网络工具：拒绝。Common 应保持纯共享数据/算法层，调用方已有不同流与错误契约。

## 影响与验证

这是 Common 跨 crate 的纯数据 API；无 IPC、事件、配置或持久化变化，无需数据库/配置重置。两个 transport 已有边界测试覆盖 Content-Length 预拒绝、流式超限；MCP 另有取消与 deadline 覆盖，测试断言未改变。本轮按执行约束未运行测试套件。

验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-crate-dependencies.ps1`、
`scripts/check-adr-index.ps1`、ADR Prettier 与 `git diff --check`。未运行测试套件。

## 回滚

若共享 API 不适用于其他稳定消费者，整体撤回两处 transport 调用与 Common 类型，并恢复各自原有边界检查；不新增旧名兼容包装。无持久数据需要重置。
