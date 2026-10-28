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
import { useTypesenseContextualFilters } from 'docusaurus-theme-search-typesense/client';
// @ts-ignore
import Translate from '@docusaurus/Translate';
// @ts-ignore
import translations from '@theme/SearchTranslations';
import { DocsPreferredVersionContextProvider } from '@docusaurus/plugin-content-docs/lib/client/index.js';
let DocSearchModal = null;

// Ejected from docusaurus-theme-search-typesense to shape the traffic the
// search box sends through /docs/search (see ../../../search/worker.js):
// typing is debounced, refusals and outages are retried after a pause, and
// each page load tags its searches with a random id so refinements line up in
// the search log. The id lives in memory only; nothing is stored on the device.
const DEBOUNCE_MS = 250;
const RETRY_DELAYS_MS = {429: [2000], 503: [1000, 3000], 502: [1000, 3000]};
const pageSessionId = typeof crypto !== 'undefined' && crypto.randomUUID ? crypto.randomUUID() : undefined;

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

// The Typesense client surfaces a refused or failed search as an error with
// its HTTP status and nothing else, so the pauses are fixed rather than read
// from the Retry-After header the Worker sends.
async function searchWithRetry(search, requests) {
    for (let attempt = 0; ; attempt++) {
        try {
            return await search(requests);
        } catch (error) {
            const delays = RETRY_DELAYS_MS[error?.httpStatus] ?? [];
            if (attempt >= delays.length) throw error;
            await new Promise((resolve) => setTimeout(resolve, delays[attempt]));
        }
    }
}

function shapeSearchClient(searchClient) {
    const search = debounceSearch((requests) => searchWithRetry((r) => searchClient.search(r), requests));
    return {...searchClient, search};
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
    const typesenseServerConfig = {
        ...props.typesenseServerConfig,
        ...(pageSessionId && {additionalHeaders: {'x-search-session': pageSessionId}}),
    };
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
            })} {...props} transformSearchClient={shapeSearchClient} typesenseSearchParameters={typesenseSearchParameters} typesenseServerConfig={typesenseServerConfig} typesenseCollectionName={typesenseCollectionName} placeholder={translations.placeholder} translations={translations.modal}/>, searchContainer.current)}
    </>);
}
export default function SearchBar() {
    const { siteConfig } = useDocusaurusContext();
    return (<DocsPreferredVersionContextProvider>
      <DocSearch {...siteConfig.themeConfig.typesense}/>
    </DocsPreferredVersionContextProvider>);
}
