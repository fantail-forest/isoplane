# `docs/userdocs` — the WISP user documentation site

A [Docusaurus](https://docusaurus.io) site, structured with
[Diátaxis](https://diataxis.fr): tutorials, how-to guides, reference,
explanation. Internal consumers only — it is not deployed anywhere, and
`docusaurus.config.js` points `url`/`baseUrl` at a local `npm run serve`.

```bash
cd docs/userdocs
npm install
npm start          # dev server with hot reload, http://localhost:3123
npm run build      # static build into ./build
npm run serve      # serve ./build, same host and port
```

Both servers bind **`0.0.0.0:3123`**, so the site is reachable from another
machine on the network, and both accept **any `Host` header** — an IP, a LAN
name, a tunnel domain. Docusaurus 3's dev server sets `allowedHosts: 'all'`
itself, so nothing extra is configured for that; the host and port live in the
`start` and `serve` scripts in `package.json`.

There is no auth in front of either, so treat `npm start` as "anyone who can
reach this box can read these docs" and pick the interface accordingly:

```bash
npm start -- --host 127.0.0.1     # loopback only, when that matters
```

## What goes where

| Directory | Diátaxis mode | Rule of thumb |
|---|---|---|
| `docs/tutorials/` | Learning | A guided walkthrough that ends with something working. Tells you exactly what to type. |
| `docs/how-to/` | Task | A recipe for someone who already knows what they want. |
| `docs/reference/` | Information | What something *is*: flags, ports, record layouts, limits. No narrative. |
| `docs/explanation/` | Understanding | Why the system is shaped this way. No commands to run. |

Mixing the modes is what makes documentation unreadable, so resist the urge to
explain in a reference page or to enumerate flags in an explanation.

## Guard rails

`onBrokenLinks` and `onBrokenAnchors` are both `throw`, and so is
`onBrokenMarkdownLinks`. A build failure on a renamed heading is the point:
these documents cross-reference heavily, and a silently dead anchor is how a
reader ends up believing the wrong page.

Cross-document links are relative file paths ending in `.md` — Docusaurus
resolves them, and the build catches a typo.

## Language toggles

Any page that shows the same thing in more than one guest language uses a
tabbed toggle rather than stacking both. Readers should not have to skim past a
language they do not use.

The page must be `.mdx`, and every tab group on every page uses the **same
`groupId`**, `guest-language`:

```mdx
import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';

<Tabs groupId="guest-language">
<TabItem value="csharp" label="C#" default>

```csharp
// …
```

</TabItem>
<TabItem value="rust" label="Rust">

```rust
// …
```

</TabItem>
</Tabs>
```

That shared `groupId` is what makes the choice sticky: Docusaurus writes it to
`localStorage` under `docusaurus.tab.guest-language`, switches every group on
the page at once, and restores it on any other page using the same id. Use a
different id and the reader has to choose again on every page.

**C# is always the first tab and carries `default`.** .NET is the target
ecosystem, so it is what a reader who expresses no preference should see.

Put language-specific *prose* inside the `TabItem` too, not just the code — a
paragraph explaining a Rust-only detail is exactly the noise the toggle exists
to remove.

## Keeping it honest

This site documents the prototype as it is, and it must **stand on its own**. A
reader has not read `spec.md` and should never be told to. Where a page needs
knowledge that currently lives only in the specification, put that knowledge
here, in a form a user can act on — do not cite a section number and move on.

Two rules follow from that:

- **No `§` references and no "the specification says".** State the thing.
- **Never copy a generated number into this site.** `docs/findings.md` and
  `docs/bench/results.md` are generated from real runs and are authoritative.
  Link to them, or describe how to produce the figure yourself. A transcribed
  number goes stale silently, which is the failure mode this rule exists to
  prevent.

When you need to check a claim before writing it down, the tree is the source:
`crates/wisp-abi` for the wire contract and the capability list,
`guest-sdks/dotnet` and `guest-sdks/rust/src` for the SDK surfaces,
`crates/wispd/src/config.rs` for node configuration, and the CLI sources for
flags. Read the code, then write what a user needs to know.
