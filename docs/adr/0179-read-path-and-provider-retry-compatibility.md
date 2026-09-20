# 0179：读路径与 provider 重试兼容边界

## 背景

Inbox 的历史/引用读取会经过 archive 整理入口。此前即使 archive 已经在字节上限内、没有新 envelope，读取也会创建临时文件、`sync_all` 并替换 archive。与此同时，流式规则触发的 guidance retry 新增了共享请求接口；只实现旧 `Vec` 请求接口的第三方 `LlmClient` 会失去这次 retry。

## 决定

- archive 只有在超出大小上限、加入新 envelope，或发现 archive 临时文件时才重写；读取路径继续持有同一 inbox 锁并先完成临时文件恢复。
- 原有 `LlmClient` guidance 共享接口保留一个兼容默认实现：复制一次 immutable 请求快照，追加 user guidance，再调用既有的 `chat_stream_with_tools_output_cap`。原生 provider 仍可覆盖该接口以避免复制。
- transcript 事件批量上限、snapshot 混合 transcript/branch 导入和 Settings 性能指标下载均由回归测试固定行为。

## 替代方案

- 每次读取都重写 archive：实现简单，但会产生无意义的磁盘写放大。
- 对未升级的 provider 直接 fail-closed：避免一次 `Vec` 分配，但 guidance retry 会按 adapter 是否升级而改变可用性。

## 影响

正常读取不再触发 archive 写入；恢复中的 `.tmp` 和超限旧 archive 仍会被整理。旧 provider 的 guidance retry 会有一次有界的 `Vec` 物化成本，原生实现不受影响。

## 验证

- Rust：Inbox、Memory、Agent、LLM 相关单元/集成测试。
- UI：SettingsView 点击“导出性能指标”并验证 Blob、下载链接点击与 URL 回收。

## 回滚与重置

无需数据库或用户配置重置。回滚代码即可；archive 仍由现有字节上限和临时文件恢复规则约束。
