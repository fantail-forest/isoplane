import assert from 'node:assert/strict';
import {mkdtempSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {test} from 'node:test';
import {renderPage} from './convert.mjs';

const site = (pages) => ({
  url: 'https://isoplane.dev',
  permalinkOf: (file) => pages[file],
  warn: (message) => { throw new Error(message); },
});

function fixture(files) {
  const dir = mkdtempSync(join(tmpdir(), 'markdown-pages-'));
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  return dir;
}

test('a page becomes front matter, a title and plain markdown with absolute links', () => {
  const dir = fixture({
    'install.md': [
      '---', 'title: Install', '---', '',
      "import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';", '',
      '<PlatformToggle />', '',
      'Needs [setup](setup.md#run) and [the root](../index.md).', '',
      '## Steps {#steps}', '',
      '<OnlyOn platforms="mac linux">', '', 'Run `sh`.', '', '</OnlyOn>', '',
      ':::note[Preview]', 'Not yet.', ':::', '',
      'Added:since[0.1.3] later.', '',
    ].join('\n'),
  });
  const pages = {[join(dir, 'setup.md')]: '/docs/how-to/setup', [join(dir, '..', 'index.md')]: '/docs/'};
  const page = {title: 'Install', description: 'Needs setup.', canonical: 'https://isoplane.dev/docs/how-to/install'};
  assert.equal(renderPage(join(dir, 'install.md'), page, site(pages)), [
    '---',
    'title: "Install"',
    'description: "Needs setup."',
    'canonical: https://isoplane.dev/docs/how-to/install',
    '---', '',
    '# Install', '',
    'Needs [setup](https://isoplane.dev/docs/how-to/setup#run) and [the root](https://isoplane.dev/docs/).', '',
    '## Steps', '',
    '**macOS and Linux**', '',
    'Run `sh`.', '',
    '> **Preview**', '>', '> Not yet.', '',
    'Added (since 0.1.3) later.', '',
  ].join('\n'));
});

test('an imported partial appears inline and a page with its own title keeps it', () => {
  const dir = fixture({
    '_preview.mdx': ':::info\nPreview [docs](index.md).\n:::\n',
    'page.md': "---\ntitle: Page\n---\n\nimport Preview from './_preview.mdx';\n\n# Page\n\n<Preview />\n\nText.\n",
  });
  const pages = {[join(dir, 'index.md')]: '/docs/'};
  const page = {title: 'Page', description: '', canonical: 'https://isoplane.dev/docs/page'};
  assert.equal(renderPage(join(dir, 'page.md'), page, site(pages)), [
    '---', 'title: "Page"', 'canonical: https://isoplane.dev/docs/page', '---', '',
    '# Page', '',
    '> **Info**', '>', '> Preview [docs](https://isoplane.dev/docs/).', '',
    'Text.', '',
  ].join('\n'));
});

test('a CommonMark page keeps angle brackets in prose and drops HTML comments', () => {
  const dir = fixture({
    'ref.md': '---\ntitle: Ref\nmdx:\n  format: md\n---\n\n# Ref\n\nReturns Task<Response>.\n\n<!-- generated -->\n',
  });
  const page = {title: 'Ref', description: '', canonical: 'https://isoplane.dev/docs/ref', format: 'md'};
  assert.equal(renderPage(join(dir, 'ref.md'), page, site({})), [
    '---', 'title: "Ref"', 'canonical: https://isoplane.dev/docs/ref', '---', '',
    '# Ref', '',
    'Returns Task<Response>.', '',
  ].join('\n'));
});

test('a link to a file that is not a page is reported and kept', () => {
  const dir = fixture({'page.md': '---\ntitle: P\n---\n\nSee [it](_partial.mdx).\n'});
  const warnings = [];
  const page = {title: 'P', description: '', canonical: 'https://isoplane.dev/docs/page'};
  const markdown = renderPage(join(dir, 'page.md'), page, {...site({}), warn: (message) => warnings.push(message)});
  assert.match(markdown, /See \[it\]\(_partial\.mdx\)\./);
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /_partial\.mdx/);
});
