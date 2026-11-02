// The search half of the docs Worker (worker/index.mjs): the search box's
// requests under /docs/search go to the Typesense server, rate limited per
// client and cached per query at the edge.

export const SEARCH_PATH = '/docs/search';
const COLLECTION = 'isoplane-docs';
const CACHE_SECONDS = 3600;
// Prefixes shorter than this get keyword matching only: semantic ranking adds
// nothing to "ap", and skipping it saves an embedding call per keystroke.
const MIN_SEMANTIC_QUERY_LENGTH = 4;
// A query's embedding call to the provider gets one try within this long;
// a slow or refusing provider costs the query its semantic half, not a stall.
const EMBEDDING_TIMEOUT_MS = 3000;
const SESSION_ID = /^[A-Za-z0-9-]{1,40}$/;

export async function search(request, env, ctx) {
  const url = new URL(request.url);
  const route = url.pathname.slice(SEARCH_PATH.length);
  const isSearch = request.method === 'POST' && route === '/multi_search';
  if (!isSearch && !(request.method === 'GET' && route === '/health')) {
    return new Response('Not found', {status: 404});
  }

  const client = request.headers.get('cf-connecting-ip') ?? 'unknown';
  const {success} = await env.SEARCH_RATE_LIMIT.limit({key: client});
  if (!success) {
    return new Response('Too many searches; slow down.', {status: 429, headers: {'retry-after': '5'}});
  }

  let query;
  let body = '';
  if (isSearch) {
    query = parseSearch(await request.text());
    if (!query) return new Response('One search of the docs collection per call.', {status: 400});
    body = JSON.stringify({searches: [query]});
  }
  // The site sends a placeholder key; the real one never leaves this Worker.
  url.searchParams.delete('x-typesense-api-key');
  url.searchParams.set('limit_multi_searches', '1');
  const origin = new URL(`${route}${url.search}`, env.SEARCH_ORIGIN);

  const cache = caches.default;
  const cacheKey = isSearch ? await searchCacheKey(url, body, env.SEARCH_INDEX_VERSION) : undefined;
  const cached = cacheKey && await cache.match(cacheKey);
  const started = Date.now();
  let response = cached;
  let mode = query?.vector_query ? 'hybrid' : 'keyword';
  if (!response) {
    // A refusal or outage goes back to the search box as it came, Retry-After
    // included. The box decides whether the query is still worth retrying;
    // queueing here would hide load from it and pile retries onto the server.
    const upstream = (payload) => fetch(origin, {
      method: request.method,
      headers: {
        'content-type': 'application/json',
        'x-typesense-api-key': env.DOCS_SEARCH_KEY,
      },
      body: isSearch ? payload : undefined,
    });
    try {
      response = await upstream(body);
      // The server reports a query it could not embed as a failed search in an
      // otherwise successful reply. The provider is the least dependable link,
      // so such a query is asked again by keyword alone, uncached.
      if (mode === 'hybrid' && await searchFailed(response)) {
        mode = 'keyword';
        response = await upstream(JSON.stringify({searches: [keywordOnly(query)]}));
      }
    } catch (error) {
      console.log(JSON.stringify({event: 'docs_search_unreachable', error: String(error)}));
      return new Response('Search is unavailable; try again shortly.', {status: 503, headers: {'retry-after': '2'}});
    }
  }

  if (cacheKey && !cached && response.ok && mode === (query.vector_query ? 'hybrid' : 'keyword')) {
    const copy = new Response(response.clone().body, response);
    copy.headers.set('cache-control', `public, s-maxage=${CACHE_SECONDS}`);
    ctx.waitUntil(cache.put(cacheKey, copy));
  }
  if (isSearch) {
    const session = request.headers.get('x-search-session');
    ctx.waitUntil(logSearch(body, response.clone(), {
      cached: Boolean(cached),
      mode,
      durationMs: Date.now() - started,
      session: session && SESSION_ID.test(session) ? session : undefined,
    }));
  }

  const headers = new Headers(response.headers);
  headers.set('cache-control', 'no-store');
  headers.set('x-search-cache', cached ? 'hit' : 'miss');
  if (isSearch) headers.set('x-search-mode', mode);
  return new Response(response.body, {status: response.status, headers});
}

// Admits one search of the docs collection, shaped for the server, or nothing.
function parseSearch(text) {
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    return undefined;
  }
  const searches = parsed?.searches;
  if (!Array.isArray(searches) || searches.length !== 1) return undefined;
  const [query] = searches;
  if (!query || typeof query !== 'object' || query.collection !== COLLECTION) return undefined;
  const q = typeof query.q === 'string' ? query.q.trim() : '';
  if (q.length < MIN_SEMANTIC_QUERY_LENGTH) return keywordOnly(query);
  if (!query.vector_query) return query;
  // Typesense's remote embedders reject prefix searches.
  return {...query, prefix: false, remote_embedding_timeout_ms: EMBEDDING_TIMEOUT_MS, remote_embedding_num_tries: 1};
}

function keywordOnly(query) {
  const {vector_query: _vectorQuery, remote_embedding_timeout_ms: _timeout, remote_embedding_num_tries: _tries, ...rest} = query;
  if (typeof rest.query_by === 'string') {
    rest.query_by = rest.query_by.split(',').filter((field) => field !== 'embedding').join(',');
  }
  return rest;
}

// A multi_search reply is 200 even when its one search failed; the failure is
// a code and error in that search's slot.
async function searchFailed(response) {
  if (!response.ok) return false;
  try {
    const {results} = await response.clone().json();
    return results?.[0]?.error !== undefined;
  } catch {
    return false;
  }
}

// One edge cache entry per distinct query against one index build. Keys are
// hashed because the query is a request body, which the cache cannot key on.
async function searchCacheKey(url, body, indexVersion) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`${indexVersion}\n${url.search}\n${body}`));
  const hash = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
  return new Request(`${url.origin}${SEARCH_PATH}/_cache/${hash}`, {method: 'GET'});
}

// One structured line per search: what was asked, whether anything matched,
// and how it was answered. The session is the search box's own random id for
// one page load, so a visitor's refinements line up; no client address goes
// with them.
async function logSearch(body, response, {cached, mode, durationMs, session}) {
  let query;
  let found;
  try {
    query = JSON.parse(body).searches[0].q;
    if (response.ok) found = (await response.json()).results?.[0]?.found;
  } catch {
    // A non-JSON upstream error is still logged, with its status.
  }
  console.log(JSON.stringify({
    event: 'docs_search', query, found, status: response.status, cached, mode, duration_ms: durationMs, session,
  }));
}
