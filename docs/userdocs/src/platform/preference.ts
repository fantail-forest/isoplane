/** The operating systems a visitor may develop on. */
export type Platform = 'mac' | 'win' | 'linux';

/** The name shown for each platform. */
export const platformLabels: Record<Platform, string> = {
  mac: 'macOS',
  win: 'Windows',
  linux: 'Linux',
};

/**
 * Browser storage key for the visitor's operating system choice. The
 * documentation site shares this origin and this entry, so a choice made on
 * either site applies to both.
 */
export const platformStorageKey = 'os';

/** Picks the platform that matches the visitor's device, defaulting to Windows. */
export function detectPlatform(platform: string, userAgent: string): Platform {
  const hints = `${platform} ${userAgent}`.toLowerCase();
  if (/windows|win32|win64|android/.test(hints)) return 'win';
  if (/mac|iphone|ipad|ipod|ios/.test(hints)) return 'mac';
  if (/linux|x11|cros|freebsd/.test(hints)) return 'linux';
  return 'win';
}

/**
 * Returns the saved choice, or the platform the browser reports when nothing
 * valid is saved or storage is unavailable. It takes the key and the detector
 * as arguments so that its source runs on its own in an inline script.
 */
export function visitorPlatform(
  storageKey: string,
  detect: (platform: string, userAgent: string) => Platform,
): Platform {
  try {
    const saved = window.localStorage.getItem(storageKey);
    if (saved === 'mac' || saved === 'win' || saved === 'linux') {
      return saved;
    }
  } catch {
    console.warn(
      'Operating system choice could not be read; using the one this browser reports.',
    );
  }
  const hinted = (
    navigator as Navigator & { userAgentData?: { platform?: string } }
  ).userAgentData?.platform;
  return detect(hinted ?? navigator.platform, navigator.userAgent);
}

/** Returns the platform to show this visitor. */
export function restorePlatform(): Platform {
  return visitorPlatform(platformStorageKey, detectPlatform);
}

/** Saves a choice for later visits to either site when browser storage allows. */
export function savePlatform(platform: Platform): void {
  try {
    window.localStorage.setItem(platformStorageKey, platform);
  } catch {
    console.warn(
      'Operating system choice applies to this page but could not be saved.',
    );
  }
}

/** A JavaScript expression for the visitor's platform, for a script that runs before the page paints. */
export const visitorPlatformExpression = `(${visitorPlatform.toString()})(${JSON.stringify(platformStorageKey)},${detectPlatform.toString()})`;
