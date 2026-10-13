# 1150：按 IANA 特殊用途登记分类网络目标

## 状态

已采纳并实施。

## 背景

网络 destination classifier 只拒绝常见私有/loopback 地址，没有完整覆盖 IPv4/IPv6 的特殊用途范围与登记的可达例外，导致安全策略名称与实际拒绝集合不一致。

## 决定

- Common 维护纯地址分类逻辑，依据 [IANA IPv4 Special-Purpose Address Registry](https://www.iana.org/assignments/iana-ipv4-special-registry) 与 [IANA IPv6 Special-Purpose Address Registry](https://www.iana.org/assignments/iana-ipv6-special-registry) 覆盖特殊用途地址及 registry 可达例外。
- IPv6 的 IPv4-mapped 地址默认拒绝，但显式 loopback policy 仍允许 mapped loopback；IANA globally-reachable 的 `64:ff9b::/96` 保留可达性，同时按嵌入 IPv4 地址复用 IPv4 特殊用途策略。`2001:20::/28` 与 `2001:30::/28` 按完整前缀处理。
- 所有网络策略继续通过既有 Common destination owner 消费该分类；provider/MCP/Tools 不复制特殊地址表。
- 每类边界与可达例外使用单元测试锁定，避免大范围 CIDR 覆盖误拒绝登记的公共可达前缀。

## 影响

目标地址判定一致用于 DNS 解析与 IP literal 路径。无持久数据变化；策略拒绝仍为 fail closed。

## 验证

新增 IPv4、IPv6 special-purpose 与可达例外边界测试；运行 Common/workspace 测试和严格 Clippy。

## 回滚

无持久数据重置要求。任何地址表调整必须对照 IANA registry 和相邻前缀测试，不得恢复不完整的常见地址黑名单。
