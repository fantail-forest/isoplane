// Renders a page's Markdown source as plain Markdown for readers outside the
// site: imports, components and directives become text or go, and links
// become absolute. The source is parsed as Docusaurus parses it, so the
// output follows the published page rather than the HTML.
import {readFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {unified} from 'unified';
import remarkParse from 'remark-parse';
import remarkFrontmatter from 'remark-frontmatter';
import remarkGfm from 'remark-gfm';
import remarkDirective from 'remark-directive';
import remarkMdx from 'remark-mdx';
import remarkStringify from 'remark-stringify';
import {toString} from 'mdast-util-to-string';
import {platformLabels} from '../../platform/preference.ts';

const admonitions = ['note', 'tip', 'info', 'warning', 'danger', 'caution'];

/**
 * Renders the page at `file` as plain Markdown with YAML front matter.
 *
 * `page` names the page: `title`, `description` (may be empty), `canonical`
 * (its absolute address) and `format` (`md` for a page Docusaurus reads as
 * CommonMark, `mdx` otherwise). `site.url` is the site's origin and
 * `site.permalinkOf` maps a source file's absolute path to its page's
 * permalink, or to `undefined` for a file that is not a page. A link to a
 * source file that is not a page is reported through `site.warn` and kept.
 */
export function renderPage(file, page, site) {
  const tree = parse(file, page.format === 'md' ? 'md' : 'mdx');
  const body = transformAll(tree.children, {file, page, site});
  const hasTitle = body.some((node) => node.type === 'heading' && node.depth === 1);
  const children = hasTitle ? body : [{type: 'heading', depth: 1, children: [{type: 'text', value: page.title}]}, ...body];
  return frontMatter(page) + stringify({type: 'root', children});
}

function parse(file, format) {
  const processor = unified().use(remarkParse).use(remarkFrontmatter).use(remarkGfm).use(remarkDirective);
  const text = readFileSync(file, 'utf8');
  if (format !== 'mdx') return processor.parse(text);
  return processor.use(remarkMdx).parse(escapeHeadingIds(text));
}

// A heading's `{#id}` would read as an expression in MDX; Docusaurus escapes
// it before parsing, with this regular expression, and so does this.
function escapeHeadingIds(text) {
  return text.replaceAll(/(?:^|\n)#{1,6}(?!#).*/g, (line) => line.replace('{#', '\\{#').replace('\\\\{#', '\\{#'));
}

function stringify(tree) {
  return unified()
    .use(remarkGfm)
    .use(remarkStringify, {bullet: '-', emphasis: '*', strong: '*', fences: true, rule: '-', listItemIndent: 'one'})
    .stringify(tree);
}

function frontMatter({title, description, canonical}) {
  const lines = [`title: ${JSON.stringify(title)}`];
  if (description) lines.push(`description: ${JSON.stringify(description)}`);
  lines.push(`canonical: ${canonical}`);
  return `---\n${lines.join('\n')}\n---\n\n`;
}

function transformAll(nodes, context) {
  const partials = partialImports(nodes, context.file);
  return nodes.flatMap((node) => transform(node, {...context, partials}));
}

// `import Name from './_partial.mdx'` makes `<Name />` stand for that file.
function partialImports(nodes, file) {
  const partials = new Map();
  for (const node of nodes) {
    if (node.type !== 'mdxjsEsm') continue;
    for (const statement of node.data?.estree?.body ?? []) {
      if (statement.type !== 'ImportDeclaration' || !/\.mdx?$/.test(statement.source.value)) continue;
      for (const specifier of statement.specifiers) {
        partials.set(specifier.local.name, resolve(dirname(file), statement.source.value));
      }
    }
  }
  return partials;
}

function transform(node, context) {
  switch (node.type) {
    case 'yaml':
    case 'mdxjsEsm':
    case 'mdxFlowExpression':
    case 'mdxTextExpression':
      return [];
    case 'html':
      return node.value.trim().startsWith('<!--') ? [] : [node];
    case 'mdxJsxFlowElement':
    case 'mdxJsxTextElement':
      return jsx(node, context);
    case 'containerDirective':
      return admonitions.includes(node.name) ? admonition(node, context) : children(node, context);
    case 'leafDirective':
      return children(node, context);
    case 'textDirective':
      return [{type: 'text', value: node.name === 'since' ? ` (since ${toString(node)})` : toString(node)}];
    case 'heading':
      return [{...node, children: withoutExplicitId(children(node, context))}];
    case 'link':
      return [{...node, url: absoluteUrl(node.url, context), children: children(node, context)}];
    case 'definition':
    case 'image':
      return [{...node, url: absoluteUrl(node.url, context)}];
    default:
      return node.children ? [{...node, children: children(node, context)}] : [node];
  }
}

function children(node, context) {
  return node.children.flatMap((child) => transform(child, context));
}

function jsx(node, context) {
  const partial = context.partials.get(node.name);
  if (partial) return transformAll(parse(partial, 'mdx').children, {...context, file: partial});
  switch (node.name) {
    case 'PlatformToggle':
      return [];
    case 'OnlyOn':
      return [strongParagraph(platformNames(node)), ...children(node, context)];
    case 'summary':
      return [strongParagraph(toString(node))];
    default:
      return children(node, context);
  }
}

// The names of the platforms an `<OnlyOn>` block is for, as the page shows
// them without JavaScript.
function platformNames(node) {
  const platforms = node.attributes.find((attribute) => attribute.name === 'platforms')?.value ?? '';
  return platforms.split(' ').map((platform) => platformLabels[platform] ?? platform).join(' and ');
}

function admonition(node, context) {
  const [label, ...rest] = node.children[0]?.data?.directiveLabel ? node.children : [undefined, ...node.children];
  const title = label ? toString(label) : node.name[0].toUpperCase() + node.name.slice(1);
  return [{type: 'blockquote', children: [strongParagraph(title), ...rest.flatMap((child) => transform(child, context))]}];
}

function strongParagraph(text) {
  return {type: 'paragraph', children: [{type: 'strong', children: [{type: 'text', value: text}]}]};
}

// A heading may end in `{#id}` to choose its anchor; the anchor stays the
// page's, so the marker goes.
function withoutExplicitId(nodes) {
  const last = nodes.at(-1);
  if (last?.type !== 'text') return nodes;
  return [...nodes.slice(0, -1), {...last, value: last.value.replace(/\s*\{#[^}]+\}\s*$/, '')}];
}

function absoluteUrl(url, {file, page, site}) {
  if (/^([a-z][a-z0-9+.-]*:|\/\/|#)/i.test(url)) return url;
  const [path, hash = ''] = url.split(/(?=#)/);
  if (path.startsWith('/')) return site.url + path + hash;
  if (/\.mdx?$/.test(path)) {
    const permalink = site.permalinkOf(resolve(dirname(file), path));
    if (permalink) return site.url + permalink + hash;
    site.warn(`${file}: the link "${url}" leads to a file that is not a page; kept as written.`);
    return url;
  }
  return new URL(path + hash, page.canonical).href;
}
