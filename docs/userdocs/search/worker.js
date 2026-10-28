// The search half of the docs Worker (worker/index.mjs): the search box's
// requests under /docs/search go to the Typesense server, rate limited per
// client and cached per query at the edge.

export const SEARCH_PATH = '/docs/search';
const COLLECTION = 'isoplane-docs';
const CACHE_SECONDS = 3600;
// Prefixes shorter than this get keyword matching only: semantic ranking adds
// nothing to "ap", and skipping it saves an embedding call per keystroke.
const MIN_SEMANTIC_QUERY_LENGTH = 4;
const UPSTREAM_ATTEMPTS = 3;
const MAX_RETRY_AFTER_MS = 5000;
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
    return new Response('Too many searches; slow down.', {status: 429, headers: {'retry-after': '10'}});
  }

  let body = '';
  if (isSearch) {
    const query = parseSearch(await request.text());
    if (!query) return new Response('One search of the docs collection per call.', {status: 400});
    body = JSON.stringify(query);
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
  if (!response) {
    try {
      response = await fetchUpstream(origin, {
        method: request.method,
        headers: {
          'content-type': 'application/json',
          'x-typesense-api-key': env.DOCS_SEARCH_KEY,
        },
        body: isSearch ? body : undefined,
      });
    } catch (error) {
      console.log(JSON.stringify({event: 'docs_search_unreachable', error: String(error)}));
      return new Response('Search is unavailable; try again shortly.', {status: 503, headers: {'retry-after': '2'}});
    }
  }

  if (cacheKey && !cached && response.ok) {
    const copy = new Response(response.clone().body, response);
    copy.headers.set('cache-control', `public, s-maxage=${CACHE_SECONDS}`);
    ctx.waitUntil(cache.put(cacheKey, copy));
  }
  if (isSearch) {
    const session = request.headers.get('x-search-session');
    ctx.waitUntil(logSearch(body, response.clone(), {
      cached: Boolean(cached),
      durationMs: Date.now() - started,
      session: session && SESSION_ID.test(session) ? session : undefined,
    }));
  }

  const headers = new Headers(response.headers);
  headers.set('cache-control', 'no-store');
  headers.set('x-search-cache', cached ? 'hit' : 'miss');
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
  if (q.length >= MIN_SEMANTIC_QUERY_LENGTH || typeof query.query_by !== 'string') return {searches: [query]};
  const {vector_query: _vectorQuery, ...keywordOnly} = query;
  keywordOnly.query_by = query.query_by.split(',').filter((field) => field !== 'embedding').join(',');
  return {searches: [keywordOnly]};
}

// The server answers 429 while a client is over its own limit and 502/503
// while the machine resumes from suspension; both say when to come back.
async function fetchUpstream(origin, init) {
  let response;
  for (let attempt = 1; attempt <= UPSTREAM_ATTEMPTS; attempt++) {
    try {
      response = await fetch(origin, init);
    } catch (error) {
      if (attempt === UPSTREAM_ATTEMPTS) throw error;
      await delay(backoffMs(attempt));
      continue;
    }
    if (![429, 502, 503, 504].includes(response.status) || attempt === UPSTREAM_ATTEMPTS) return response;
    await delay(retryAfterMs(response) ?? backoffMs(attempt));
  }
  return response;
}

function retryAfterMs(response) {
  const seconds = Number(response.headers.get('retry-after'));
  return Number.isFinite(seconds) && seconds > 0 ? Math.min(seconds * 1000, MAX_RETRY_AFTER_MS) : undefined;
}

const backoffMs = (attempt) => 300 * 2 ** (attempt - 1);
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

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
async function logSearch(body, response, {cached, durationMs, session}) {
  let query;
  let found;
  try {
    query = JSON.parse(body).searches[0].q;
    if (response.ok) found = (await response.json()).results?.[0]?.found;
  } catch {
    // A non-JSON upstream error is still logged, with its status.
  }
  console.log(JSON.stringify({
    event: 'docs_search', query, found, status: response.status, cached, duration_ms: durationMs, session,
  }));
}
