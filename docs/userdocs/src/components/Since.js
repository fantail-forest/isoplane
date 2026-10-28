import React from 'react';

/**
 * The release that added the thing this follows, as a small pill. Written in
 * Markdown as `:since[0.1.3]`; `next` is a release not yet snapshotted.
 */
export default function Since({version}) {
  const next = version === 'next';
  return (
    <span className="since" title={next ? 'Added in the next release' : `Added in ${version}`}>
      {version}
    </span>
  );
}
