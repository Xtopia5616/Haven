# 1146：限制 Skill 清单读取并统一目录身份

## 状态

已采纳并实施。

## 背景

Skills scanner 先按 metadata 长度校验 `SKILL.md`，随后无界读取；文件在 stat/read 之间增长时，实际读取与 decode 分配不再受配置上限约束。有效清单与无效清单诊断也分别去重，导致有效 manifest name 与另一目录的 invalid diagnostic name 可碰撞，破坏 Skills IPC 的唯一 name 契约。

## 决定

- 保留 metadata 快速拒绝，并在打开后的文件句柄上最多读取 `skills_max_md_bytes + 1` 字节；超限即跳过，不把增长后的内容交给 decoder/parser。
- 扫描完成后在同一大小写不敏感身份空间内收敛有效技能与 invalid diagnostic；有效条目保留，身份冲突的 invalid diagnostic 从列表省略。
- Renderer `SkillInfo` 不承载本地路径；Agent `skills_list` 使用 `SkillCatalogEntry` 作为 ADR 0878 的显式路径投影例外。
- 明确保留一个局部风险：canonical path containment 检查与 pathname open 之间仍有竞态，requirements fingerprint 与 pip 再按路径访问亦然。当前 Skill root 是用户控制的本地目录，同一安全主体并发改写不构成权限边界；Job Object 不提供 filesystem sandbox。若未来支持不可信共享写入目录，需用句柄锚定的 containment 与稳定 requirements snapshot 一并关闭该风险，不能声称现有 process containment 已覆盖它。

## 影响

SKILL.md 实际分配受硬上限约束；Skills list 不再因重复名称被 renderer fail-closed。无需数据库、配置或 IPC schema 迁移；Skills command wire path 删除由 ADR 1142 记录。

## 验证

新增 `max + 1` 有界读取与有效 Skill / invalid diagnostic 同名回归。运行 workspace Rust 测试、严格 Clippy 和 IPC drift 门禁。

## 回滚

无持久数据重置要求。回滚前须保留打开后实际读 cap，并保证有效技能和诊断共用身份空间。
