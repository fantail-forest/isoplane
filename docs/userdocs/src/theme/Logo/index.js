import React from 'react';
import Glyph from '@site/src/img/logo.svg';

/**
 * The wordmark in the navbar and the mobile menu. It leads to the marketing
 * home page, a separate application on this origin, so it is a plain anchor
 * the docs router leaves alone.
 */
export default function Logo({imageClassName, titleClassName, ...props}) {
  return (
    <a href="/" aria-label="Isoplane home" {...props}>
      <Glyph className="wordmark-glyph" aria-hidden="true" />
      <b className={titleClassName}>
        isoplane<span className="wordmark-dot">.</span>
      </b>
    </a>
  );
}
