---
title: Work with an AI coding agent
---

A coding agent such as Claude Code, Codex, Cursor or Copilot writes better
code for an Isoplane application when it knows the rules of the sandbox. This
page describes what gives an agent those rules, how an existing project gets
it, and how to keep it current.

## What a project gives an agent

- **`AGENTS.md`** at the root of a project created from a
  [template](../reference/project-templates.md): the commands to run, test and
  debug the application, the rules of the sandbox in a few lines, and where to
  look next. Most agents read this file at the start of a session. It is yours
  to edit.
- **The `isoplane` skill**, a directory named `isoplane` in the project's
  skills directories, which are where a coding agent looks for skills. Its
  `SKILL.md` holds the rules of the sandbox and the commands for the SDK
  version the project uses, and links to the markdown form of these
  documentation pages and the SDK API reference for the Isoplane packages the
  project uses. An agent that supports
  [Agent Skills](https://agentskills.io/) loads the skill when the work
  concerns the application. A project created from a template starts with the
  skill. An existing project gets it from
  [`isoplane skills install`](#add-the-skill-to-an-existing-project).
- **Links from diagnostics.** Each `ISOPLANE` code the build or the editor
  reports links to its section of [Error codes](../reference/error-codes.md).
- **This documentation as markdown.** Append `.md` to any page's URL, or send
  `Accept: text/markdown`, to read it without the site around it.

## Choose the skills directories

The skill can go into two skills directories:

- `.claude/skills`, for Claude Code.
- `.agents/skills`, for agents that follow the Agent Skills convention.

A template writes the skill into both. To leave one out, create the project
with `--dot-claude false` or `--dot-agents false`:

```bash
dotnet new isoplane-webapi --name MyApi --dot-claude false
```

## Add the skill to an existing project

The skill comes from the Isoplane packages a project references, so the
project needs a reference to an Isoplane SDK package first. Then run the
command in the project directory. It restores the project before it reads the
packages, as `dotnet build` does:

```bash
isoplane skills install
```

The command asks which skills directories to use. `--dot-agents` and
`--dot-claude` answer in advance. A script or an agent has to pass them,
because the command can't ask without a terminal:

```bash
isoplane skills install --dot-agents --dot-claude
```

`--skills-path` names a skills directory directly, such as one that is not
under the repository root. The command writes the skill into its `isoplane`
directory and doesn't look for a copy elsewhere:

```bash
isoplane skills install --skills-path ../.claude/skills
```

The skill it writes matches the project's SDK version and covers the Isoplane
packages the project references. It goes under the nearest directory above the
project that holds a `.git` entry or a solution file, which is where an agent
usually opens the code. When no directory above the project holds either, it
goes under the project directory. Commit what the command wrote, so that an
agent working from a clone has the skill.

[`isoplane skills install`](../reference/cli/skills.md#isoplane-skills-install)
lists the command's options.

## Update the skill

The skill describes the SDK version and the packages the project had when the
skill was written. After you change the SDK version, or add or remove an
Isoplane package, write the skill again with `--force`:

```bash
isoplane skills install --force --dot-agents --dot-claude
```

The command writes the skill where it already is. For each skills directory
you choose, it looks in the project directory and each directory above it up
to the repository root, so the copy a template wrote beside the project is the
one it updates. A skills directory that holds the skill in none of them gets
it where a first install puts it. When the command finds a skills directory's
copy in more than one of those directories, it writes nothing and lists them.
Remove the copies you don't want, then run it again.

Without `--force`, the command writes nothing when a `SKILL.md` it would
create already exists, and it lists the files it found. With `--force`, it
overwrites them. The command writes only `SKILL.md`, so other files in the
`isoplane` directory, such as the `references` directory an earlier SDK
version wrote, stay where they are until you delete them. Leave the `isoplane`
directory itself in place, because that is how the command finds the copy to
write.

## Keep guidance of your own

`--force` overwrites edits made to the skill's `SKILL.md`. Put instructions for
your own application in `AGENTS.md`, or in a skill of your own beside the
`isoplane` skill, such as `.agents/skills/my-app/SKILL.md`. The command writes
inside the `isoplane` directories and nowhere else.

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
