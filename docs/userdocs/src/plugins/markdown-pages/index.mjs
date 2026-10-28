import {mkdirSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {markdownPath, pageRoute} from '../../markdown-path.mjs';
import {renderPage} from './convert.mjs';

/**
 * Writes each published doc page as Markdown beside its HTML, at the page's
 * route plus `.md` (`index.md` for a directory route), for readers that fetch
 * pages rather than render them. Runs after the build, from the docs plugin's
 * loaded content, so every version the site publishes is covered.
 */
export default function markdownPages() {
  return {
    name: 'markdown-pages',
    async postBuild({plugins, outDir, siteConfig, siteDir}) {
      const docs = plugins
        .filter((plugin) => plugin.name === 'docusaurus-plugin-content-docs')
        .flatMap((plugin) => plugin.content.loadedVersions)
        .flatMap((version) => version.docs);
      const sourceOf = (doc) => join(siteDir, doc.source.replace(/^@site\//, ''));
      const routeOf = (doc) => pageRoute(doc.permalink, siteConfig);
      const routes = new Map(docs.map((doc) => [sourceOf(doc), routeOf(doc)]));
      const site = {
        url: siteConfig.url,
        permalinkOf: (file) => routes.get(file),
        warn: (message) => console.warn(`[markdown-pages] ${message}`),
      };
      for (const doc of docs) {
        const markdown = renderPage(sourceOf(doc), {
          title: doc.title,
          description: doc.frontMatter.description,
          canonical: siteConfig.url + routeOf(doc),
          format: doc.frontMatter.mdx?.format,
        }, site);
        const file = join(outDir, markdownPath(routeOf(doc)).slice(siteConfig.baseUrl.length));
        mkdirSync(dirname(file), {recursive: true});
        writeFileSync(file, markdown);
      }
      console.log(`[markdown-pages] Wrote ${docs.length} Markdown pages.`);
    },
  };
}
