# 0179：读路径与 provider 重试兼容边界

## 背景

Inbox 的历史/引用读取会经过 archive 整理入口。此前即使 archive 已经在字节上限内、没有新 envelope，读取也会创建临时文件、`sync_all` 并替换 archive。与此同时，流式规则触发的 guidance retry 新增了共享请求接口；只实现旧 `Vec` 请求接口的第三方 `LlmClient` 会失去这次 retry。

## 决定

- archive 只有在超出大小上限、加入新 envelope，或发现 archive 临时文件时才重写；读取路径继续持有同一 inbox 锁并先完成临时文件恢复。
- `LlmClient` guidance 共享接口默认 fail-closed；只有实现新共享边界的 provider 才参与 guidance retry。这样未升级的第三方 provider 不会在每次 retry 隐式复制 `Vec` 请求。
- transcript 事件批量上限、snapshot 混合 transcript/branch 导入和 Settings 性能指标下载均由回归测试固定行为。

## 替代方案

- 每次读取都重写 archive：实现简单，但会产生无意义的磁盘写放大。
- 为未升级的 provider 复制 `Vec` 请求作为兼容回退：虽然保留 retry 可用性，但会绕过共享快照的分配约束，并让新旧 adapter 的性能语义不一致。

## 影响

正常读取不再触发 archive 写入；恢复中的 `.tmp` 和超限旧 archive 仍会被整理。未升级 provider 会明确跳过 guidance retry，原生实现保持共享快照路径。

## 验证

- Rust：Inbox、Memory、Agent、LLM 相关单元/集成测试。
- UI：SettingsView 点击“导出性能指标”并验证 Blob、下载链接点击与 URL 回收。

## 回滚与重置

无需数据库或用户配置重置。回滚代码即可；archive 仍由现有字节上限和临时文件恢复规则约束。
