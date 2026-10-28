// Serves the built site, giving a page's Markdown form to a client whose
// Accept header asks for it. Everything else is the static asset as built.
import {markdownSibling, prefersMarkdown} from './negotiate.mjs';

export default {
  async fetch(request, env) {
    if (isPageRequest(request) && prefersMarkdown(request.headers.get('Accept'))) {
      const sibling = new URL(request.url);
      sibling.pathname = markdownSibling(sibling.pathname);
      const markdown = await env.ASSETS.fetch(new Request(sibling, request));
      if (markdown.ok) return withHeaders(markdown, markdownHeaders);
    }
    const response = await env.ASSETS.fetch(request);
    const type = response.headers.get('Content-Type') ?? '';
    if (type.startsWith('text/html')) return withHeaders(response, {Vary: 'Accept'});
    if (type.startsWith('text/markdown')) return withHeaders(response, markdownHeaders);
    return response;
  },
};

// The Markdown form is the same page under a second content type, so search
// engines are told to index the HTML only.
const markdownHeaders = {
  'Content-Type': 'text/markdown; charset=utf-8',
  'Vary': 'Accept',
  'X-Robots-Tag': 'noindex',
};

// A route may end in what looks like an extension (`/docs/0.1`), so only a
// request for a Markdown file itself is left out.
function isPageRequest(request) {
  if (request.method !== 'GET' && request.method !== 'HEAD') return false;
  return !new URL(request.url).pathname.endsWith('.md');
}

function withHeaders(response, added) {
  const headers = new Headers(response.headers);
  for (const [name, value] of Object.entries(added)) {
    if (name === 'Vary') headers.append(name, value);
    else headers.set(name, value);
  }
  return new Response(response.body, {status: response.status, statusText: response.statusText, headers});
}
