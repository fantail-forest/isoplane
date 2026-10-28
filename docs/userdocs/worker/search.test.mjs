import assert from 'node:assert/strict';
import {test} from 'node:test';
import worker from './index.mjs';

// The upstream the Worker proxies to, scripted per test: each entry is the
// body of the next reply, and every request body is kept.
function fakeUpstream(replies) {
  const calls = [];
  globalThis.fetch = async (_url, init) => {
    calls.push(JSON.parse(init.body).searches[0]);
    return new Response(JSON.stringify(replies.shift()), {status: 200, headers: {'content-type': 'application/json'}});
  };
  return calls;
}

globalThis.caches = {default: {match: async () => undefined, put: async () => {}}};
const env = {
  SEARCH_ORIGIN: 'https://origin.test', DOCS_SEARCH_KEY: 'k', SEARCH_INDEX_VERSION: 'v',
  SEARCH_RATE_LIMIT: {limit: async () => ({success: true})},
  ASSETS: {fetch: async () => new Response('<html></html>', {headers: {'content-type': 'text/html'}})},
};
const ctx = {waitUntil: () => {}};
console.log = () => {};

async function search(q) {
  const request = new Request('https://isoplane.dev/docs/search/multi_search', {
    method: 'POST',
    body: JSON.stringify({searches: [{collection: 'isoplane-docs', q, query_by: 'content,embedding', vector_query: 'embedding:([], k: 10)'}]}),
  });
  return worker.fetch(request, env, ctx);
}
const found = {results: [{found: 3, hits: []}]};
const notEmbedded = {results: [{code: 400, error: 'API timeout.'}]};

test('a hybrid search goes upstream once, with one embedding try', async () => {
  const calls = fakeUpstream([found]);
  const response = await search('activate instance');
  assert.equal(response.headers.get('x-search-mode'), 'hybrid');
  assert.deepEqual(await response.json(), found);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].remote_embedding_num_tries, 1);
  assert.ok('vector_query' in calls[0]);
});

test('a query the server could not embed is asked again by keyword alone', async () => {
  const calls = fakeUpstream([notEmbedded, found]);
  const response = await search('activate instance');
  assert.equal(response.headers.get('x-search-mode'), 'keyword');
  assert.deepEqual(await response.json(), found);
  assert.equal(calls.length, 2);
  assert.equal('vector_query' in calls[1], false);
  assert.equal(calls[1].query_by, 'content');
});

test('a short prefix is keyword-only from the start', async () => {
  const calls = fakeUpstream([found]);
  const response = await search('ap');
  assert.equal(response.headers.get('x-search-mode'), 'keyword');
  assert.equal(calls.length, 1);
  assert.equal('vector_query' in calls[0], false);
});

test('a page request still goes to the assets', async () => {
  const response = await worker.fetch(new Request('https://isoplane.dev/docs/quickstart'), env, ctx);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('vary'), 'Accept');
});
