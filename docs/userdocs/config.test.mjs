import assert from 'node:assert/strict';
import {cp, mkdir, mkdtemp, rm, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {test} from 'node:test';
import {fileURLToPath} from 'node:url';
import {loadSiteConfig} from '@docusaurus/core/lib/server/config.js';

const siteDirectory = fileURLToPath(new URL('.', import.meta.url));

async function loadConfig(context, environment) {
  const cache = join(siteDirectory, '.docusaurus');
  await mkdir(cache, {recursive: true});
  const directory = await mkdtemp(join(cache, 'config-test-'));
  context.after(() => rm(directory, {recursive: true, force: true}));
  for (const path of ['docusaurus.config.js', 'versions.json', 'src']) {
    await cp(join(siteDirectory, path), join(directory, path), {recursive: true});
  }
  if (environment !== null) await writeFile(join(directory, '.env'), environment);
  const {siteConfig} = await loadSiteConfig({siteDir: directory});
  return siteConfig.presets.find((preset) => Array.isArray(preset) && preset[0] === 'classic')[1];
}

test('the docs omit GTM unless their own .env contains a nonblank container ID', async (context) => {
  const inherited = process.env.DOCS_GTM_CONTAINER_ID;
  process.env.DOCS_GTM_CONTAINER_ID = 'GTM-IGNORED';
  context.after(() => {
    if (inherited === undefined) delete process.env.DOCS_GTM_CONTAINER_ID;
    else process.env.DOCS_GTM_CONTAINER_ID = inherited;
  });
  for (const environment of [null, '', 'DOCS_GTM_CONTAINER_ID=\n', 'DOCS_GTM_CONTAINER_ID="   "\n']) {
    const options = await loadConfig(context, environment);
    assert.equal(Object.hasOwn(options, 'googleTagManager'), false);
  }
  const options = await loadConfig(context, 'DOCS_GTM_CONTAINER_ID=" GTM-TESTONLY "\n');
  assert.deepEqual(options.googleTagManager, {containerId: 'GTM-TESTONLY'});
});
