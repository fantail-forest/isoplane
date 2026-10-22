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
