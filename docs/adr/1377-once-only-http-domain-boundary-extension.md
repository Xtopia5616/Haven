# ADR 1377：HTTP 全局域名边界的一次性扩展

## 状态

已实施 — 2026-10-23。

## 背景

ADR 1376 已将 Files 越界写收敛为对精确目标的一次性确认，但全局 `ResourcePolicy.network.allowed_domains` 对 HTTP 的拒绝仍不可在本次操作中处理。用户只能改写已保存策略后再执行，或放弃当前调用。HTTP 已由可信工具声明 scheme、host、port，因此可以在保持同一授权 owner 的前提下把确认范围限制到单个 origin。

## 决定

1. 仅当 `http` 调用声明一个 `NetworkDestination`、公共网络访问已启用、且唯一不匹配项是全局域名列表时，Collaborative 模式可发出 `resource_boundary_extension` 确认。收据和 proof 继续绑定完整输入、精确 scheme/host/port、策略版本和期限。
2. 该扩展不覆盖工具级域名限制、工具或全局网络禁用、显式 deny、Analyze，也不允许 Unattended 模式继续。Opaque 网络能力、未声明/多目的地请求及其它资源类型继续 fail closed。
3. 扩展只用于当前调用，不生成 Session/Always grant，不修改配置。HTTP 执行上下文移除本次不匹配的全局域名组，同时保留工具级域名组；工具在发请求前再次校验 URL 的 origin 与 proof 绑定资源完全相同。
4. HTTP 始终禁用自动重定向并使用共享目标 resolver。SSRF 特殊地址拒绝、DNS pin、每跳目标校验和同源重定向限制不受扩展影响；扩展不能授权另一 scheme、host 或 port。

## 配置与数据影响

无配置、数据库 schema、session event 或 IPC shape 变化。既有 confirmation event 标记只表示一次性资源边界扩展；UI 展示由后端声明的 origin。不会持久化扩展或授权记录，因此无需重置数据。

## 替代方案

- 修改已保存 allowlist 后自动重试：拒绝；这会把一次操作改成后续调用的持续能力，并可能在副作用结果未知时重放。
- 将任意 HTTP 目标追加到工具级白名单：拒绝；会突破工具级资源策略的收窄职责。
- 继续把全局域名列表视为不可临时扩展：拒绝；用户必须为一次已声明且可验证的公共 origin 改写持久设置。

## 验收与限制

测试覆盖全局白名单外的精确 origin 确认、收据不能复用到另一个 origin、工具级域名限制拒绝、Analyze/Unattended 拒绝，以及 HTTP 执行上下文只允许已确认 origin。网络访问仍会暴露给用户确认的远端服务；DNS/响应体限制和 Windows 发布验收继续由现有门禁负责。
