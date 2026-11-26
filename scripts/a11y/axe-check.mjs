// axe-core over a running site, the second opinion next to TagCheck.
// Run by scripts/check-a11y.sh; needs `playwright` and `@axe-core/playwright`
// (installed by the script into its tools folder).
//
//   node axe-check.mjs --base http://127.0.0.1:59310 --paths paths.txt
//        [--login email --password-env VAR] [--crawl /admin/] [--save dir]
//
// Every page is checked in light and dark colour schemes at a desktop
// width and in the light scheme at a phone width, against WCAG 2.0/2.1/2.2
// A and AA. `--crawl` follows
// same-origin links under that prefix from the paths given (signed in
// first with `--login`). `--save` writes each page's server HTML (as
// fetched, before scripts) under that folder, for TagCheck. Exit code 1
// when axe finds violations.

import { chromium } from "playwright";
import { AxeBuilder } from "@axe-core/playwright";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const argv = process.argv.slice(2);
const opt = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
};
const base = opt("--base");
const pathsFile = opt("--paths");
const login = opt("--login");
const passwordEnv = opt("--password-env");
const crawl = opt("--crawl");
const save = opt("--save");
const maxPages = Number(opt("--max") ?? 60);
if (!base || !pathsFile) {
  console.error("usage: axe-check.mjs --base URL --paths FILE [...]");
  process.exit(2);
}

const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const VIEWS = [
  { name: "desktop light", viewport: { width: 1280, height: 900 }, colorScheme: "light" },
  { name: "desktop dark", viewport: { width: 1280, height: 900 }, colorScheme: "dark" },
  { name: "phone light", viewport: { width: 390, height: 844 }, colorScheme: "light" },
];
// Never follow these while crawling: they change state or are not pages.
const SKIP = [/\/logout\//, /\/delete\//, /\.csv$/, /\/file$/, /\/retry\//, /\/revoke\//, /\/restore\//, /\/preview\//];

const browser = await chromium.launch();
const context = await browser.newContext();
let failures = 0;

if (login) {
  const page = await context.newPage();
  await page.goto(new URL("/account/login/", base).href);
  await page.fill('input[name="email"]', login);
  await page.fill('input[name="password"]', process.env[passwordEnv] ?? "");
  await Promise.all([page.waitForLoadState("load"), page.click('button[type="submit"]')]);
  if (new URL(page.url()).pathname.startsWith("/account/login/")) {
    console.error(`axe-check: could not sign in as ${login}`);
    process.exit(2);
  }
  await page.close();
}

const queue = readFileSync(pathsFile, "utf8").split("\n").map((s) => s.trim()).filter(Boolean);
const seen = new Set();
let checked = 0;
while (queue.length && checked < maxPages) {
  const path = queue.shift();
  if (seen.has(path)) continue;
  seen.add(path);
  const url = new URL(path, base).href;
  const res = await context.request.get(url);
  const type = res.headers()["content-type"] ?? "";
  if (!type.startsWith("text/html")) continue;
  checked++;
  const html = await res.text();
  if (save) {
    const file = join(save, path.replace(/^\//, ""), path.endsWith("/") ? "index.html" : "");
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file.endsWith(".html") ? file : `${file}.html`, html);
  }
  for (const view of VIEWS) {
    const page = await context.newPage({});
    await page.setViewportSize(view.viewport);
    await page.emulateMedia({ colorScheme: view.colorScheme });
    await page.goto(url, { waitUntil: "load" });
    // Let TagAction finish and transitions settle.
    await page.waitForTimeout(150);
    const result = await new AxeBuilder({ page }).withTags(TAGS).analyze();
    for (const v of result.violations) {
      failures += v.nodes.length;
      console.log(`${path} [${view.name}] ${v.impact ?? ""} ${v.id}: ${v.help}`);
      const targets = v.nodes.slice(0, 3).map((n) => n.target.join(" ")).join(" | ");
      console.log(`    ${v.nodes.length} node(s): ${targets}`);
    }
    if (crawl && view === VIEWS[0]) {
      const links = await page.$$eval("a[href]", (as) => as.map((a) => a.href));
      for (const l of links) {
        const u = new URL(l);
        if (u.origin !== new URL(base).origin) continue;
        const p = u.pathname + u.search;
        if (!u.pathname.startsWith(crawl) || SKIP.some((r) => r.test(u.pathname))) continue;
        if (!seen.has(p)) queue.push(p);
      }
    }
    await page.close();
  }
}
await browser.close();
console.log(`axe-check: ${checked} page(s) in ${VIEWS.length} views, ${failures} violation node(s) on ${base}`);
process.exit(failures ? 1 : 0);
