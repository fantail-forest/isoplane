# Isoplane customer guides

Customer-facing documentation for the Isoplane SDK and tools.

Google Tag Manager is optional. Set `DOCS_GTM_CONTAINER_ID` in a `.env` file
beside `docusaurus.config.js` to include it. An absent file or blank value
leaves it out; Git ignores the file. Rebuild after changing the value. Requires
Node.js 20.12 or newer in the 20.x line, or Node.js 21.7 or newer.

```bash
cd docs/userdocs
pnpm install --frozen-lockfile
pnpm start        # http://localhost:3123/docs/
pnpm run build
pnpm run serve
```

The servers bind loopback by default. Pass `--host 0.0.0.0` to make a local
preview accessible from another machine. Nothing is published by these commands.

## Publishing

The site is served at `https://isoplane.dev/docs/` by its own Cloudflare Worker,
`isoplane-docs`. `pnpm run deploy:prod` builds the site and uploads it.

The site permits indexing and generates a sitemap at `/docs/sitemap.xml`.

The site takes its palette from `docs/userdocs/src/css/tokens.css` and uses
root-relative addresses for its wordmark and Blog links.
The site honors the visitor’s light or dark and operating-system preferences.
The operating-system choice comes from `docs/userdocs/src/platform/preference.ts`;
pages show per-platform content with `src/components/Platform.js`.

Follow [the Diátaxis opinion](../opinion-registry/frameworks/docusaurus.md):
keep tutorials, how-to guides, generated reference and explanation distinct.
The sidebar groups pages by these needs; each page has one primary purpose. Run
tutorial commands against installed packages from a fresh directory before claiming that the workflow works. Keep
internal paths, repository build steps and unvalidated future commands out of
the guide. Both links and anchors are checked by the static build.

Starting or building the site generates the local limits table through Task and
Cargo. Building also checks the retained SDK reference against the .NET assemblies.

## Versions

`docs/` describes the next release and is never published: a local server shows
it as "Next", and the built site leaves it out. What is published are the
snapshots under `versioned_docs/`, one per release line. A line begins with a
breaking release (`0.1`, `0.2`, then `1`, `2`); the newest line is served at
`/docs/` and the others at `/docs/<line>/`, with a version menu in the navbar.

Every release, breaking or not, re-snapshots its line from `docs/`:

```bash
pnpm run snapshot          # the version in Directory.Build.props
pnpm run snapshot 0.1.3
```

The script stamps the release into `since.json` and regenerates the reference,
resolves release markers, replaces the line's snapshot, and leaves the result
to commit with the release. Nothing under `versioned_docs/` is edited by hand;
a fix to a published line is a change to `docs/` and a new patch release.

### Release markers

Write `:since[next]` after the name of anything added since the last release,
in a table cell or in a heading below the page title. It renders as a small
pill. A snapshot turns it into `:since[0.1.3]`, and drops a marker for a release
no later than the line's first, since every reader of that line has the item.
Generated reference pages mark their own items from `since.json`, which records
the release that added each documented type, member, overload, error code,
setting, template option, launch key, telemetry field and debugger attribute.
