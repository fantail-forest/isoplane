import React, { useState, useRef, useCallback, useMemo } from 'react';
import { createPortal } from 'react-dom';
// @ts-ignore
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
// @ts-ignore
import { useHistory } from '@docusaurus/router';
// @ts-ignore
import { useBaseUrlUtils } from '@docusaurus/useBaseUrl';
// @ts-ignore
import Link from '@docusaurus/Link';
// @ts-ignore
import { isRegexpStringMatch } from '@docusaurus/theme-common';
import { DocSearchButton, useDocSearchKeyboardEvents, } from 'typesense-docsearch-react';
import { SearchResponseAdapter } from 'typesense-instantsearch-adapter/lib/SearchResponseAdapter';
import { useTypesenseContextualFilters } from 'docusaurus-theme-search-typesense/client';
// @ts-ignore
import Translate from '@docusaurus/Translate';
// @ts-ignore
import translations from '@theme/SearchTranslations';
import { DocsPreferredVersionContextProvider } from '@docusaurus/plugin-content-docs/lib/client/index.js';
let DocSearchModal = null;

// Ejected from docusaurus-theme-search-typesense to shape the traffic the
// search box sends through /docs/search (see ../../../search/worker.js):
// typing is debounced, a refusal or outage is retried after the pause its
// Retry-After names, and each page load can tag its searches with a random id
// so refinements line up in the search log. The id lives in memory only;
// nothing is stored on the device. It is disabled pending a consent
// implementation.
const DEBOUNCE_MS = 250;
// Pauses when a refusal or outage names none, per attempt.
const RETRY_FALLBACK_MS = {429: [5000], 502: [2000, 4000], 503: [2000, 4000]};
const MAX_RETRY_AFTER_MS = 10000;
const SEARCH_SESSION_ENABLED = false;
const pageSessionId = SEARCH_SESSION_ENABLED && typeof crypto !== 'undefined' && crypto.randomUUID
    ? crypto.randomUUID()
    : undefined;

// Resolves every call made during a pause with the result of the last one, so
// a burst of keystrokes is one search and no caller is left waiting.
function debounceSearch(search) {
    let timer;
    let waiting = [];
    return (requests) => new Promise((resolve, reject) => {
        waiting.push({resolve, reject});
        clearTimeout(timer);
        timer = setTimeout(() => {
            const settle = waiting;
            waiting = [];
            search(requests).then(
                (result) => settle.forEach(({resolve: done}) => done(result)),
                (error) => settle.forEach(({reject: fail}) => fail(error)));
        }, DEBOUNCE_MS);
    });
}

// The modal's one search, sent directly rather than through the Typesense
// client, which drops every response header, so that a refusal's Retry-After
// sets the pause. The adapter restores the response shape the modal expects.
function searchTransport({nodes: [node], apiKey, additionalHeaders}) {
    const endpoint = `${node.protocol}://${node.host}:${node.port}${node.path ?? ''}/multi_search`;
    const headers = {'content-type': 'application/json', 'x-typesense-api-key': apiKey, ...additionalHeaders};
    return async ([request]) => {
        const response = await fetchWithRetry(endpoint, {method: 'POST', headers, body: JSON.stringify({searches: [request]})});
        const {results: [result]} = await response.json();
        const adapter = new SearchResponseAdapter(result, {
            params: {...request.params, highlightPreTag: '<mark>', highlightPostTag: '</mark>'},
        }, {geoLocationField: ''});
        return {results: [adapter.adapt()]};
    };
}

async function fetchWithRetry(url, init) {
    for (let attempt = 0; ; attempt++) {
        let response;
        try {
            response = await fetch(url, init);
        } catch {
            response = undefined;
        }
        if (response?.ok) return response;
        const status = response?.status ?? 503;
        const fallback = RETRY_FALLBACK_MS[status] ?? [];
        if (attempt >= fallback.length) {
            throw Object.assign(new Error(`Search failed with status ${status}`), {httpStatus: status});
        }
        await new Promise((resolve) => setTimeout(resolve, retryAfterMs(response) ?? fallback[attempt]));
    }
}

