// End-to-end run of the visual editor in a real browser (headless
// Chromium through Playwright): add a block, write rich text, reorder,
// pick a design token, watch the preview follow, click a block in the
// preview, save and publish, reload, lose a race against a second editor
// (version conflict, nothing overwritten), and read the published page.
// Run by scripts/check-editor.sh (which builds the engine, prepares the
// cms example on PostgreSQL and serves it); needs `playwright` from
// target/a11y-tools.
//
//   node editor-e2e.mjs --base http://127.0.0.1:59420 --type article --id 1 \
//        --login a@x --password-env PW --login2 b@x --password2-env PW2

import { chromium } from "playwright";
import { AxeBuilder } from "@axe-core/playwright";

const argv = process.argv.slice(2);
const opt = (n) => {
  const i = argv.indexOf(n);
  return i >= 0 ? argv[i + 1] : undefined;
};
const base = opt("--base");
const type = opt("--type") ?? "article";
const id = opt("--id") ?? "1";
const editorUrl = new URL(`/admin/editor/${type}/${id}/`, base).href;
let step = 0;
const ok = (msg) => console.log(`ok ${++step} - ${msg}`);
const fail = (msg) => {
  console.error(`not ok ${++step} - ${msg}`);
  process.exitCode = 1;
  throw new Error(msg);
};
const expect = (cond, msg) => (cond ? ok(msg) : fail(msg));

const browser = await chromium.launch();

async function signIn(email, pwEnv) {
  // Reduced motion: TagBrush turns on cross-document view transitions
  // otherwise, and headless Chromium stops painting after a same-origin
  // navigation with one (no animation frames, so no clicks).
  const context = await browser.newContext({ viewport: { width: 1400, height: 1000 }, reducedMotion: "reduce" });
  const page = await context.newPage();
  // WROUGHT_E2E_CPU_THROTTLE=6 slows the page's CPU six times (Chromium
  // DevTools throttling), to shake out races a slow CI runner would hit.
  const throttle = Number(process.env.WROUGHT_E2E_CPU_THROTTLE ?? 0);
  if (throttle > 1) {
    const cdp = await context.newCDPSession(page);
    await cdp.send("Emulation.setCPUThrottlingRate", { rate: throttle });
  }
  await page.goto(new URL("/account/login/", base).href);
  await page.fill('input[name="email"]', email);
  await page.fill('input[name="password"]', process.env[pwEnv] ?? "");
  await Promise.all([page.waitForLoadState("load"), page.click('button[type="submit"]')]);
  return page;
}

async function openEditor(page, query = "") {
  await page.goto(editorUrl + query);
  await page.waitForSelector("#wrought-editor[data-ready]", { timeout: 15000 });
  await previewSettled(page);
}

/** Wait until the preview says it is up to date. */
async function previewSettled(page) {
  await page.waitForFunction(
    () => {
      const s = document.querySelector(".preview-state");
      const f = document.querySelector(".preview iframe:not(.standby)");
      return s?.textContent?.includes("Up to date") && f?.contentDocument?.body?.childElementCount > 0;
    },
    null,
    { timeout: 15000, polling: 100 },
  );
}

/** Block ids in page order, as the preview frame marks them. */
async function frameOrder(page) {
  return page.evaluate(() => {
    const doc = document.querySelector(".preview iframe:not(.standby)").contentDocument;
    const ids = [];
    for (const el of doc.querySelectorAll("[data-wrought-block]")) {
      const id = el.dataset.wroughtBlock;
      if (ids.at(-1) !== id) ids.push(id);
    }
    return ids;
  });
}

async function listOrder(page) {
  return page.$$eval(".block-list li .select", (bs) => bs.map((b) => b.id.replace(/^sel-/, "")));
}

const frameHtml = (page) =>
  page.evaluate(() => document.querySelector(".preview iframe:not(.standby)").contentDocument.body.innerHTML);

