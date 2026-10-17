# ADR 1308：将 Tools 授权契约迁入私有模块

状态：已接受并实施（2026-10-18）。

## 背景

Tools 的授权请求/结果、确认票据、执行证明和 `AuthorizationPort` 与唯一 `AuthorizationEngine` 混在 `security.rs`。这些类型构成跨 crate 稳定契约，而 engine/config 是 Tools 内部策略 owner；应在模块边界上区分二者，同时保留现有 crate API 与证明构造约束。

## 决定

1. 将 `ConfirmationReceipt`、`AuthorizationDecision` / reason code、`AuthorizationRequest`、execution proof/check 和 `AuthorizationPort` 移入私有 `security/contract.rs`。
2. `security.rs` 继续作为稳定 facade 并 re-export 所有既有公共授权类型与 trait；`haven_tools` crate 根导出和字段/serde shape 不变。新模块不公开给其他 crate。
3. execution proof 的绑定数据与 proof-construction helper 保持对 crate 外不可见；proof 仍 move-only，外部只能经授权 port 验证或从已批准 receipt 获得 proof。
4. 授权与 proof 绑定测试依赖 engine policy fixture，继续由 `security.rs` 的 engine 测试组持有；不为搬文件而复制 fixture 或拆断因果场景。
5. `AuthorizationEngine`、单份 `SafetyConfig`、策略 revision、critical-floor、grant scope 和 revoke owner 均未拆动。后续 engine/execution/filesystem 与有序检查切片仍处于 Slice 5 Active 状态。

## 替代方案

- 保持契约与 engine 混在同一 facade 文件：拒绝；稳定的跨 crate contract 和内部策略有不同可见性与变化原因。
- 让 Agent/App 直接依赖 `security/contract` 子模块：拒绝；子模块仍是 Tools 私有实现，调用方继续使用 `haven_tools` façade。
- 为 receipt/proof 测试新建一份 mock policy engine：拒绝；会复制授权状态和安全策略，现有集成断言应继续通过唯一 engine 验证。

## 影响与回滚

没有 wire、serde、DB、IPC 或授权语义变化，不需要重置。回滚时把 `contract.rs` 类型并回 `security.rs` 并保留原 façade re-export；不需改动任何调用点。

## 验收

- `cargo check --locked -p haven-tools` 通过。
- `cargo test --locked -p haven-tools -- --test-threads=4`：975 passed、2 ignored；MCP integration 7 passed。默认高并发运行中的两个无关子进程用例曾间歇失败，runner 用例单独重跑通过，4 线程全量运行通过。
- `cargo clippy --locked -p haven-tools -- -D warnings` 通过。
- `cargo fmt --all` 已执行；提交前格式检查复验。
