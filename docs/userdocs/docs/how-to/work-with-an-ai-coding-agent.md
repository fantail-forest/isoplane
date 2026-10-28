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

## Give the agent tools

The CLI is also a [Model Context Protocol](https://modelcontextprotocol.io)
server, so an agent can run the application and read what it reports through
tools instead of a terminal it has to watch. Register it with the agent you
use, from the repository root:

```bash
isoplane agent init --harness claude-code
```

[`isoplane agent init`](../reference/cli/agent.md#isoplane-agent-init) lists
the agents it knows and the file it writes for each; the agent then starts
`isoplane agent mcp` itself. The tools start, watch and stop an application
and read its events and logs, and check that the machine is set up;
[`isoplane agent mcp`](../reference/cli/agent.md#isoplane-agent-mcp) lists
them.

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
