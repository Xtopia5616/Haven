# ADR 0863：共用 managed path 比较原语

## 状态

Accepted — 2026-10-10

## 背景

App 的上传/生成媒体清理与 Tools 的媒体资产注册表各自实现了完全相同的两个纯路径谓词：比较两个路径是否相等，以及判断候选路径是否等于或位于根路径之下。它们在 Windows 上按不区分大小写的 lossy path text 比较，并要求子路径前有分隔符；其它平台用 `Path` 的组件语义。分散实现会让 App 删除文件前的保留判断与 Tools 注册表清理逐渐采用不同边界规则。

## 决定

- `haven_common::path` 唯一拥有 `path_is_equal` 与 `path_is_equal_or_child`，保留现有各平台算法和返回语义。
- 两个调用方使用这些谓词做 managed media 引用、租约和已删除目录下注册项的比较。
- Common 只拥有纯 lexical comparison，不访问文件系统、不解析链接、不 canonicalize 路径、不决定 root 或删除策略。App 继续拥有上传/生成媒体扫描和删除；Tools 继续拥有资产注册、授权访问、租约与 pruning。
- 不改路径来源、遍历范围、symlink/reparse 检查或 fail-closed 规则；无 IPC、配置、数据库和持久化变化，不需要重置。

## 替代方案

- 保留两个调用方副本：拒绝。算法完全相同，已在同一 cleanup lifecycle 中交叉使用，两份 owner 会让安全相关路径比较产生偏差。
- 将路径 canonicalization、symlink 检查或 managed root 策略移入 Common：拒绝。它们涉及文件系统与领域安全责任，分别由 Platform 和调用方拥有。

## 影响与验证

本次将两个相同 predicate 移至 Common，并保留 App 与 Tools 的所有清理/资产状态职责。workspace 编译、测试目标编译及严格 Clippy 通过；测试套件未执行。依赖真实文件系统行为的 Windows 生命周期验收仍由 Windows 发布 Gate 覆盖。

## 回滚

若某个调用方需要不同的比较语义，应为该语义定义独立、明确命名的 predicate，而不是复制 Common 当前算法并沿用相同名称。当前无持久数据需要重置。
