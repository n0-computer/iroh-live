import { test, expect, Page } from "@playwright/test";
import { expectVideoContent, startTicketPublisher } from "../fixtures/publisher";
import {
  startPrivateRelay,
  stopRelay,
  waitForRelayOutput,
  PrivateRelayInfo,
} from "../fixtures/relay";

/**
 * The viewer's token on a private relay: read from `#jwt=`, removed from the
 * address bar, and sent only to the page's own origin.
 *
 * The relay verifies JWTs with a key the fixture generates and has no public
 * area, so a session without a token is refused.
 */

let relay: PrivateRelayInfo;

test.beforeAll(async () => {
  relay = await startPrivateRelay();
});

test.afterAll(async () => {
  await stopRelay(relay);
});

/** One WebTransport session the page opened, and whether it has closed. */
interface Session {
  url: string;
  closed: boolean;
}

/**
 * Records every WebTransport session the page opens, with the URL it opened.
 *
 * The page's HTTP requests show up in Playwright, but its WebTransport
 * sessions do not, and the session URL is where the viewer puts the token.
 * A relay refuses a session after the handshake, by closing it, so the
 * record notes the close too.
 */
async function recordSessions(page: Page): Promise<() => Promise<Session[]>> {
  await page.addInitScript(() => {
    const sessions: { url: string; closed: boolean }[] = [];
    (window as any).__sessions = sessions;
    const Native = (window as any).WebTransport;
    (window as any).WebTransport = class extends Native {
      constructor(url: string | URL, options?: object) {
        super(url, options);
        const session = { url: String(url), closed: false };
        sessions.push(session);
        const close = () => (session.closed = true);
        (this as any).closed.then(close, close);
      }
    };
  });
  return () => page.evaluate(() => (window as any).__sessions as Session[]);
}

/** Fails the test if any HTTP request the page makes carries `token`. */
function forbidTokenInRequests(page: Page, token: string): string[] {
  const leaked: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes(token)) leaked.push(request.url());
  });
  return leaked;
}

test("a #jwt= token watches a private relay and leaves the address bar", async ({
  page,
}) => {
  page.on("pageerror", (err) => console.log(`BROWSER ERROR: ${err}`));
  const sessions = await recordSessions(page);

  const { publisher, ticket } = await startTicketPublisher("token-test");
  const from = relay.output.text.length;
  try {
    const token = relay.key.sign({ subscribe: ["**"] });
    const leaked = forbidTokenInRequests(page, token);

    await page.goto(
      `http://localhost:${relay.httpPort}/?name=${encodeURIComponent(ticket)}#jwt=${token}`
    );

    // The token leaves the address bar at once, and the shareable link the
    // viewer writes keeps the name.
    expect(page.url()).not.toContain("jwt");
    expect(page.url()).not.toContain(token);
    expect(new URL(page.url()).searchParams.get("name")).toBe(ticket);

    await expect.poll(async () => (await sessions()).length, { timeout: 15_000 }).toBeGreaterThan(0);
    for (const session of await sessions()) {
      expect(new URL(session.url).searchParams.get("jwt")).toBe(token);
    }

    // The token grants reading the ticket's path, so the relay pulls it.
    const canvas = page.locator("moq-watch canvas");
    await expect(canvas).toBeVisible({ timeout: 15_000 });
    await expectVideoContent(canvas);

    expect(page.url()).not.toContain("jwt");
    expect(leaked).toEqual([]);
    expect(relay.output.text.slice(from)).not.toContain("refusing a browser session");
    expect((await sessions()).every((session) => !session.closed)).toBe(true);
  } finally {
    publisher.kill();
  }
});

test("without a token the private relay refuses the viewer", async ({ page }) => {
  const sessions = await recordSessions(page);
  const from = relay.output.text.length;

  await page.goto(`http://localhost:${relay.httpPort}/?name=private-test`);

  await waitForRelayOutput(relay, /refusing a browser session/, from);
  await expect
    .poll(async () => (await sessions()).some((session) => session.closed), { timeout: 15_000 })
    .toBe(true);
  for (const session of await sessions()) {
    expect(new URL(session.url).searchParams.has("jwt")).toBe(false);
  }
});

test("a ?url= on another origin does not receive the token", async ({ page }) => {
  const sessions = await recordSessions(page);
  const token = relay.key.sign({ subscribe: ["**"] });
  const leaked = forbidTokenInRequests(page, token);
  const from = relay.output.text.length;

  // The same relay under another host name is another origin to the page,
  // so the viewer withholds the token, and the relay sees a session without
  // one and refuses it.
  const other = `http://127.0.0.1:${relay.httpPort}/`;
  await page.goto(
    `http://localhost:${relay.httpPort}/?url=${encodeURIComponent(other)}&name=private-test#jwt=${token}`
  );

  expect(page.url()).not.toContain("jwt");
  await expect(page.locator("#now-watching")).toContainText("token not sent");

  await waitForRelayOutput(relay, /refusing a browser session/, from);
  await expect
    .poll(async () => (await sessions()).some((session) => session.closed), { timeout: 15_000 })
    .toBe(true);
  for (const session of await sessions()) {
    expect(new URL(session.url).host).toBe(`127.0.0.1:${relay.httpPort}`);
    expect(new URL(session.url).searchParams.has("jwt")).toBe(false);
    expect(session.url).not.toContain(token);
  }
  expect(leaked).toEqual([]);
});
