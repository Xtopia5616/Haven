# ADR 1310：将 Tools 文件系统策略迁入私有模块

状态：已接受并实施（2026-10-18）。

## 背景

`security.rs` 同时持有唯一授权 engine 和文件读写策略、路径规范化/重解析检查。文件策略由多个 Tools capability 消费，路径处理自身有独立的文件系统安全不变量；将它们放入私有 owner 可以明确职责，同时保留 engine 对授权 roots 的唯一决策权。

## 决定

1. 将 `FileReadPolicy`、`FileWritePolicy`、路径允许判断、目录 capability 打开、路径解析及文件句柄身份验证实现迁入私有 `security/filesystem.rs`。
2. 将直接验证这些路径和文件策略行为的七项测试迁入该模块；engine 决策、工具配置与授权交互测试仍依赖唯一 `AuthorizationEngine` fixture 并留在 `security.rs`。
3. 保留 `security.rs` 的内部 facade re-export，使现有 Tools 消费者继续使用 `FileReadPolicy`、`FileWritePolicy` 与 `is_safe_local_path` 原路径。前两者仍为 crate 内接口，后者仍是 App 调用的公开安全入口。
4. `AuthorizationEngine` 仍是允许路径、读写 roots 与 sandbox 决策的唯一 owner；模块移动不改变路径验证顺序、拒绝条件、句柄绑定、授权结果或配置读取。

## 替代方案

- 将文件策略继续留在 engine 文件中：拒绝；文件句柄验证与路径解析不依赖授权状态，且被多个文件能力调用。
- 在 filesystem 模块复制授权配置或独立计算允许 roots：拒绝；这会产生第二个安全策略来源。该模块只消费 engine 派生的 roots/capabilities。
- 将 filesystem helper 暴露给其他 crate：拒绝；当前只有 `is_safe_local_path` 是既有跨 crate 入口，不新增路径策略 API。

## 影响与回滚

没有授权、IPC、数据库、配置或用户可见安全语义变化，不需要重置。回滚时将私有文件策略和 helper 并回 `security.rs`，保留当前消费方调用路径与公开 `is_safe_local_path` re-export。

## 验收

- Windows `cargo check --locked -p haven-tools` 通过。
- Windows `cargo test --locked -p haven-tools -- --test-threads=4`：975 passed、2 ignored；MCP integration 7 passed。
- Windows `cargo clippy --locked -p haven-tools -- -D warnings` 通过。
- `cargo fmt --all -- --check` 通过。
- installer UI 与 HCS quota 超限的实际 release 验收不受该私有模块切片影响，仍由路线图 §5.1 Open Gate 追踪。
