---
title: Work with an AI coding agent
---

An Isoplane project gives a coding agent such as Claude Code, Codex, Cursor or
Copilot what it needs without setup on your part. This page says what that is,
where it comes from, and how to change it.

## What a project gives an agent

- **`AGENTS.md`** at the root of a project created from a
  [template](../reference/project-templates.md): the commands to run, test and
  debug the application, the rules of the sandbox in a few lines, and where to
  look next. Most agents read this file at the start of a session. It is yours
  to edit.
- **The `isoplane` skill**, which the build writes into `.agents/skills/` and
  `.claude/skills/` at the repository root. Its `SKILL.md` holds the rules of
  the sandbox and the commands, and its `references/` directory holds copies
  of these documentation pages and the SDK API reference for the packages the
  project uses, at the SDK version in use. An agent that supports
  [Agent Skills](https://agentskills.io/) loads it when the work concerns the
  application. The build keeps the skill current: a package added to the
  project adds its pages on the next build, and a new SDK version replaces
  them. Commit the two directories, so that an agent working from a clone has
  them.
- **Links from diagnostics.** Each `ISOPLANE` code the build or the editor
  reports links to its section of [Error codes](../reference/error-codes.md).
- **This documentation as markdown.** Append `.md` to any page's URL, or send
  `Accept: text/markdown`, to read it without the site around it.

An existing application gets the skill on its first build after it references
an Isoplane SDK package.

## Keep guidance of your own

The build overwrites the `isoplane` skill, so put instructions for your own
application in `AGENTS.md` or in a skill of your own beside it, such as
`.agents/skills/my-app/SKILL.md`. Both survive every build.

## Change where the skill is written, or stop writing it

Three [project settings](../reference/project-settings.md) control the skill:
[`IsoplaneWriteAgentSkill`](../reference/project-settings.md#isoplanewriteagentskill)
switches the writing off,
[`IsoplaneAgentSkillDirectories`](../reference/project-settings.md#isoplaneagentskilldirectories)
chooses the skills directories, and
[`IsoplaneAgentSkillRoot`](../reference/project-settings.md#isoplaneagentskillroot)
chooses the directory they are created under. A project created from a
template shows all three in its project file, commented out.
