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
`isoplane-docs`. `pnpm run deploy:prod` builds the site, builds its search index,
deploys the search server and uploads the site; see [Search](#search).

The site permits indexing and generates a sitemap at `/docs/sitemap.xml`.

Every page is also built as Markdown beside its HTML, at the page's route plus
`.md` (`index.md` for the docs root and a version root), by the plugin in
`src/plugins/markdown-pages`. It renders the page's source after the build:
imports, platform blocks, partials, admonitions and release markers become
plain text, and links become absolute. The Worker script, `worker/index.mjs`,
serves that file to a request whose `Accept` header ranks `text/markdown`
above `text/html`, with `Vary: Accept` and `X-Robots-Tag: noindex`; other
requests get the asset as built, HTML with `Vary: Accept`. Each page links its
Markdown form from its head and from a row of actions above its title.
`static/llms.txt`, served at `/docs/llms.txt`, states these conventions for an
agent and points at the sitemap. `pnpm test` runs the unit tests of the
negotiation and the conversion; a local server has no Markdown pages, since the
plugin only runs in a build.

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
`pnpm run build` clears the Docusaurus cache first: a build made after a local
server or a dev-mode build would otherwise keep the "Next" menu entry.

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

## Search

The search box is [Typesense DocSearch](https://typesense.org/docs/guide/docsearch.html):
`docusaurus-theme-search-typesense` in the site, a Typesense server on Fly.io,
and the DocSearch scraper to index one into the other. Searches are hybrid:
keyword matching plus an embedding of each section, computed by the model named
in `search/scraper.config.json` through its provider's API: Qwen3-Embedding-8B
served by Fireworks, with the model's instruction template as the query prefix.
Nothing under `search/` is reachable from the site except through the Worker.

The search bar itself is ejected into `src/theme/SearchBar` so that typing is
debounced, a refused or failed search is retried after the pause its
`Retry-After` names, prefixes under
four characters are keyword-only, and each page load can tag its searches with
a random in-memory id that the search log records as `session`. That tag is
disabled (`SEARCH_SESSION_ENABLED`) pending a consent implementation.

`pnpm run search:index` (`search/build-index.sh`) runs after `pnpm run build`.
It starts a throwaway Typesense and an nginx serving `build/` in Docker, crawls
the built site into it, creates the one search-only key the server will answer,
snapshots the result into `search/snapshot/` and tears everything down. The
server is deployed as that snapshot baked into an image (`search/Dockerfile`,
`search/fly.toml`) in which Typesense listens on loopback and an nginx on the
machine's port admits only `POST /multi_search`, with the search-only key and
one query per call, and `GET /health`; every other route is 404 before it
reaches Typesense. nginx also limits each client address, so the machine is
protected even from traffic that bypasses Cloudflare. Production therefore has
no write path, no volume and no admin key in use. The two share a machine
because the index is small; should search ever need more than one machine,
nginx moves to its own, in front of Typesense machines on Fly's private network. `pnpm run search:deploy` (`search/deploy.sh`)
deploys the image to Fly and then the site to Cloudflare, with the snapshot's
version as the edge cache key. The Fly machine suspends when idle and resumes
on the next search.

`search/worker.js` is the site's Worker. Pages are static assets; requests under
`/docs/search` are proxied to the Fly server with the search-only key, rate
limited per client address, cached at the edge per distinct query, and logged
as `docs_search` events (query, hit count, status, cache, session) in Workers Logs.
A refusal or outage is passed back to the search box with its `Retry-After`
rather than retried in the Worker, which would queue requests out of the box's
sight and multiply load on the server. The embedding provider is the least
dependable link, so a query it fails to embed within three seconds is asked
again by keyword alone, answered uncached and logged with `mode: keyword`.

Configuration lives in `search/.env`, which Git ignores; copy
`search/.env.example` and fill it in. The Worker's own settings are in
`wrangler.jsonc`; it needs no `.dev.vars`.

`search/deploy.sh` sets the secrets from `search/.env` over stdin, so no secret
is typed into a shell: `DOCS_SEARCH_KEY` on Fly, for nginx to admit, and on the
Worker; and the server's own admin key, `TYPESENSE_API_KEY`, generated on the
first deploy and never used by the site. Searches at query time embed the
query with the same provider, so the server holds the provider key too, inside
the baked snapshot. To change model or provider, edit `model_config` in
`search/scraper.config.json`: any OpenAI-compatible provider works as
`openai/<model id>` with its base `url`, and `query_prefix` carries whatever
instruction the model wants in front of a query. Then index and deploy.
Documents and queries must be embedded by the same provider at the same
precision, so a provider change is a reindex, not a redeploy.

The first deploy needs only `fly auth login` and `wrangler login`; it creates
the Fly app itself. Changing `DOCS_SEARCH_KEY` in `search/.env` is a reindex
and a deploy, nothing more.

To run the search box against a local server: `pnpm run search:serve` runs the
production image from `search/snapshot/`, and `pnpm run search:start` points a
local site at it with the key from `search/.env`.