try {
  const a = await signIn(opt("--login"), opt("--password-env"));
  await openEditor(a);
  const before = await listOrder(a);
  expect(before.length >= 1, `editor loaded with ${before.length} block(s)`);
  expect(JSON.stringify(await frameOrder(a)) === JSON.stringify(before), "preview marks every block, in list order");

  // axe on the editor with a block selected and the rich-text editor open.
  await a.click(`#sel-${before[0]}`);
  const axe = await new AxeBuilder({ page: a }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).exclude(".preview iframe").analyze();
  for (const v of axe.violations) console.log(`   axe: ${v.id}: ${v.help} (${v.nodes.length})`);
  expect(axe.violations.length === 0, "axe finds nothing on the editor with a block open");

  // Add a block from the library, with the keyboard.
  await a.focus("#add-block");
  await a.keyboard.press("Enter");
  await a.waitForSelector("dialog.add-block[open]");
  await a.getByRole("button", { name: /^(Te|TE)?\s*Text$/ }).first().click();
  const added = (await listOrder(a)).find((x) => !before.includes(x));
  expect(!!added, "a Text block was added");
  // Rich text: plain, then bold through the toolbar.
  const rt = a.locator(".panel [contenteditable='true']").first();
  await rt.click();
  const rtFocused = () => document.activeElement?.classList.contains("rich-text-area");
  await a.waitForFunction(rtFocused, null, { timeout: 15000, polling: 50 });
  await a.keyboard.type("Forged in the editor ");
  const bold = a.locator(".panel .rt-toolbar button", { hasText: "Bold" }).first();
  await bold.click();
  // Type only once bold is on and the text has the focus (keys typed
  // before that would not reach it).
  await a.waitForFunction(
    () =>
      document.querySelector(".panel .rt-toolbar button:first-child")?.getAttribute("aria-pressed") === "true" &&
      document.activeElement?.classList.contains("rich-text-area"),
    null,
    { timeout: 15000, polling: 50 },
  );
  await a.keyboard.type("with bold");
  await a.waitForFunction(
    () => /Forged in the editor <strong>with bold/.test(document.querySelector(".preview iframe:not(.standby)")?.contentDocument?.body?.innerHTML ?? ""),
    null,
    { timeout: 15000, polling: 100 },
  ).catch(() => {});
  let html = await frameHtml(a);
  if (!/Forged in the editor <strong>/.test(html)) console.log("   frame:", html.slice(html.indexOf("Forged") - 200, html.indexOf("Forged") + 200), await a.textContent(".preview"));
  expect(/Forged in the editor <strong>with bold<\/strong>/.test(html), "the preview shows the new rich text, bold included");

  // Reorder: move the new block to the top with the keyboard (Alt+↑).
  await a.focus(`#sel-${added}`);
  const n = (await listOrder(a)).length;
  for (let i = 0; i < n; i++) await a.keyboard.press("Alt+ArrowUp");
  await previewSettled(a);
  await a.waitForTimeout(500);
  await previewSettled(a);
  expect((await listOrder(a))[0] === added, "Alt+↑ moved the block to the top of the list");
  expect((await frameOrder(a))[0] === added, "the preview follows the new order");
  const live = await a.textContent(".live");
  expect(/moved to position 1/.test(live ?? ""), `the move was announced ("${live}")`);

  // Move it back down one with the toolbar inside the preview.
  await a.evaluate(() => {
    const doc = document.querySelector(".preview iframe:not(.standby)").contentDocument;
    doc.querySelector('.wrought-frame-toolbar button[aria-label="Move down"]').click();
  });
  await a.waitForTimeout(500);
  await previewSettled(a);
  expect((await frameOrder(a))[1] === added, "the preview's own Move down button moved it");

  // Click a block in the preview: it is selected for editing.
  const panelId = (await a.$$eval(".block-list li .name", (ns) => ns.map((x) => x.textContent))).findIndex((t) => /Panel/.test(t));
  const ids = await listOrder(a);
  if (panelId >= 0) {
    await a.evaluate((pid) => {
      const doc = document.querySelector(".preview iframe:not(.standby)").contentDocument;
      doc.querySelector(`[data-wrought-block="${pid}"]`).click();
    }, ids[panelId]);
    await a.waitForFunction(() => /Panel/.test(document.querySelector("#panel-heading")?.textContent ?? ""), null, { polling: 100 });
    ok("clicking the panel in the preview opened it in the side panel");
    // Design token: pick another background; the preview uses it.
    await a.getByRole("radio", { name: "highlight" }).check();
    await a.waitForTimeout(500);
    await previewSettled(a);
    html = await frameHtml(a);
    expect(html.includes('data-background="highlight"'), "the token choice reached the preview");
    const radios = await a.$$eval(".tokens input[type=radio]", (r) => r.map((x) => x.value));
    expect(radios.every((v) => /^[a-z_]*$/.test(v)), `token choices are names only (${radios.join(", ")})`);
  }

  // Save and publish.
  const pub = a.getByRole("button", { name: /Save and publish|Save and update the site/ });
  await pub.click();
  await a.waitForFunction(() => /Published as version|Saved as version/.test(document.querySelector(".live")?.textContent ?? ""), null, { timeout: 15000, polling: 100 });
  ok(`saved: ${await a.textContent(".live")}`);
  const savedOrder = await listOrder(a);

  // Reload: the same blocks, ids and order come back from the server.
  await openEditor(a);
  expect(JSON.stringify(await listOrder(a)) === JSON.stringify(savedOrder), "after a reload the blocks, ids and order are as saved");

  // A second editor saves first.
  const b = await signIn(opt("--login2"), opt("--password2-env"));
  await openEditor(b, "?block=1");
  await b.locator(".panel [contenteditable='true'], .panel textarea").first().click();
  await b.keyboard.press("End");
  await b.keyboard.type(" (second editor)");
  await previewSettled(b);
  await b.getByRole("button", { name: /^Save/ }).first().click();
  await b.waitForFunction(() => /version/.test(document.querySelector(".live")?.textContent ?? ""), null, { timeout: 15000, polling: 100 });
  ok(`the second editor saved: ${await b.textContent(".live")}`);

  // The first, still on the older version, edits and saves: a conflict.
  await a.click(`#sel-${savedOrder[0]}`);
  await a.locator(".panel [contenteditable='true'], .panel textarea").first().click();
  await a.keyboard.type(" late change");
  await a.getByRole("button", { name: /^Save/ }).first().click();
  await a.waitForSelector("dialog.conflict[open]", { timeout: 15000 });
  const text = await a.textContent("#conflict-text");
  expect(/Nothing was overwritten/.test(text ?? ""), "a stale save opens the conflict dialog; nothing is overwritten");
  const focusInDialog = await a.evaluate(() => !!document.activeElement?.closest("dialog.conflict"));
  expect(focusInDialog, "focus moved into the conflict dialog");
  await a.getByRole("button", { name: "Load their version (discard mine)" }).click();
  await a.waitForFunction(() => !document.querySelector("dialog.conflict[open]"), null, { polling: 100 });
  const theirs = await a
    .waitForFunction(
      () => document.querySelector(".preview iframe:not(.standby)")?.contentDocument?.body?.innerHTML.includes("(second editor)"),
      null,
      { timeout: 15000, polling: 100 },
    )
    .then(() => true, () => false);
  expect(theirs, "loading their version shows the other editor's text");

  // The published page.
  const url = await a.$eval(".bar a[href]", (x) => x.getAttribute("href")).catch(() => null);
  expect(!!url, `the editor links the published page (${url})`);
  let pageHtml = "";
  for (let i = 0; i < 50; i++) {
    const r = await a.context().request.get(new URL(url, base).href, { headers: { cookie: "" } });
    pageHtml = await r.text();
    if (pageHtml.includes("(second editor)")) break;
    await a.waitForTimeout(200);
  }
  expect(/Forged in the editor <strong>with bold<\/strong>/.test(pageHtml), "the published page has the rich text");
  expect(pageHtml.includes("(second editor)"), "the published page has the second editor's save");
  expect(!pageHtml.includes("wrought-block:"), "the published page has no editor markers");
} catch (e) {
  if (!process.exitCode) {
    console.error(e);
    process.exitCode = 1;
  }
} finally {
  await browser.close();
}
