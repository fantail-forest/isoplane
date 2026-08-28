// @ts-check
// Docs-only Docusaurus site for the WISP proof of concept.
//
// Internal consumers only: nothing here is deployed, so `url`/`baseUrl` are
// set for a local `npm run serve` and the edit links point at the repo path
// rather than a forge URL.

import {themes as prismThemes} from 'prism-react-renderer';

/** @type {import('@docusaurus/types').Config} */
const config = {
  title: 'WISP',
  tagline: 'A WASM-based serverless platform where the isolation boundary is the sandbox',
  favicon: 'img/favicon.svg',

  // Nothing is deployed; `url` only feeds absolute links (canonical tags, the
  // sitemap). The dev and preview servers bind 0.0.0.0:3123 so the site is
  // reachable from another machine on the network, and webpack-dev-server's
  // host check is off by default in Docusaurus 3 (`allowedHosts: 'all'`), so
  // any Host header — an IP, a LAN name, a tunnel domain — is accepted.
  url: 'http://localhost:3123',
  baseUrl: '/',

  onBrokenLinks: 'throw',
  onBrokenAnchors: 'throw',

  i18n: {defaultLocale: 'en', locales: ['en']},

  markdown: {
    mermaid: true,
    hooks: {
      onBrokenMarkdownLinks: 'throw',
    },
  },
  themes: ['@docusaurus/theme-mermaid'],

  presets: [
    [
      'classic',
      /** @type {import('@docusaurus/preset-classic').Options} */
      ({
        docs: {
          path: 'docs',
          routeBasePath: '/',
          sidebarPath: './sidebars.js',
          // The docs live in the repo they document, so "edit this page" is a
          // path, not a URL. Left off rather than pointed somewhere wrong.
          editUrl: undefined,
          showLastUpdateTime: false,
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      }),
    ],
  ],

  themeConfig:
    /** @type {import('@docusaurus/preset-classic').ThemeConfig} */
    ({
      colorMode: {respectPrefersColorScheme: true},
      navbar: {
        title: 'WISP',
        items: [
          {type: 'doc', docId: 'tutorials/quickstart', label: 'Tutorials', position: 'left'},
          {type: 'doc', docId: 'how-to/index', label: 'How-to', position: 'left'},
          {type: 'doc', docId: 'reference/index', label: 'Reference', position: 'left'},
          {type: 'doc', docId: 'explanation/index', label: 'Explanation', position: 'left'},
        ],
      },
      footer: {
        style: 'dark',
        copyright:
          'WISP documentation.',
      },
      prism: {
        theme: prismThemes.github,
        darkTheme: prismThemes.dracula,
        additionalLanguages: ['bash', 'toml', 'rust', 'csharp', 'json', 'ini'],
      },
      tableOfContents: {minHeadingLevel: 2, maxHeadingLevel: 4},
    }),
};

export default config;
