// Live regions in a real browser (headless Chromium through Playwright),
// run by scripts/check-realtime.sh: the article list of the cms example
// with a live region following `c:article`. The page opens its event
// stream; the shell edits an article (E2E_EDIT); the region shows the new
// title without a reload, and nothing else on the page changed.
//
//   E2E_EDIT='...' node realtime-e2e.mjs --base http://127.0.0.1:59450 \
//     --path /articles/ --region latest --after 'New title'

import { execSync } from "node:child_process";
import { chromium } from "playwright";

const argv = process.argv.slice(2);
const opt = (n) => {
  const i = argv.indexOf(n);
  if (i < 0) throw new Error(`missing ${n}`);
  return argv[i + 1];
};
const base = opt("--base");
const path = opt("--path");
const region = opt("--region");
const after = opt("--after");
let step = 0;
const ok = (msg) => console.log(`ok ${++step} - ${msg}`);
const fail = (msg) => {
  console.error(`not ok ${++step} - ${msg}`);
  process.exitCode = 1;
  throw new Error(msg);
};
const expect = (cond, msg) => (cond ? ok(msg) : fail(msg));

const browser = await chromium.launch();
const context = await browser.newContext({ reducedMotion: "reduce" });
const page = await context.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
page.on("console", (m) => {
  if (m.type() === "error") errors.push(m.text());
});
let streams = 0;
page.on("request", (r) => {
  if (new URL(r.url()).pathname === "/_engine/live") streams++;
});
try {
  const opened = page.waitForResponse(
    (r) => new URL(r.url()).pathname === "/_engine/live" && r.status() === 200,
    { timeout: 15000 },
  );
  await page.goto(new URL(path, base).href);
  const res = await opened;
  expect(
    (res.headers()["content-type"] || "").startsWith("text/event-stream"),
    "the page opened its event stream",
  );
  await page.evaluate(() => {
    window.__notReloaded = true;
  });
  const sel = `#${region}`;
  const before = await page.textContent(sel);
  expect(!before.includes(after), `the region does not show "${after}" yet`);
  const outside = await page.evaluate((sel) => {
    const r = document.querySelector(sel);
    return document.querySelector("main").textContent.replace(r.textContent, "");
  }, sel);
  const t0 = Date.now();
  execSync(process.env.E2E_EDIT, { stdio: "inherit", shell: "/bin/sh" });
  await page.waitForFunction(
    ({ sel, after }) => document.querySelector(sel)?.textContent.includes(after),
    { sel, after },
    { timeout: 15000 },
  );
  ok(`the region shows the edit ${Date.now() - t0} ms after it was saved`);
  expect(await page.evaluate(() => window.__notReloaded === true), "without a reload");
  expect(
    !(await page.evaluate((sel) => document.querySelector(sel).hasAttribute("aria-busy"), sel)),
    "the region is not left busy",
  );
  const outsideNow = await page.evaluate((sel) => {
    const r = document.querySelector(sel);
    return document.querySelector("main").textContent.replace(r.textContent, "");
  }, sel);
  // The cards below the region are not live: they keep what was rendered.
  expect(outsideNow === outside, "only the region changed");
  expect(streams === 1, `one stream for the page (${streams})`);
  expect(errors.length === 0, `no script errors${errors.length ? ": " + errors.join("; ") : ""}`);
} finally {
  await browser.close();
}
