// Browser caching, end to end, in a real browser (headless Chromium
// through Playwright): a page refresh is all it takes to see an edit, and
// nothing more is fetched than needed. Run by scripts/check-caching.sh,
// which serves the cms example (through `engine serve`, then through the
// shipped Caddyfile.publish) and passes the commands that edit it.
//
// 1. A published article: load it, refresh. The shell then edits the
//    article (title, published) and the stylesheet linked through
//    `asset()`, and waits until both are published. One normal refresh
//    shows the new title in the new colour, with a new stylesheet URL.
//    A second refresh: the page is revalidated (304) and no content-hashed
//    file is fetched from the network.
// 2. A page rendered per request (search, `?q=`): a refresh is a 304; after
//    the shell publishes another title, one refresh shows it; the next is a
//    304 again.
//
//   E2E_EDIT_ARTICLE='...' E2E_EDIT_SEARCH='...' node caching-e2e.mjs \
//     --base http://127.0.0.1:59430 --article /articles/x/ \
//     --title-before 'Old' --title-after 'New' --color-before 'rgb(1, 2, 3)' \
//     --color-after 'rgb(4, 5, 6)' --search '/search/?q=word' --search-after 'Newer'

import { execSync } from "node:child_process";
import { chromium } from "playwright";

const argv = process.argv.slice(2);
const opt = (n) => {
  const i = argv.indexOf(n);
  if (i < 0) throw new Error(`missing ${n}`);
  return argv[i + 1];
};
const base = opt("--base");
const url = (p) => new URL(p, base).href;
let step = 0;
const ok = (msg) => console.log(`ok ${++step} - ${msg}`);
const fail = (msg) => {
  console.error(`not ok ${++step} - ${msg}`);
  process.exitCode = 1;
  throw new Error(msg);
};
const expect = (cond, msg) => (cond ? ok(msg) : fail(msg));
// The rule of engine_core::assets::is_fingerprinted.
const hashed = (u) => {
  const m = new URL(u).pathname.match(/[.-]([0-9a-f]{8,})\.[0-9A-Za-z]+$/);
  return !!m && /[0-9]/.test(m[1]) && /[a-f]/.test(m[1]);
};

const browser = await chromium.launch();
// Reduced motion: TagBrush's cross-document view transitions otherwise
// stop headless painting after a same-origin navigation.
const context = await browser.newContext({ reducedMotion: "reduce" });
const page = await context.newPage();

// What the network did, from the DevTools protocol: one record per request
// of the last navigation (status as the browser saw it: a revalidated page
// is a 304; a file taken from the cache is marked so).
const cdp = await context.newCDPSession(page);
await cdp.send("Network.enable");
let log = new Map();
cdp.on("Network.requestWillBeSent", (e) => {
  if (!e.request.url.startsWith(base)) return;
  log.set(e.requestId, { url: e.request.url, type: e.type, cached: false, status: 0, wire: 0 });
});
cdp.on("Network.requestServedFromCache", (e) => {
  const r = log.get(e.requestId);
  if (r) r.cached = true;
});
// The status on the wire: a revalidated page is a 200 to the page but a
// 304 here (only requests that reached the network have one).
cdp.on("Network.responseReceivedExtraInfo", (e) => {
  const r = log.get(e.requestId);
  if (r) r.wire = e.statusCode;
});
cdp.on("Network.responseReceived", (e) => {
  const r = log.get(e.requestId);
  if (!r) return;
  r.status = e.response.status;
  if (e.response.fromDiskCache || e.response.fromMemoryCache || e.response.fromPrefetchCache) r.cached = true;
});
async function visit(how) {
  log = new Map();
  await how();
  await page.waitForLoadState("load");
  // Let late responses' events arrive.
  await page.waitForTimeout(300);
  return [...log.values()];
}
// The page's request; `status` is what reached the network (304 when
// revalidated).
const doc = (reqs) => {
  const d = reqs.find((r) => r.type === "Document");
  return d && { ...d, status: d.wire || d.status };
};
const assetsOf = async () =>
  page.$$eval('link[rel="stylesheet"][href], script[src]', (els) => els.map((e) => e.href ?? e.src));
const h1 = async () =>
  page.$eval("h1", (e) => ({ text: e.textContent.trim(), color: getComputedStyle(e).color }));
const show = (reqs) =>
  reqs.map((r) => `${r.wire || r.status}${r.cached ? "(cache)" : ""} ${new URL(r.url).pathname}`).join(", ");

// 1. A published page and its assets.
await visit(() => page.goto(url(opt("--article"))));
let t = await h1();
expect(t.text === opt("--title-before"), `the article shows its title (${t.text})`);
expect(t.color === opt("--color-before"), `the asset() stylesheet applies (${t.color})`);
const before = await assetsOf();
expect(before.length >= 3 && before.every(hashed), `every stylesheet and script URL is content-hashed (${before.join(" ")})`);
let reqs = await visit(() => page.reload());
expect(doc(reqs)?.status === 304, `a refresh of the unchanged page is a 304 (${show(reqs)})`);

execSync(process.env.E2E_EDIT_ARTICLE, { stdio: "inherit", shell: "/bin/sh" });

reqs = await visit(() => page.reload());
t = await h1();
expect(doc(reqs)?.status === 200, `after the edit, one refresh fetches the page (${show(reqs)})`);
expect(t.text === opt("--title-after"), `one refresh shows the new title (${t.text})`);
expect(t.color === opt("--color-after"), `one refresh shows the new stylesheet (${t.color})`);
const after = await assetsOf();
const changed = after.filter((a) => !before.includes(a));
expect(changed.length === 1 && changed[0].includes("/css/check."), `only the edited stylesheet got a new URL (${changed.join(" ")})`);
const refetched = reqs.filter((r) => hashed(r.url) && !r.cached && !changed.includes(r.url));
expect(refetched.length === 0, `unchanged hashed files came from the cache (${show(reqs)})`);

reqs = await visit(() => page.reload());
expect(doc(reqs)?.status === 304, `a second refresh is a 304 (${show(reqs)})`);
const fetched = reqs.filter((r) => hashed(r.url) && !r.cached);
expect(fetched.length === 0, `a second refresh fetches no hashed file from the network (${show(reqs)})`);
t = await h1();
expect(t.text === opt("--title-after") && t.color === opt("--color-after"), "and still shows the edit");

// 2. A page rendered per request.
await visit(() => page.goto(url(opt("--search"))));
reqs = await visit(() => page.reload());
expect(doc(reqs)?.status === 304, `a refresh of an unchanged request-mode page is a 304 (${show(reqs)})`);

execSync(process.env.E2E_EDIT_SEARCH, { stdio: "inherit", shell: "/bin/sh" });

reqs = await visit(() => page.reload());
expect(doc(reqs)?.status === 200, `after a publish, one refresh renders it again (${show(reqs)})`);
const results = await page.textContent("main");
expect(results.includes(opt("--search-after")), "the request-mode page shows the published change");
reqs = await visit(() => page.reload());
expect(doc(reqs)?.status === 304, `the next refresh is a 304 again (${show(reqs)})`);
expect(reqs.filter((r) => hashed(r.url) && !r.cached).length === 0, "with no hashed file fetched");

await browser.close();
console.log(`== browser caching: ${step} checks passed against ${base}`);
