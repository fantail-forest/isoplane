// @ts-check
import {readFileSync} from 'node:fs';
import {parseEnv} from 'node:util';
import {themes as prismThemes} from 'prism-react-renderer';
import {visitorPlatformExpression} from './src/platform/preference.ts';

// Set DOCS_GTM_CONTAINER_ID in .env beside this config to enable the tag;
// an absent file or blank value leaves it out.
let siteEnvironment = {};
try {
  siteEnvironment = parseEnv(readFileSync(new URL('.env', import.meta.url), 'utf8'));
} catch (error) {
  if (error.code !== 'ENOENT') throw error;
}
const gtmContainerId = siteEnvironment.DOCS_GTM_CONTAINER_ID?.trim();

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

// The marketing Worker serves the rest of this origin; see wrangler.jsonc.
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
  // Cloudflare serves `install.html` at `/docs/install` and redirects the
  // slashed form to it, so links and canonical URLs leave the slash off.
  trailingSlash: false,
  // Unlisted until launch: every page asks crawlers not to index it and no
  // sitemap is generated. Nothing on the marketing site links here either.
  noIndex: true,
  onBrokenLinks: 'throw',
  onBrokenAnchors: 'throw',
  i18n: {defaultLocale: 'en', locales: ['en']},
  markdown: {mermaid: true, hooks: {onBrokenMarkdownLinks: 'throw'}},
  themes: ['@docusaurus/theme-mermaid'],
  presets: [['classic', {
    docs: {
      path: 'docs', routeBasePath: '/', sidebarPath: './sidebars.js',
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
    ]},
    footer: {copyright: '© 2026 Fantail Forest, Ltd. · A New Zealand limited company.'},
    prism: {
      theme: onBrandSurface(prismThemes.gruvboxMaterialLight),
      darkTheme: onBrandSurface(prismThemes.gruvboxMaterialDark),
      additionalLanguages: ['bash', 'csharp', 'json', 'markup', 'powershell'],
    },
  },
};
