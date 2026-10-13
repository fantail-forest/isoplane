import React, {useId, useSyncExternalStore} from 'react';
import {
  platformLabels,
  platformStorageKey,
  restorePlatform,
  savePlatform,
} from '../platform/preference';

// The chosen platform lives on the root element, where the inline script in
// docusaurus.config.js puts it before the page paints. The stylesheet hides
// other platforms' content from there, so the page never shows all three.
const changed = 'isoplane-platform-change';
const root = () => document.documentElement;

function subscribe(onChange) {
  const synchronize = (event) => {
    if (event.key === platformStorageKey || event.key === null) {
      root().dataset.os = restorePlatform();
      onChange();
    }
  };
  window.addEventListener(changed, onChange);
  window.addEventListener('storage', synchronize);
  return () => {
    window.removeEventListener(changed, onChange);
    window.removeEventListener('storage', synchronize);
  };
}

function choose(platform) {
  root().dataset.os = platform;
  savePlatform(platform);
  window.dispatchEvent(new Event(changed));
}

/** Buttons that choose which operating system's instructions the page shows. */
export function PlatformToggle() {
  const chosen = useSyncExternalStore(subscribe, () => root().dataset.os, () => undefined);
  const label = useId();
  return (
    <div className="platform-toggle" role="group" aria-labelledby={label}>
      <span id={label}>Show instructions for</span>
      {Object.entries(platformLabels).map(([platform, label]) => (
        <button
          key={platform}
          type="button"
          data-platform={platform}
          aria-pressed={chosen === platform}
          onClick={() => choose(platform)}>
          {label}
        </button>
      ))}
    </div>
  );
}

/**
 * Content for the platforms named in `platforms`, separated by spaces: `mac`,
 * `win` and `linux`. A page viewed without JavaScript shows every block, each
 * under the names of its platforms.
 */
export function OnlyOn({platforms, children}) {
  const names = platforms.split(' ').map((platform) => platformLabels[platform]);
  return (
    <div data-only-on={platforms} data-platform-names={names.join(' and ')}>
      {children}
    </div>
  );
}
