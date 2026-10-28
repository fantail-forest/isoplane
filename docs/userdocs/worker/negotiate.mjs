import {markdownPath} from '../src/markdown-path.mjs';

/**
 * Whether an `Accept` header asks for Markdown over HTML: the best match for
 * `text/markdown` outranks the best match for `text/html` by quality, or by
 * specificity at equal quality. A bare `text/markdown` qualifies; a wildcard
 * alone, or a browser's list with `text/html` first, does not.
 */
export function prefersMarkdown(accept) {
  const markdown = {q: 0, specificity: -1};
  const html = {q: 0, specificity: -1};
  for (const field of (accept ?? '').split(',')) {
    const [range, ...parameters] = field.split(';').map((part) => part.trim());
    const [type, subtype] = range.toLowerCase().split('/');
    if (!type || !subtype) continue;
    const specificity = type === '*' ? 0 : subtype === '*' ? 1 : 2;
    const q = quality(parameters);
    for (const [best, wanted] of [[markdown, 'markdown'], [html, 'html']]) {
      const matches = (type === '*' || type === 'text') && (subtype === '*' || subtype === wanted);
      if (matches && specificity > best.specificity) Object.assign(best, {q, specificity});
    }
  }
  return markdown.q > html.q || (markdown.q > 0 && markdown.q === html.q && markdown.specificity > html.specificity);
}

function quality(parameters) {
  for (const parameter of parameters) {
    const [name, value] = parameter.split('=').map((part) => part.trim());
    if (name.toLowerCase() !== 'q') continue;
    const q = Number.parseFloat(value);
    return Number.isFinite(q) ? Math.min(Math.max(q, 0), 1) : 1;
  }
  return 1;
}

/**
 * The asset path of a page's Markdown form, as the build lays the files out:
 * the page path plus `.md`, or `index.md` under a directory path. The docs
 * root is a directory whether or not its slash was sent.
 */
export function markdownSibling(pathname) {
  return markdownPath(pathname === '/docs' ? '/docs/' : pathname);
}
