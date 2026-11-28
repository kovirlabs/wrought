<script lang="ts">
  // The live preview: the page as the engine renders it from the unsaved
  // blocks, in a same-origin <iframe>. Each change is posted (debounced) to
  // the preview endpoint, which validates and renders it exactly like a
  // save; the new page loads into a second, hidden frame and the two swap
  // when it is ready, keeping the scroll position (no flash). Blocks drawn
  // by render_block are marked in the frame (the engine wraps them in
  // comments naming their ids): clicking one selects it; the selected one
  // gets a small toolbar to move it, or a handle to drag it.
  import { ApiError } from "./api";
  import { newId } from "./blocks";
  import type { Editor } from "./state.svelte";

  let { ed }: { ed: Editor } = $props();

  const tab = newId().replace(/[^A-Za-z0-9]/g, "");
  let frames: HTMLIFrameElement[] = $state([]);
  let active = $state(0);
  let seq = 0;
  let shown = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let updating = $state(false);
  let previewProblem = $state("");
  let dragId: string | null = null;

  $effect(() => {
    void ed.revision;
    clearTimeout(timer);
    timer = setTimeout(render, shown === 0 ? 0 : 350);
  });

  $effect(() => {
    const id = ed.selectedId;
    const f = frames[active];
    if (f?.contentDocument) highlight(f.contentDocument, id, true);
  });

  async function render(): Promise<void> {
    if (!ed.item) return;
    const my = ++seq;
    updating = true;
    try {
      const r = await ed.api.preview(tab, ed.snapshot());
      if (my !== seq) return;
      const standby = frames[1 - active];
      if (!standby) return;
      standby.dataset.seq = String(my);
      standby.src = `${r.frame}&r=${my}`;
      previewProblem = "";
    } catch (e) {
      if (my !== seq) return;
      updating = false;
      previewProblem =
        e instanceof ApiError && e.status === 422
          ? `The preview shows the last valid state: ${e.message}`
          : `The preview could not be updated: ${e instanceof Error ? e.message : String(e)}`;
    }
  }

  function loaded(i: number): void {
    const f = frames[i];
    const my = Number(f?.dataset.seq ?? 0);
    if (!f || !my || my < shown) return;
    const doc = f.contentDocument;
    if (!doc) return;
    const old = frames[active];
    const y = old?.contentWindow?.scrollY ?? 0;
    install(doc);
    f.contentWindow?.scrollTo(0, y);
    shown = my;
    active = i;
    if (my === seq) updating = false;
    highlight(doc, ed.selectedId, false);
    readTokens(doc);
  }

  /** Mark each block's elements with its id (from the engine's comments). */
  function install(doc: Document): void {
    const link = doc.createElement("link");
    link.rel = "stylesheet";
    link.href = ed.frameCss;
    doc.head?.append(link);
    const walker = doc.createTreeWalker(doc.body ?? doc, NodeFilter.SHOW_COMMENT);
    const starts: Comment[] = [];
    while (walker.nextNode()) {
      const c = walker.currentNode as Comment;
      if (c.data.startsWith("hammerwright-block:")) starts.push(c);
    }
    for (const c of starts) {
      const id = c.data.slice("hammerwright-block:".length);
      for (let n = c.nextSibling; n; n = n.nextSibling) {
        if (n.nodeType === Node.COMMENT_NODE && (n as Comment).data === `/hammerwright-block:${id}`) break;
        if (n instanceof doc.defaultView!.HTMLElement && !n.matches("link, script")) {
          n.dataset.hammerwrightBlock = id;
        }
      }
    }
    doc.addEventListener("click", onClick, true);
    doc.addEventListener("submit", (e) => e.preventDefault(), true);
    doc.addEventListener("dragover", onDragOver);
    doc.addEventListener("dragleave", clearDrop);
    doc.addEventListener("drop", onDrop);
  }

  function blockOf(t: EventTarget | null): HTMLElement | null {
    const el = t as Element | null;
    return (el?.closest?.("[data-hammerwright-block]") as HTMLElement | null) ?? null;
  }

  function onClick(e: MouseEvent): void {
    const target = e.target as Element | null;
    if (target?.closest?.(".hammerwright-frame-toolbar")) return;
    // Links and buttons of the page do nothing in the preview.
    e.preventDefault();
    const b = blockOf(e.target);
    if (b?.dataset.hammerwrightBlock) {
      ed.select(b.dataset.hammerwrightBlock);
      ed.announce(`${ed.label(ed.find(b.dataset.hammerwrightBlock)!.block)} selected.`);
    }
  }

  function highlight(doc: Document, id: string | null, scroll: boolean): void {
    doc.querySelectorAll("[data-hammerwright-selected]").forEach((el) => el.removeAttribute("data-hammerwright-selected"));
    doc.querySelector(".hammerwright-frame-toolbar")?.remove();
    if (!id) return;
    const els = Array.from(doc.querySelectorAll<HTMLElement>(`[data-hammerwright-block="${CSS.escape(id)}"]`));
    if (!els.length) return;
    els.forEach((el) => el.setAttribute("data-hammerwright-selected", ""));
    const first = els[0];
    if (scroll) {
      const r = first.getBoundingClientRect();
      const h = doc.defaultView?.innerHeight ?? 0;
      if (r.top < 0 || r.top > h - 40) first.scrollIntoView({ block: "center" });
    }
    toolbar(doc, id, first);
  }

  function toolbar(doc: Document, id: string, at: HTMLElement): void {
    const where = ed.find(id);
    if (!where || !ed.writable) return;
    const bar = doc.createElement("div");
    bar.className = "hammerwright-frame-toolbar";
    bar.setAttribute("role", "toolbar");
    bar.setAttribute("aria-label", `${ed.label(where.block)}: move`);
    const button = (text: string, label: string, act: () => void, disabled = false) => {
      const b = doc.createElement("button");
      b.type = "button";
      b.textContent = text;
      b.setAttribute("aria-label", label);
      b.disabled = disabled;
      b.addEventListener("click", (e) => {
        e.preventDefault();
        e.stopPropagation();
        act();
      });
      bar.append(b);
      return b;
    };
    const n = ed.fields[where.field].length;
    button("↑", "Move up", () => ed.moveTo(id, where.index - 1), where.index === 0);
    button("↓", "Move down", () => ed.moveTo(id, where.index + 1), where.index === n - 1);
    const handle = button("⠿ Drag", "Drag to move", () => {});
    handle.draggable = true;
    handle.addEventListener("dragstart", (e) => {
      dragId = id;
      e.dataTransfer?.setData("text/plain", id);
      if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
    });
    handle.addEventListener("dragend", () => {
      dragId = null;
      clearDrop();
    });
    const r = at.getBoundingClientRect();
    const win = doc.defaultView!;
    bar.style.top = `${Math.max(0, r.top + win.scrollY - 36)}px`;
    bar.style.left = `${Math.max(0, r.left + win.scrollX)}px`;
    doc.body.append(bar);
  }

  function clearDrop(): void {
    const doc = frames[active]?.contentDocument;
    doc?.querySelectorAll("[data-hammerwright-drop]").forEach((el) => el.removeAttribute("data-hammerwright-drop"));
  }

  function dropTarget(e: DragEvent): { id: string; after: boolean; el: HTMLElement } | null {
    const b = blockOf(e.target);
    const id = b?.dataset.hammerwrightBlock;
    if (!b || !id || !dragId || id === dragId) return null;
    if (ed.find(id)?.field !== ed.find(dragId)?.field) return null;
    const r = b.getBoundingClientRect();
    return { id, after: e.clientY > r.top + r.height / 2, el: b };
  }

  function onDragOver(e: DragEvent): void {
    const t = dropTarget(e);
    clearDrop();
    if (!t) return;
    e.preventDefault();
    t.el.setAttribute("data-hammerwright-drop", t.after ? "after" : "before");
  }

  function onDrop(e: DragEvent): void {
    const t = dropTarget(e);
    clearDrop();
    if (!t || !dragId) return;
    e.preventDefault();
    const from = ed.find(dragId)!.index;
    let to = ed.find(t.id)!.index + (t.after ? 1 : 0);
    if (from < to) to -= 1;
    ed.moveTo(dragId, to);
    dragId = null;
  }

  /** The site's actual token values, for the swatches. */
  function readTokens(doc: Document): void {
    const style = doc.defaultView?.getComputedStyle(doc.documentElement);
    if (!style || !ed.tokens) return;
    const out: Record<string, string> = {};
    for (const g of ed.tokens.choices) {
      for (const n of g.names) {
        if (n.property) out[n.property] = style.getPropertyValue(n.property).trim();
      }
    }
    ed.resolved = out;
  }
</script>

<section class="preview" aria-labelledby="preview-heading">
  <div class="preview-head">
    <h2 id="preview-heading">Preview</h2>
    <p class="preview-state" aria-live="off">
      {#if updating}Updating…{:else}Up to date{/if}
    </p>
  </div>
  {#if previewProblem}<p class="problem" role="status">{previewProblem}</p>{/if}
  <p class="hint">Click a block in the page to edit it. The selected block has buttons to move it, and a handle to drag it.</p>
  <div class="frames">
    {#each [0, 1] as i (i)}
      <iframe
        bind:this={frames[i]}
        title={i === active ? "Preview of the page with your changes" : "Preview being prepared"}
        class:standby={i !== active}
        inert={i !== active}
        onload={() => loaded(i)}
      ></iframe>
    {/each}
  </div>
</section>
