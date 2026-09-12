// @ts-check
import {themes as prismThemes} from 'prism-react-renderer';

/** @type {import('@docusaurus/types').Config} */
export default {
  title: 'Isoplane',
  tagline: 'Build and run your .NET application',
  favicon: 'img/favicon.svg',
  url: 'http://localhost:3123',
  baseUrl: '/',
  onBrokenLinks: 'throw',
  onBrokenAnchors: 'throw',
  i18n: {defaultLocale: 'en', locales: ['en']},
  markdown: {hooks: {onBrokenMarkdownLinks: 'throw'}},
  presets: [['classic', {
    docs: {path: 'docs', routeBasePath: '/', sidebarPath: './sidebars.js'},
    blog: false,
    theme: {customCss: './src/css/custom.css'},
  }]],
  themeConfig: {
    colorMode: {respectPrefersColorScheme: true},
    navbar: {title: 'Isoplane', items: [
      {type: 'doc', docId: 'index', label: 'Documentation', position: 'left'},
    ]},
    prism: {theme: prismThemes.github, darkTheme: prismThemes.dracula,
      additionalLanguages: ['bash', 'csharp', 'json', 'markup']},
  },
};
