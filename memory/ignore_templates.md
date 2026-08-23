---
name: 忽略 templates 目录
description: 用户要求完全不关注 templates 目录下的任何内容
type: feedback
---

不要关注 `templates/` 目录下的任何内容，所有针对该目录的文件操作、代码审查、修改建议都应跳过。

**Why:** 用户明确指示该目录无关紧要，无需关注。

**How to apply:** 在读取文件、搜索代码、审查变更时，主动跳过 `templates/` 路径。如果该目录中的文件出现在 git status 或搜索结果中，直接忽略。
