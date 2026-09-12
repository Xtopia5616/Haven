# ADR 0132：Operation view、结构化错误与 HTTP 安全契约

## 状态

Accepted — 2026-09-12

## 背景

P1 工具体验优化已经注册了 `files.*` 和 `system.info` 的独立模型视图，但如果 schema、风险、权限、renderer 和 prompt 分散在不同层，新增别名会继续产生漂移。与此同时，agent 需要区分可重试的瞬时失败、未知结果和权限/校验失败；HTTP 能力若先扩展下载或缓存，则必须先有 SSRF 防护和逐跳重定向边界。

## 决策

1. 用后端 `OperationViewContract` 作为 operation view 的契约源，统一声明固定 operation/scope、schema、风险、幂等性、并发资源、权限键、renderer、icon 和 prompt 说明。模型 view 复用聚合工具的执行实现；授权和执行都使用带固定 discriminator 的 canonical input。
2. UI 保留轻量镜像，用于 parser、label、icon 和 renderer 路由，并用契约测试保证名称、renderer、icon、prompt 一致；历史恢复通过稳定 tool name 和 root renderer 映射兼容旧步骤。
3. `ToolResult` 携带结构化 `ToolErrorClass`。工具边界可把旧错误转换成分类，但 ReAct retry/nudge/未知结果判断只消费分类，不扫描错误文本。
4. HTTP client 关闭自动重定向；每一跳只允许 `http/https`，拒绝 userinfo、localhost/loopback、私网、link-local、云元数据地址和解析到受限地址的域名，并在配置存在时执行域名 allowlist。跨 origin 重定向移除认证/cookie 头。

## 影响

- 现有聚合工具和持久化 schema 不变；operation view 仍是模型/会话注册层的附加入口。
- 用户可以用工具级 `disabled_operations`、`allowed_paths` 和 HTTP `allowed_domains` 收窄能力；operation view 会继承聚合工具的安全边界。
- HTTP 本地 listener 测试使用显式测试策略，不改变生产默认拒绝规则。

## 验证与回滚

- 运行 `cargo fmt --all -- --check`、tools/agent 的契约和 HTTP 负向测试、workspace Rust 测试、UI check/test。
- 若需回滚，必须同时移除 operation view 的注册、UI 镜像、结构化错误字段和 HTTP 配置；不执行数据库迁移或部分回滚单个边界。
