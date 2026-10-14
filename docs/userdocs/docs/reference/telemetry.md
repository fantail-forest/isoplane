---
sidebar_position: 7
mdx:
  format: md
---

# Telemetry

The `isoplane` CLI records anonymous usage data and sends it to Isoplane. It shows which commands are used, where they fail and on which platforms. Only the CLI does this: the SDK packages an application references, the build and the local runtime send nothing.

This page lists everything that is collected and how to turn it off. Its tables are generated from the definitions the CLI uses, so they match the version you have installed.

A CLI built from source collects nothing. `isoplane telemetry status` says whether yours does.

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

After that, each command writes one event to `telemetry/pending` in the Isoplane directory when it ends. That directory is `.isoplane` in your home directory unless `ISOPLANE_HOME` names another. A later command that runs for more than two seconds sends the waiting events while it works, and no command waits for that before it exits. An event that could not be sent is deleted after 30 days, and at most 1,000 are kept.

## What is collected

One event for each command. `--help`, `--version` and `isoplane telemetry` record nothing.

| Field | What it records | Values |
| --- | --- | --- |
| `id` | A random identifier for this event, so that one sent twice is stored once. |  |
| `occurredAt` | When the command ended, in UTC, by this machine's clock. |  |
| `installId` | The random identifier of this installation, from telemetry/state.json in the Isoplane home directory. Absent on continuous integration machines. |  |
| `ci` | Whether the command ran on a continuous integration machine. |  |
| `agent` | The coding agent that ran the command, when its environment identifies one. | `none`, `claude`, `copilot`, `cursor`, `gemini` |
| `command` | The command that ran. A name the CLI does not recognise is recorded as unknown, never as typed. | `dev`, `debug-adapter`, `setup`, `uninstall-sdk`, `doctor`, `doctor-module`, `cache`, `bundle`, `unknown` |
| `mode` | Why the command ran: test when Isoplane.Testing started it, debug for a debugging session, and run otherwise. | `run`, `debug`, `test` |
| `outcome` | How the command ended. | `succeeded`, `failed`, `cancelled`, `crashed` |
| `exitCode` | The exit code the command returned. Absent when the command crashed. |  |
| `failure` | The category of the error that ended the command, when one did. Error messages are never recorded. | `usage`, `invalid-data`, `invalid-operation`, `file-system`, `process`, `unexpected` |
| `durationMs` | How long the command ran, in milliseconds. |  |
| `cliVersion` | The version of the isoplane CLI. |  |
| `platform` | The operating system family and the processor architecture the CLI runs as, such as linux-x64 or win-arm64. |  |
| `osArchitecture` | The machine's processor architecture, such as x64 or arm64. It differs from the platform's when the CLI runs under emulation: an x64 .NET under Rosetta 2 on an Apple silicon Mac, or on Windows on Arm. |  |
| `osVersion` | The operating system's version number; on Linux, the kernel's. |  |
| `distribution` | The Linux distribution's identifier from os-release, such as ubuntu. Absent on other systems and when the file is missing. |  |
| `distributionVersion` | The Linux distribution's version from os-release, such as 24.04. Absent on other systems and on distributions without one. |  |
| `runtimeVersion` | The version of the .NET runtime the CLI ran on. |  |
| `locale` | The language and region the CLI would display messages in, such as en-NZ. Absent when the machine sets none. |  |

The service that receives an event adds the time it arrived and the country the request came from. It does not keep the address the request came from.

`ci` is true when one of these environment variables is set: `CI`, `TF_BUILD`, `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `CIRCLECI`, `TRAVIS`, `APPVEYOR`, `TEAMCITY_VERSION`, `JENKINS_URL`, `CODEBUILD_BUILD_ID`. `agent` is read from these: `CLAUDECODE`, `GITHUB_COPILOT_CLI_MODE`, `CURSOR_EDITOR`, `GEMINI_CLI`. Only whether a variable is set is used; its value is not collected.

## What is never collected

- Project, application and team names
- File paths
- Command arguments and option values
- The values of environment variables
- Error messages
- Source code, and anything an application sends, receives or logs

<!-- Generated by tools/ApiReference. -->
