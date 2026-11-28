/* Hammerwright live_region component: parts of a page ([data-live]) that
   refresh themselves when the data under their cache tags changes.

   One event stream per page for all its regions (/_engine/live, server-sent
   events; a region with data-live-endpoint uses that handler of the site
   instead, one stream per endpoint). On an event for one of a region's tags the region's content is
   fetched again (data-live-src, or this page; revalidated, so a published
   page costs a 200 only when it changed) and the children of the element
   with the region's id replace its own. A burst of events is one fetch.

   With htmx on the page, a region with hx-get gets an htmx `live` trigger
   instead (hx-trigger="live"); data-live-swap="none" leaves the region to
   the page's own script, which listens for the `hammerwright:live` event
   (detail: {tag, kind, payload}). Every region gets that event.

   When the server says it goes idle (`bye`), the stream stays closed until
   the visitor does something, so an idle site can stop. No dependencies,
   no inline script. */
const ENDPOINT = "/_engine/live";
const ACTIVITY = ["pointerdown", "keydown", "focusin", "visibilitychange", "scroll"];
const queued = new Map();

const regions = () => [...document.querySelectorAll("[data-live]")];
const tagsOf = (el) => (el.dataset.live || "").split(/[\s,]+/).filter(Boolean);
const endpointOf = (el) => el.dataset.liveEndpoint || ENDPOINT;

function refresh(el, detail) {
  el.dispatchEvent(new CustomEvent("hammerwright:live", { bubbles: true, detail }));
  if (el.dataset.liveSwap === "none") return;
  if (window.htmx && el.hasAttribute("hx-get")) {
    window.htmx.trigger(el, "live", detail);
    return;
  }
  clearTimeout(queued.get(el));
  queued.set(el, setTimeout(() => fetchInto(el), 50));
}

async function fetchInto(el) {
  queued.delete(el);
  const src = el.dataset.liveSrc || location.pathname + location.search;
  el.setAttribute("aria-busy", "true");
  try {
    const r = await fetch(src, {
      cache: "no-cache",
      credentials: "same-origin",
      headers: { "X-Hammerwright-Live": "1" },
    });
    if (!r.ok) return;
    const doc = new DOMParser().parseFromString(await r.text(), "text/html");
    const fresh = (el.id && doc.getElementById(el.id)) || (el.dataset.liveSrc ? doc.body : null);
    if (!fresh) return;
    // Content, not code: scripts in it never run.
    for (const s of fresh.querySelectorAll("script")) s.remove();
    el.replaceChildren(...[...fresh.childNodes].map((n) => document.importNode(n, true)));
  } catch {
    // Offline or refused: the next event tries again.
  } finally {
    el.removeAttribute("aria-busy");
  }
}

// One stream: the regions of one endpoint.
class Stream {
  constructor(endpoint) {
    this.endpoint = endpoint;
    this.source = null;
    this.last = "";
    this.backoff = 1000;
    this.timer = 0;
  }

  regions() {
    return regions().filter((el) => endpointOf(el) === this.endpoint);
  }

  close() {
    if (this.source) this.source.close();
    this.source = null;
    clearTimeout(this.timer);
  }

  retry(ms) {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.connect(), ms);
  }

  // The server went idle: reconnect when the visitor is back.
  rest() {
    const wake = () => {
      if (document.visibilityState === "hidden") return;
      for (const t of ACTIVITY) removeEventListener(t, wake, true);
      this.connect();
    };
    for (const t of ACTIVITY) addEventListener(t, wake, { capture: true, passive: true });
  }

  connect() {
    this.close();
    const tags = new Set();
    for (const el of this.regions()) for (const t of tagsOf(el)) tags.add(t);
    if (!tags.size) return;
    const url = new URL(this.endpoint, location.href);
    url.searchParams.set("tags", [...tags].sort().join(","));
    if (this.last) url.searchParams.set("last", this.last);
    const es = new EventSource(url.href);
    this.source = es;
    es.addEventListener("open", () => {
      this.backoff = 1000;
    });
    es.addEventListener("live", (e) => {
      if (e.lastEventId) this.last = e.lastEventId;
      let d;
      try {
        d = JSON.parse(e.data);
      } catch {
        return;
      }
      for (const el of this.regions()) if (tagsOf(el).includes(d.tag)) refresh(el, d);
    });
    es.addEventListener("reset", (e) => {
      if (e.lastEventId) this.last = e.lastEventId;
      for (const el of this.regions()) refresh(el, { kind: "reset" });
    });
    es.addEventListener("bye", (e) => {
      let idle = false;
      try {
        idle = JSON.parse(e.data).idle === true;
      } catch {
        // A restart.
      }
      this.close();
      if (idle) this.rest();
      else this.retry(2000 + Math.random() * 3000);
    });
    es.addEventListener("error", () => {
      // While CONNECTING the browser tries again itself (with Last-Event-ID).
      if (es.readyState !== EventSource.CLOSED || this.source !== es) return;
      this.close();
      this.retry(this.backoff + Math.random() * 1000);
      this.backoff = Math.min(this.backoff * 2, 300000);
    });
  }
}

for (const endpoint of new Set(regions().map(endpointOf))) new Stream(endpoint).connect();
