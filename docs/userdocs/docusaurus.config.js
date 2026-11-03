// @ts-check
import {readFileSync} from 'node:fs';
import {parseEnv} from 'node:util';
import {themes as prismThemes} from 'prism-react-renderer';
import {visitorPlatformExpression} from './src/platform/preference.ts';
import since from './src/remark/since.js';
import markdownPages from './src/plugins/markdown-pages/index.mjs';

// A nonblank DOCS_GTM_CONTAINER_ID in this site's .env enables optional GTM.
let siteEnvironment = {};
try {
  siteEnvironment = parseEnv(readFileSync(new URL('.env', import.meta.url), 'utf8'));
} catch (error) {
  if (error.code !== 'ENOENT') throw error;
}
const gtmContainerId = siteEnvironment.DOCS_GTM_CONTAINER_ID?.trim();

// The search box queries Typesense through the docs Worker at /docs/search,
// which holds the real key; the site sends a placeholder. A local site can
// point at a local server instead: see search/serve.sh and search/start-site.sh.
const searchUrl = new URL(process.env.DOCS_SEARCH_URL ?? 'https://isoplane.dev/docs/search');
const searchKey = process.env.DOCS_SEARCH_KEY ?? 'proxied';
// Hybrid search needs the embedding field that search/build-index.sh indexes
// unless DOCS_SEARCH_EMBEDDINGS=off there; this keeps the two in step.
const searchEmbeddings = process.env.DOCS_SEARCH_EMBEDDINGS !== 'off';
const searchFields = ['hierarchy.lvl0', 'hierarchy.lvl1', 'hierarchy.lvl2', 'hierarchy.lvl3',
  'hierarchy.lvl4', 'hierarchy.lvl5', 'hierarchy.lvl6', 'content'];

// The release lines with a snapshot under versioned_docs, newest first, as
// scripts/snapshot.mjs maintains them. The newest is the site's default.
const releaseLines = JSON.parse(readFileSync(new URL('versions.json', import.meta.url), 'utf8'));
// `docs/` describes the next release. A local server shows it as "Next"; the
// published site shows only the snapshots.
const includeNext = process.env.NODE_ENV !== 'production';

// A page beside a directory of the same name is that directory's landing page.
// Docusaurus lists the two as siblings, so link the directory's category to the
// page and drop the page's own entry.
function linkCategoriesToTheirPages(items, titles) {
  const name = (item) => item.id.split('/').at(-1);
  const categories = new Set(items.filter((item) => item.type === 'category').map((item) => item.label));
  const pages = new Map(items.filter((item) => item.type === 'doc' && categories.has(name(item)))
    .map((item) => [name(item), item.id]));
  return items.filter((item) => item.type !== 'doc' || !pages.has(name(item))).map((item) => {
    if (item.type !== 'category') return item;
    const id = pages.get(item.label);
    return {
      ...item,
      ...(id && {label: titles.get(id), link: {type: 'doc', id}}),
      items: linkCategoriesToTheirPages(item.items, titles),
    };
  });
}

// Code keeps the Prism theme's syntax colours but sits on the site's own surface.
const onBrandSurface = (theme) => ({
  ...theme,
  plain: {color: 'var(--foreground)', backgroundColor: 'var(--surface)'},
});

const baseUrl = '/docs/';

/** @type {import('@docusaurus/types').Config} */
export default {
  title: 'Isoplane',
  tagline: 'Build and run your .NET application',
  favicon: 'img/favicon.ico',
  headTags: [
    {tagName: 'link', attributes: {
      rel: 'icon', href: `${baseUrl}img/favicon.svg`, sizes: 'any', type: 'image/svg+xml',
    }},
    // Names the visitor's operating system on the root element before the page
    // paints, so a page with per-platform content shows only theirs.
    {tagName: 'script', attributes: {},
      innerHTML: `document.documentElement.dataset.os=${visitorPlatformExpression};`},
  ],
  url: 'https://isoplane.dev',
  baseUrl,
  // Cloudflare serves `how-to/install.html` at `/docs/how-to/install` and redirects the
  // slashed form to it, so links and canonical URLs leave the slash off.
  trailingSlash: false,
  noIndex: false,
  onBrokenLinks: 'throw',
  onBrokenAnchors: 'throw',
  i18n: {defaultLocale: 'en', locales: ['en']},
  markdown: {mermaid: true, hooks: {onBrokenMarkdownLinks: 'throw'}},
  themes: ['@docusaurus/theme-mermaid', 'docusaurus-theme-search-typesense'],
  // Every page is also built as Markdown beside its HTML; the Worker in
  // wrangler.jsonc serves that form to a client that asks for it.
  plugins: [markdownPages],
  presets: [['classic', {
    docs: {
      path: 'docs', routeBasePath: '/', sidebarPath: './sidebars.js',
      includeCurrentVersion: includeNext,
      lastVersion: releaseLines[0],
      versions: {
        ...(includeNext && {current: {label: 'Next', path: 'next', banner: 'unreleased'}}),
        ...Object.fromEntries(releaseLines.map((line) => [line, {label: `${line}.x`}])),
      },
      beforeDefaultRemarkPlugins: [since],
      async sidebarItemsGenerator({defaultSidebarItemsGenerator, ...args}) {
        const titles = new Map(args.docs.map((doc) => [doc.id, doc.title]));
        return linkCategoriesToTheirPages(await defaultSidebarItemsGenerator(args), titles);
      },
    },
    blog: false,
    ...(gtmContainerId ? {googleTagManager: {containerId: gtmContainerId}} : {}),
    theme: {customCss: './src/css/custom.css'},
  }]],
  themeConfig: {
    colorMode: {respectPrefersColorScheme: true},
    navbar: {items: [
      {type: 'doc', docId: 'index', label: 'Docs', position: 'left'},
      {type: 'custom-siteLink', href: '/blog', label: 'Blog', position: 'left'},
      {type: 'docsVersionDropdown', position: 'right'},
      {href: 'https://github.com/fantail-forest/isoplane', label: 'GitHub', position: 'right'},
      {type: 'doc', docId: 'quickstart', label: 'Get started', position: 'right',
        className: 'navbar__cta'},
    ]},
    footer: {copyright: '© 2026 Fantail Forest, Ltd. · A New Zealand limited company.'},
    typesense: {
      typesenseCollectionName: 'isoplane-docs',
      typesenseServerConfig: {
        nodes: [{
          host: searchUrl.hostname,
          port: Number(searchUrl.port || (searchUrl.protocol === 'https:' ? 443 : 80)),
          protocol: searchUrl.protocol.replace(':', ''),
          path: searchUrl.pathname.replace(/\/$/, ''),
        }],
        apiKey: searchKey,
      },
      typesenseSearchParameters: searchEmbeddings ? {
        query_by: [...searchFields, 'embedding'].join(','),
        vector_query: 'embedding:([], k: 10, distance_threshold: 1.0, alpha: 0.3)',
      } : {query_by: searchFields.join(',')},
      // Results come from the release line being read.
      contextualSearch: true,
    },
    prism: {
      theme: onBrandSurface(prismThemes.gruvboxMaterialLight),
      darkTheme: onBrandSurface(prismThemes.gruvboxMaterialDark),
      additionalLanguages: ['bash', 'csharp', 'json', 'markup', 'powershell'],
    },
  },
};
