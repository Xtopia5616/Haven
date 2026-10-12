# ADR 1090：原子发布 Skill 配置与目录快照

## 状态

已接受（2026-10-13）

## 背景

`SkillRegistry::set_config` 先更新 root/allowlist，再扫描目录。扫描失败时新配置已可见，但旧 catalog 仍在使用，形成互不匹配的运行时状态。

## 决定

`set_config` 在发布配置前按候选 root、allowlist 和当前 limits 完成扫描；只有扫描成功，才在同一 write lock 下同时替换配置和目录快照。`refresh_from_disk` 也在同一锁定快照中读取配置、扫描并替换 catalog，避免并发 refresh 用旧配置覆盖新目录结果。

## 影响

扫描失败时旧 root、allowlist、skills、diagnostics 和 catalog version 全部保持不变。扫描期间 registry 查询短暂等待，不观察到混合状态。

## 验证

Skills 单测用普通文件作为无效目录，确认失败后所有可见快照和版本仍保持原值；运行 Skills crate 测试。

## 回滚

不得把配置字段提前发布到一次可能失败的磁盘扫描之前。
