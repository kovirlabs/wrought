// Page weight of a few URLs in headless Chromium: what a first visit with
// an empty cache transfers, by kind, compressed (bytes on the wire, as the
// server sent them) and uncompressed, with the request count and JS bytes.
// Run by bench/run.py with the Playwright install of scripts/check-a11y.sh
// (target/a11y-tools):
//
//   node pageweight.mjs URL... > weights.json
//
// One fresh browser context per URL (no cache, no cookies), a desktop
// viewport, `reducedMotion: "reduce"`, and the page counted once the
// network has been idle for half a second.

import { chromium } from "playwright";

const urls = process.argv.slice(2);
const kindOf = (type, mime) => {
  if (type === "document") return "html";
  if (type === "stylesheet") return "css";
  if (type === "script") return "js";
  if (type === "image" || (mime || "").startsWith("image/")) return "images";
  if (type === "font" || (mime || "").startsWith("font/")) return "fonts";
  return "other";
};

const browser = await chromium.launch();
const out = {};
for (const url of urls) {
  const ctx = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    reducedMotion: "reduce",
  });
  const page = await ctx.newPage();
  const responses = [];
  page.on("response", (r) => responses.push(r));
  await page.goto(url, { waitUntil: "networkidle" });
  await page.waitForTimeout(500);
  const kinds = {};
  let requests = 0;
  for (const r of responses) {
    const req = r.request();
    if (req.url().startsWith("data:")) continue;
    requests += 1;
    const mime = (r.headers()["content-type"] || "").split(";")[0];
    const k = kindOf(req.resourceType(), mime);
    let wire = 0;
    let body = 0;
    try {
      const sizes = await req.sizes();
      wire = sizes.responseBodySize;
    } catch {}
    try {
      body = (await r.body()).length;
    } catch {}
    kinds[k] ??= { requests: 0, transferred: 0, uncompressed: 0 };
    kinds[k].requests += 1;
    kinds[k].transferred += wire;
    kinds[k].uncompressed += body;
  }
  const sum = (f) => Object.values(kinds).reduce((a, k) => a + k[f], 0);
  out[url] = {
    requests,
    transferred: sum("transferred"),
    uncompressed: sum("uncompressed"),
    js_bytes: kinds.js ? kinds.js.uncompressed : 0,
    kinds,
  };
  await ctx.close();
}
await browser.close();
console.log(JSON.stringify(out, null, 1));