function retryAfterMs(response) {
    const seconds = Number(response?.headers.get('retry-after'));
    return Number.isFinite(seconds) && seconds > 0 ? Math.min(seconds * 1000, MAX_RETRY_AFTER_MS) : undefined;
}
function Hit({ hit, children, }) {
    return <Link to={hit.url}>{children}</Link>;
}
function ResultsFooter({ state, onClose }) {
    const { siteConfig: { baseUrl } } = useDocusaurusContext();
    return (<Link to={`${baseUrl}search?q=${encodeURIComponent(state.query)}`} onClick={onClose}>
      <Translate id="theme.SearchBar.seeAll" values={{ count: state.context.nbHits }}>
        {'See all {count} results'}
      </Translate>
    </Link>);
}
function DocSearch({ contextualSearch, externalUrlRegex, ...props }) {
    const { siteMetadata } = useDocusaurusContext();
    const contextualSearchFacetFilters = useTypesenseContextualFilters();
    const configFacetFilters = props.typesenseSearchParameters?.filter_by ?? '';
    const facetFilters = contextualSearch
        ? // Merge contextual search filters with config filters
            [contextualSearchFacetFilters, configFacetFilters]
                .filter((e) => e)
                .join(' && ')
        : // ... or use config facetFilters
            configFacetFilters;
    // we let user override default searchParameters if he wants to
    const typesenseSearchParameters = {
        filter_by: facetFilters,
        ...props.typesenseSearchParameters,
    };
    const typesenseServerConfig = useMemo(() => ({
        ...props.typesenseServerConfig,
        ...(pageSessionId && {additionalHeaders: {'x-search-session': pageSessionId}}),
    }), [props.typesenseServerConfig]);
    const transformSearchClient = useCallback(
        (client) => ({...client, search: debounceSearch(searchTransport(typesenseServerConfig))}),
        [typesenseServerConfig]);
    const typesenseCollectionName = props.typesenseCollectionName;
    const { withBaseUrl } = useBaseUrlUtils();
    const history = useHistory();
    const searchContainer = useRef(null);
    const searchButtonRef = useRef(null);
    const [isOpen, setIsOpen] = useState(false);
    const [initialQuery, setInitialQuery] = useState(undefined);
    const importDocSearchModalIfNeeded = useCallback(() => {
        if (DocSearchModal) {
            return Promise.resolve();
        }
        return Promise.all([
            // @ts-ignore
            import('typesense-docsearch-react/modal'),
            // @ts-ignore
            import('typesense-docsearch-react/style'),
            // @ts-ignore
            import('./styles.css'),
        ]).then(([{ DocSearchModal: Modal }]) => {
            DocSearchModal = Modal;
        });
    }, []);
    const onOpen = useCallback(() => {
        importDocSearchModalIfNeeded().then(() => {
            searchContainer.current = document.createElement('div');
            document.body.insertBefore(searchContainer.current, document.body.firstChild);
            setIsOpen(true);
        });
    }, [importDocSearchModalIfNeeded, setIsOpen]);
    const onClose = useCallback(() => {
        setIsOpen(false);
        searchContainer.current?.remove();
    }, [setIsOpen]);
    const onInput = useCallback((event) => {
        importDocSearchModalIfNeeded().then(() => {
            setIsOpen(true);
            setInitialQuery(event.key);
        });
    }, [importDocSearchModalIfNeeded, setIsOpen, setInitialQuery]);
    const navigator = useRef({
        navigate({ itemUrl }) {
            // Algolia results could contain URL's from other domains which cannot
            // be served through history and should navigate with window.location
            if (isRegexpStringMatch(externalUrlRegex, itemUrl)) {
                window.location.href = itemUrl;
            }
            else {
                history.push(itemUrl);
            }
        },
    }).current;
    const transformItems = useRef((items) => items.map((item) => {
        // If result contains a external domain, we should navigate without
        // relative URL
        if (isRegexpStringMatch(externalUrlRegex, item.url)) {
            return item;
        }
        // We transform the absolute URL into a relative URL.
        const url = new URL(item.url);
        return {
            ...item,
            url: withBaseUrl(`${url.pathname}${url.search}${url.hash}`),
        };
    })).current;
    const resultsFooterComponent = useMemo(() =>
    // eslint-disable-next-line react/no-unstable-nested-components
    (footerProps) => (<ResultsFooter {...footerProps} onClose={onClose}/>), [onClose]);
    useDocSearchKeyboardEvents({
        isOpen,
        onOpen,
        onClose,
        onInput,
        searchButtonRef,
    });
    return (<>
      <DocSearchButton onTouchStart={importDocSearchModalIfNeeded} onFocus={importDocSearchModalIfNeeded} onMouseOver={importDocSearchModalIfNeeded} onClick={onOpen} ref={searchButtonRef} translations={translations.button}/>

      {isOpen &&
            DocSearchModal &&
            searchContainer.current &&
            createPortal(<DocSearchModal onClose={onClose} initialScrollY={window.scrollY} initialQuery={initialQuery} navigator={navigator} transformItems={transformItems} hitComponent={Hit} {...(props.searchPagePath && {
                resultsFooterComponent,
            })} {...props} transformSearchClient={transformSearchClient} typesenseSearchParameters={typesenseSearchParameters} typesenseServerConfig={typesenseServerConfig} typesenseCollectionName={typesenseCollectionName} placeholder={translations.placeholder} translations={translations.modal}/>, searchContainer.current)}
    </>);
}
export default function SearchBar() {
    const { siteConfig } = useDocusaurusContext();
    return (<DocsPreferredVersionContextProvider>
      <DocSearch {...siteConfig.themeConfig.typesense}/>
    </DocsPreferredVersionContextProvider>);
}
