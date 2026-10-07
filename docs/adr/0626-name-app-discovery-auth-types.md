# ADR 0626：命名 App model discovery 认证结果

## 状态

已采纳并实施。

## 背景

App 的 model discovery 认证路径将 provider header policy 表达为 `(header_name, prefix)`，再把 API key 和 `(header_name, header_value)` 嵌套成 tuple。STT discovery 与普通 provider discovery 都使用相同的 header policy 形状，但有不同的 scheme resolution 规则。含凭据的 tuple 也会自动获得 `Debug`，使断言或临时日志有意外显示 secret 的可能。

## 决定

1. provider/STT policy 使用 `AuthHeaderScheme { header_name, prefix }`。
2. 将 key 应用到方案后生成 LLM registry 拥有的 `ModelDiscoveryAuthHeader { header_name, value }`，并由 App 直接传给 `ModelRegistry`；该类型不实现 `Debug`。
3. discovery 入口使用 `ResolvedDiscoveryAuth { api_key, auth_header }`，其中 `auth_header` 可为空以表达显式无认证模式；该结果也不实现 `Debug`。
4. provider 与 STT 的 scheme resolution 规则保持各自独立；两条路径只在明确的 registry header 契约上汇合。
5. header 名称、prefix 拼接、key 与 URL 的绑定规则、`skip_auth` 行为，以及发往 LLM registry 的 HTTP 请求保持不变。

## 替代方案

- 把 provider 和 STT scheme resolver 合并成一个函数：拒绝，两条解析策略接受不同输入并有不同协议映射。
- 返回 header tuple，仅重命名解构变量：拒绝，policy、实际 header 与 API key 仍会在位置式字符串容器中混淆。
- 对含凭据结构派生 `Debug` 以方便 `assert_eq!`：拒绝，测试改为分别检查字段，避免差错输出自动包含 secret。

## 影响与验证

- 这是 App 与 LLM registry 的 Rust API 调整，没有 Tauri wire、配置或持久化变化。
- 命名审计 §5.7 保持 Active；其它 crate、UI、IPC、配置和持久名仍待逐域审计。
- 验证：workspace fmt、locked check、strict Clippy、workspace 单线程测试、ADR 索引及 staged diff 检查。

## 回滚

恢复 auth scheme 与 discovery 凭据 tuple，并同步 STT/provider resolver、registry 调用和测试；无需数据或 wire 迁移。
