import React from 'react';
import clsx from 'clsx';

/**
 * A navbar link to a page of the marketing site. The two sites share an origin
 * but not a router, so this is a plain anchor to a root-relative address,
 * which a Docusaurus link would prefix with the docs base URL.
 */
export default function SiteLink({href, label, className, mobile = false, onClick}) {
  const link = (
    <a
      href={href}
      className={clsx(mobile ? 'menu__link' : 'navbar__item navbar__link', className)}
      onClick={onClick}>
      {label}
    </a>
  );
  return mobile ? <li className="menu__list-item">{link}</li> : link;
}
