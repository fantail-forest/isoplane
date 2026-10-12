// @ts-check
import {themes as prismThemes} from 'prism-react-renderer';

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
    docs: {
      path: 'docs', routeBasePath: '/', sidebarPath: './sidebars.js',
      async sidebarItemsGenerator({defaultSidebarItemsGenerator, ...args}) {
        const titles = new Map(args.docs.map((doc) => [doc.id, doc.title]));
        return linkCategoriesToTheirPages(await defaultSidebarItemsGenerator(args), titles);
      },
    },
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
