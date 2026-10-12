# ADR 1098：逐跳校验并固定不可信 HTTP 目标

## 状态

已接受（2026-10-13）

## 背景

HTTP 工具和 OpenAI 图片响应都可能根据外部 URL 发起请求。此前 HTTP 工具自行实现 DNS/IP 分类，图片下载则直接跟随 provider 返回的 URL 和重定向；两条路径的 SSRF 边界不一致。HTTP 工具还没有限制 native 调用者提交的 URL、请求头和 body，也不能在请求进行中响应取消。

## 决定

- `haven_common::network` 是不可信 HTTP 目标的共享校验 owner：只接受 HTTP(S)，拒绝 userinfo、metadata/local/special-use 地址，检查 DNS 的全部答案，并将选中的公网地址固定到连接；URL 最长 8 KiB。
- HTTP 工具复用共享校验和 allowlist 规则，关闭自动重定向、每跳复核，限制为 same-origin redirect；请求不使用代理，以免代理重新解析主机而绕过地址固定。请求最多 64 个 header、header 总量 32 KiB、body 1 MiB；取消会丢弃在途网络 future。
- 图片 URL 下载关闭自动重定向且不使用代理；每跳单独解析并固定地址，最多 10 跳。图片 GET 不附带 provider 凭据或 cookie，因此允许通过同一公网校验的跨 origin CDN 跳转。下载响应仍受 32 MiB 限额约束。
- 用户配置的 provider API endpoint 保持原有连接策略；本决定只约束由 HTTP 工具或 provider 响应提供的目标 URL。

## 影响

HTTP 请求不能再依赖环境代理访问目标；这是保持已校验 IP 连接语义所需的边界。需要代理支持时，必须增加可验证实际代理目标的专用策略，不能恢复隐式代理绕过 pin。HTTP 工具拒绝跨 origin redirect；图片 CDN 可跨 origin，但每个 hop 都需满足公网策略。

## 验证

Common 测试覆盖特殊用途地址、凭据、协议、allowlist 和通配符边界；HTTP 工具回归覆盖 native 入参限额、在途取消与 redirect 校验；LLM 回归覆盖图片 URL redirect 到 metadata 地址时在下一次连接前拒绝。适用门禁见本轮验证记录。

## 回滚

不能恢复图片 URL 的自动跟随或未固定 DNS 的不可信目标连接。若需要改变代理或跨 origin 策略，须先给出实际连接目标校验方案和负向 SSRF 回归。
