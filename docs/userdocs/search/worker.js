// The search half of the docs Worker (worker/index.mjs): the search box's
// requests under /docs/search go to the Typesense server, rate limited per
// client and cached per query at the edge.

export const SEARCH_PATH = '/docs/search';
const CACHE_SECONDS = 3600;

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

  const body = isSearch ? await request.text() : '';
  // The site sends a placeholder key; the real one never leaves this Worker.
  url.searchParams.delete('x-typesense-api-key');
  const origin = new URL(`${route}${url.search}`, env.SEARCH_ORIGIN);

  const cache = caches.default;
  const cacheKey = isSearch ? await searchCacheKey(url, body, env.SEARCH_INDEX_VERSION) : undefined;
  const cached = cacheKey && await cache.match(cacheKey);
  const started = Date.now();
  const response = cached ?? await fetch(origin, {
    method: request.method,
    headers: {
      'content-type': request.headers.get('content-type') ?? 'application/json',
      'x-typesense-api-key': env.DOCS_SEARCH_KEY,
    },
    body: isSearch ? body : undefined,
  });

  if (cacheKey && !cached && response.ok) {
    const copy = new Response(response.clone().body, response);
    copy.headers.set('cache-control', `public, s-maxage=${CACHE_SECONDS}`);
    ctx.waitUntil(cache.put(cacheKey, copy));
  }
  if (isSearch) ctx.waitUntil(logSearch(body, response.clone(), Boolean(cached), Date.now() - started));

  const headers = new Headers(response.headers);
  headers.set('cache-control', 'no-store');
  headers.set('x-search-cache', cached ? 'hit' : 'miss');
  return new Response(response.body, {status: response.status, headers});
}

// One edge cache entry per distinct query against one index build. Keys are
// hashed because the query is a request body, which the cache cannot key on.
async function searchCacheKey(url, body, indexVersion) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`${indexVersion}\n${url.search}\n${body}`));
  const hash = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
  return new Request(`${url.origin}${SEARCH_PATH}/_cache/${hash}`, {method: 'GET'});
}

// One structured line per search: what was asked, whether anything matched,
// and how it was answered. Queries are content, not identity; no client
// address goes with them.
async function logSearch(body, response, cached, durationMs) {
  let queries = [];
  let found = [];
  try {
    queries = JSON.parse(body).searches?.map((s) => s.q) ?? [];
    if (response.ok) found = (await response.json()).results?.map((r) => r.found ?? 0) ?? [];
  } catch {
    // A malformed body or a non-JSON upstream error is still logged, with
    // status and no query.
  }
  console.log(JSON.stringify({
    event: 'docs_search', queries, found, status: response.status, cached, duration_ms: durationMs,
  }));
}
