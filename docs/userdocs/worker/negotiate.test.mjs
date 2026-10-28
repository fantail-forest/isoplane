import assert from 'node:assert/strict';
import {test} from 'node:test';
import {markdownSibling, prefersMarkdown} from './negotiate.mjs';

const browser = 'text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8';

test('a request that names only text/markdown gets markdown', () => {
  assert.equal(prefersMarkdown('text/markdown'), true);
  assert.equal(prefersMarkdown('text/markdown; charset=utf-8'), true);
  assert.equal(prefersMarkdown('TEXT/Markdown;q=0.5'), true);
});

test('a browser, a wildcard or no header gets html', () => {
  assert.equal(prefersMarkdown(browser), false);
  assert.equal(prefersMarkdown('*/*'), false);
  assert.equal(prefersMarkdown('text/*'), false);
  assert.equal(prefersMarkdown(''), false);
  assert.equal(prefersMarkdown(null), false);
  assert.equal(prefersMarkdown('application/json'), false);
});

test('quality values decide between the two types', () => {
  assert.equal(prefersMarkdown('text/html;q=0.5, text/markdown'), true);
  assert.equal(prefersMarkdown('text/markdown;q=0.5, text/html'), false);
  assert.equal(prefersMarkdown('text/markdown;q=0, */*'), false);
  assert.equal(prefersMarkdown('text/markdown, text/html'), false);
  assert.equal(prefersMarkdown('text/html;q=0.9, text/*'), true);
});

test('an explicit markdown type outranks a wildcard that also matches html', () => {
  assert.equal(prefersMarkdown('text/markdown, */*'), true);
  assert.equal(prefersMarkdown('text/markdown, text/*;q=0.9'), true);
  assert.equal(prefersMarkdown('text/html, */*'), false);
});

test('the markdown sibling of a page path adds .md, and a directory path gets index.md', () => {
  assert.equal(markdownSibling('/docs/quickstart'), '/docs/quickstart.md');
  assert.equal(markdownSibling('/docs/how-to/install'), '/docs/how-to/install.md');
  assert.equal(markdownSibling('/docs'), '/docs/index.md');
  assert.equal(markdownSibling('/docs/'), '/docs/index.md');
  assert.equal(markdownSibling('/docs/0.1/'), '/docs/0.1/index.md');
  assert.equal(markdownSibling('/docs/0.1'), '/docs/0.1.md');
});
