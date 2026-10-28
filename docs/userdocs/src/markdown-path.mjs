/**
 * The route a page is served at: its permalink with the site's `trailingSlash`
 * setting applied, as Docusaurus applies it to routes and canonical links. A
 * doc's permalink may keep a slash that its route loses. The base URL keeps
 * its slash either way.
 */
export function pageRoute(permalink, {baseUrl, trailingSlash}) {
  if (trailingSlash === undefined || permalink === baseUrl || permalink === '/') return permalink;
  return trailingSlash ? permalink.replace(/\/?$/, '/') : permalink.replace(/\/+$/, '');
}

/**
 * The address of a page's Markdown form: the page's route with `.md` added,
 * or `index.md` under a directory route such as the docs root.
 */
export function markdownPath(route) {
  return route.endsWith('/') ? `${route}index.md` : `${route}.md`;
}
