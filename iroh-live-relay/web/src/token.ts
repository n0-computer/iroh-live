/**
 * The relay token a page was opened with, and the URLs it may travel in.
 *
 * A link carries the token in its fragment, `#jwt=<TOKEN>`, which the browser
 * never sends to a server. `?jwt=` is read too, for older links, but it
 * reaches the server and the logs on the way, so the docs discourage it.
 */

/**
 * Reads the token from the page's address and removes it from the address bar.
 *
 * Removing it keeps the token out of the history, out of a link copied from
 * the address bar, and out of the link the viewer writes when it starts
 * watching.
 */
export function takeToken(): string | undefined {
  const url = new URL(window.location.href);
  const fragment = new URLSearchParams(url.hash.slice(1));
  const token = fragment.get("jwt") || url.searchParams.get("jwt") || undefined;
  if (fragment.has("jwt") || url.searchParams.has("jwt")) {
    fragment.delete("jwt");
    url.searchParams.delete("jwt");
    const rest = fragment.toString();
    url.hash = rest ? `#${rest}` : "";
    history.replaceState(null, "", url.toString());
  }
  return token;
}

/** A relay URL, and whether the page withheld its token from it. */
export interface RelayUrl {
  url: string;
  withheld: boolean;
}

/**
 * Returns the relay URL for `path` below `base`, with `token` as `?jwt=` if the relay is this page's origin.
 *
 * A page can be opened with `?url=` naming any relay, so the token goes only
 * to the origin that served the page, which is the relay the token is for.
 */
export function relayUrl(base: string, path: string, token: string | undefined): RelayUrl {
  const url = new URL(base + path);
  if (!token) return { url: url.toString(), withheld: false };
  if (url.origin !== window.location.origin) return { url: url.toString(), withheld: true };
  url.searchParams.set("jwt", token);
  return { url: url.toString(), withheld: false };
}
