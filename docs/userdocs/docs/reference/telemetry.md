---
sidebar_position: 8
mdx:
  format: md
---

# Telemetry

The `isoplane` CLI records anonymous usage data and sends it to Isoplane. It shows which commands are used, where they fail and on which platforms. Only the CLI does this: the SDK packages an application references, the build and the local runtime send nothing.

This page lists everything that is collected and how to turn it off. Its tables are generated from the definitions the CLI uses, for the source version used to build this documentation.

Release builds enable telemetry by default. Debug builds collect nothing unless a telemetry endpoint was configured when building. `isoplane telemetry status` says whether yours does.

## Turn it off

```bash
isoplane telemetry disable
```

This deletes the events waiting to be sent and this installation's identifier. `isoplane telemetry enable` turns telemetry back on with a new identifier, and `isoplane telemetry status` shows the current setting and identifier.

These environment variables change it for one shell, container or pipeline:

| Variable | Description |
| --- | --- |
| `ISOPLANE_TELEMETRY_OPTOUT` | Set to `1` or `true` to turn telemetry off. Nothing is recorded or sent while it is set. |
| `DO_NOT_TRACK` | Set to `1` or `true` to turn telemetry off, as it does for other tools that read this variable. |
| `ISOPLANE_TELEMETRY_DEBUG` | Set to `1` or `true` to print each event to standard error when a command ends. While it is set, nothing is recorded or sent, so it shows what a command would report without reporting it. |
| `ISOPLANE_TELEMETRY_TEST_RUN` | Isoplane.Testing sets this to `1` on the CLI it starts, so that a session a test starts is recorded as one. |

## When data is sent

The first command you run prints a notice and sends nothing, so you can opt out before any data leaves the machine. Where nobody would see the notice, nothing is recorded until a run that can show it. That is any run whose error output is redirected, unless it is on a continuous integration machine, where the notice goes to the log.

After that, each command writes one event to `telemetry/pending` in the Isoplane directory when it ends. That directory is `.isoplane` in your home directory unless `ISOPLANE_HOME` names another. Each later command sends the waiting events while it works. If it finishes while they are still being sent, it waits at most a quarter of a second, then leaves them for the next command. An event that could not be sent is deleted after 30 days, and at most 1,000 are kept.

## What is collected

One event for each command. `--help`, `--version`, `isoplane telemetry` and `isoplane completions` record nothing. A tool call a coding agent makes to `isoplane agent mcp` that does what a command does is recorded as that command, with `interface` set to `mcp`: `start_session` as `dev`, ending when the session does, and `check_machine` as `doctor`. The other tools record nothing.

| Field | What it records | Values |
| --- | --- | --- |
| `id` | A random identifier for this event, so that one sent twice is stored once. |  |
| `occurredAt` | When the command ended, in UTC, by this machine's clock. |  |
| `installId` | The random identifier of this installation, from telemetry/state.json in the Isoplane home directory. Absent on continuous integration machines. |  |
| `ci` | Whether the command ran on a continuous integration machine. |  |
| `ciProvider` | The continuous integration service the command ran on, or other for one the CLI does not recognise. Absent when it did not run on one. | `github-actions`, `azure-pipelines`, `gitlab`, `bitbucket`, `buildkite`, `circleci`, `travis`, `appveyor`, `drone`, `teamcity`, `jenkins`, `aws-codebuild`, `other` |
| `agent` | The coding agent that ran the command, when its environment identifies one. | `none`, `cowork`, `claude`, `cursor`, `gemini`, `copilot-cli`, `copilot-app`, `copilot-vscode`, `codex`, `aider`, `plandex`, `amp`, `qwen`, `droid`, `opencode`, `zed`, `kimi`, `openhands`, `goose`, `cline`, `roo`, `windsurf`, `replit`, `augment`, `antigravity`, `pi`, `other` |
| `interface` :since[next] | How the command was invoked: cli from a command line, and mcp as a tool call a coding agent made to the server `isoplane agent mcp` runs. | `cli`, `mcp` |
| `command` | The command that ran. A name the CLI does not recognise is recorded as unknown, never as typed. | `dev`, `debug-adapter`, `setup`, `uninstall-sdk`, `doctor`, `cache`, `bundle`, `prepare-runtime`, `agent`, `daemon`, `deploy`, `unknown` |
| `mode` | Why the command ran: test when Isoplane.Testing started it, debug for a debugging session, and run otherwise. | `run`, `debug`, `test` |
| `outcome` | How the command ended. | `succeeded`, `failed`, `cancelled`, `crashed` |
| `exitCode` | The exit code the command returned. Absent when the command crashed. |  |
| `failure` | The category of the error that ended the command, when one did. Error messages are never recorded. | `usage`, `invalid-data`, `invalid-operation`, `file-system`, `process`, `unexpected` |
| `durationMs` | How long the command ran, in milliseconds. |  |
| `cliVersion` | The version of the isoplane CLI. |  |
| `platform` | The operating system family and the processor architecture the CLI runs as, such as linux-x64 or osx-arm64. |  |
| `osArchitecture` | The machine's processor architecture, such as x64 or arm64. It differs from the platform's when the CLI runs under emulation: an x64 .NET under Rosetta 2 on an Apple silicon Mac, or on Windows on Arm. |  |
| `osVersion` | The operating system's version number; on Linux, the kernel's. |  |
| `distribution` | The Linux distribution's identifier from os-release, such as ubuntu. Absent on other systems and when the file is missing. |  |
| `distributionVersion` | The Linux distribution's version from os-release, such as 24.04. Absent on other systems and on distributions without one. |  |
| `runtimeVersion` | The version of the .NET runtime the CLI ran on. |  |
| `locale` | The language and region the CLI would display messages in, such as en-NZ. Absent when the machine sets none. |  |

