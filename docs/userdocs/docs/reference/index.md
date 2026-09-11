---
id: index
title: Reference
sidebar_label: Overview
---

# Reference

Look-up material. Every page here describes what something *is*, not what to
do with it; for that, see the [how-to guides](../how-to/index.md).

| Page | Covers |
|---|---|
| [Runtime architecture](architecture.md) | What runs where, on which ports, speaking which protocols |
| [Trust bundle](trust-bundle.md) | The public TLS root store guests read: provenance, versioning, rotation, staleness, the `[trust]` keys |

Configuration keys, command-line flags and SDK members are documented where
they are defined: `wispd::config::NodeConfig` and `wispd --help` for the
daemon, `wisp --help` for the command, and the doc comments of `Wisp.Sdk` for
the C# SDK.
