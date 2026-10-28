import React, {useState} from 'react';
import Content from '@theme-original/DocItem/Content';
import Head from '@docusaurus/Head';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import {useDoc} from '@docusaurus/plugin-content-docs/client';
import {markdownPath, pageRoute} from '@site/src/markdown-path.mjs';

// Each assistant opens a new conversation from a prompt in its address, as
// Mintlify, Nextra and Cypress's documentation sites hand pages over.
const assistants = [
  {name: 'Claude', open: (prompt) => `https://claude.ai/new?q=${encodeURIComponent(prompt)}`},
  {name: 'ChatGPT', open: (prompt) => `https://chatgpt.com/?hints=search&prompt=${encodeURIComponent(prompt)}`},
];

const copyLabels = {idle: 'Copy as Markdown', copied: 'Copied', failed: 'Copy failed'};

function CopyMarkdown({path}) {
  const [state, setState] = useState('idle');
  const copy = async () => {
    // The clipboard is handed the pending text, so the write stays within the
    // click that allowed it even though the fetch comes first.
    const text = fetch(path).then((response) => {
      if (!response.ok) throw new Error(`${path} answered ${response.status}`);
      return response.text();
    });
    try {
      if (typeof ClipboardItem === 'undefined') await navigator.clipboard.writeText(await text);
      else await navigator.clipboard.write([new ClipboardItem({'text/plain': text.then((value) => new Blob([value], {type: 'text/plain'}))})]);
      setState('copied');
    } catch (error) {
      console.warn(`The page could not be copied as Markdown: ${error.message}`);
      setState('failed');
    }
    setTimeout(() => setState('idle'), 2000);
  };
  return <button type="button" onClick={copy}>{copyLabels[state]}</button>;
}

/**
 * The page, with a row that hands it to an assistant as Markdown and a link
 * to that Markdown in the page head.
 */
export default function ContentWrapper(props) {
  const {metadata} = useDoc();
  const {siteConfig} = useDocusaurusContext();
  const path = markdownPath(pageRoute(metadata.permalink, siteConfig));
  const url = siteConfig.url + path;
  const prompt = `Read ${url} so that I can ask you questions about it.`;
  return (
    <>
      <Head>
        <link rel="alternate" type="text/markdown" href={url} />
      </Head>
      <div className="markdown-actions">
        <CopyMarkdown path={path} />
        <a href={path}>View as Markdown</a>
        {assistants.map(({name, open}) => (
          <a key={name} href={open(prompt)} target="_blank" rel="noopener noreferrer">Open in {name}</a>
        ))}
      </div>
      <Content {...props} />
    </>
  );
}