One more event when the CLI creates an installation identifier: the first time a command is recorded, usually `isoplane setup`, and again after `isoplane telemetry disable` and `enable`.

| Field | What it records | Values |
| --- | --- | --- |
| `id` | A random identifier for this event, so that one sent twice is stored once. |  |
| `occurredAt` | When the identifier was created, in UTC, by this machine's clock. |  |
| `installId` | The new identifier, which the installation's command events carry from then on. |  |

The service that receives an event adds the time it arrived and the country the request came from. It does not keep the address the request came from.

`ci` is true, and `ciProvider` names the service, when one of these environment variables is set. The first in the table that is set decides.

| Variable | Recorded as |
| --- | --- |
| `GITHUB_ACTIONS`, when `true` or `1` | `github-actions` |
| `TF_BUILD`, when `true` or `1` | `azure-pipelines` |
| `GITLAB_CI`, when `true` or `1` | `gitlab` |
| `BITBUCKET_BUILD_NUMBER` | `bitbucket` |
| `BUILDKITE`, when `true` or `1` | `buildkite` |
| `CIRCLECI`, when `true` or `1` | `circleci` |
| `TRAVIS`, when `true` or `1` | `travis` |
| `APPVEYOR`, when `true` or `1` | `appveyor` |
| `DRONE`, when `true` or `1` | `drone` |
| `TEAMCITY_VERSION` | `teamcity` |
| `JENKINS_URL` | `jenkins` |
| `CODEBUILD_BUILD_ID` | `aws-codebuild` |
| `CI`, unless empty, `false` or `0` | `other` |

`agent` names the coding agent from these variables, following the Aspire CLI's list. The first row that matches decides.

| Variables | Recorded as |
| --- | --- |
| `CLAUDE_CODE_IS_COWORK` | `cowork` |
| `CLAUDECODE`, `CLAUDE_CODE`, `CLAUDE_CODE_ENTRYPOINT` | `claude` |
| `CURSOR_AI`, `CURSOR_TRACE_ID`, `CURSOR_AGENT` | `cursor` |
| `GEMINI_CLI` | `gemini` |
| `COPILOT_CLI`, `GITHUB_COPILOT_CLI_MODE`, `GH_COPILOT_WORKING_DIRECTORY`, `COPILOT_MODEL`, `COPILOT_ALLOW_ALL`, `COPILOT_GITHUB_TOKEN` | `copilot-cli` |
| `AI_AGENT`, set to `github_copilot_app_agent` | `copilot-app` |
| `AI_AGENT`, set to `github_copilot_vscode_agent` | `copilot-vscode` |
| `COPILOT_AGENT` | `copilot-vscode` |
| `CODEX_CLI`, `CODEX_SANDBOX`, `CODEX_CI`, `CODEX_THREAD_ID` | `codex` |
| `OR_APP_NAME`, set to `Aider` | `aider` |
| `OR_APP_NAME`, set to `plandex` | `plandex` |
| `AMP_HOME` | `amp` |
| `QWEN_CODE` | `qwen` |
| `DROID_CLI` | `droid` |
| `OPENCODE_AI` | `opencode` |
| `ZED_ENVIRONMENT`, `ZED_TERM` | `zed` |
| `KIMI_CLI` | `kimi` |
| `OR_APP_NAME`, set to `OpenHands` | `openhands` |
| `GOOSE_TERMINAL`, `GOOSE_PROVIDER` | `goose` |
| `CLINE_TASK_ID` | `cline` |
| `ROO_CODE_TASK_ID` | `roo` |
| `WINDSURF_SESSION` | `windsurf` |
| `REPL_ID` | `replit` |
| `AUGMENT_AGENT` | `augment` |
| `ANTIGRAVITY_AGENT` | `antigravity` |
| `PI_CODING_AGENT` | `pi` |
| `AGENT_CLI` | `other` |

Only whether a variable is set, or whether it has the value shown, is used; no variable's value is collected.

## What is never collected

- Project, application and team names
- File paths
- Command arguments and option values
- The values of environment variables
- Error messages
- Source code, and anything an application sends, receives or logs

<!-- Generated by tools/ApiReference. -->
