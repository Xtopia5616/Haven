# ADR 0864：直接使用 Common XML entity decoder

## 状态

Accepted — 2026-10-10

## 背景

`haven-app-binary::autostart` 声明了一个同名 `xml_unescape` 函数，只把参数转发给 `haven_common::encoding::xml_unescape`。Task Scheduler `<Command>` 解析和 Tools 的 CLIXML 消息解码实际共享同一实体解码语义；App wrapper 没有额外策略或状态，只增加了一个没有独立 owner 职责的符号。

## 决定

- App Autostart 在 Windows 构建下直接导入 `haven_common::encoding::xml_unescape`，删除同名 wrapper。
- Common 继续是 XML entity decoding 的唯一实现 owner；App 保留 `<Command>` / `<Arguments>` 提取、任务有效性和路径匹配策略。
- 保持实体替换顺序（`&amp;` 最后）、输入输出与 Windows 条件编译范围不变；无 IPC、配置、数据库和持久化变化，不需要重置。

## 替代方案

- 保留 wrapper 作为 App 别名：拒绝。其返回值、错误和行为均由 Common 决定，别名没有消费者可见的领域职责。
- 将 Task Scheduler XML 解析整体移动到 Common：拒绝。标签语义、启动参数和应用路径校验属于 App。

## 影响与验证

本次删除无行为转发函数，调用方直接引用共享 decoder。workspace 编译、测试目标编译及严格 Clippy 通过；测试套件未执行。当前环境为 Windows，可编译 Windows 条件分支；本次不改变 Task Scheduler 的实际系统交互。

## 回滚

若 App 未来需要与 CLIXML 不同的实体处理语义，应新增有明确规则的 App parser，而不是恢复同名转发 wrapper。当前无持久数据需要重置。
