<script lang="ts">
  // The blocks of the active field, in order. Every action has a button
  // (keyboard first); blocks can also be dragged, and Alt+↑/↓ moves the
  // focused block. Moves are announced.
  import { tick } from "svelte";
  import { summary } from "./blocks";
  import type { Editor } from "./state.svelte";

  let { ed, onadd }: { ed: Editor; onadd: () => void } = $props();

  let dragging: string | null = $state(null);
  let over: { id: string; after: boolean } | null = $state(null);

  async function focusSelect(id: string): Promise<void> {
    await tick();
    document.getElementById(`sel-${id}`)?.focus();
  }

  async function moveBy(id: string, by: number, focusId: string): Promise<void> {
    const w = ed.find(id);
    if (!w) return;
    ed.moveTo(id, w.index + by);
    await tick();
    document.getElementById(focusId)?.focus();
  }

  function keys(e: KeyboardEvent, id: string): void {
    if (!e.altKey || !ed.writable) return;
    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      void moveBy(id, e.key === "ArrowUp" ? -1 : 1, `sel-${id}`);
    }
  }

  function remove(id: string): void {
    const w = ed.find(id);
    ed.remove(id);
    const next = w ? ed.blocks[Math.min(w.index, ed.blocks.length - 1)] : undefined;
    if (next) void focusSelect(next.id);
    else void tick().then(() => document.getElementById("add-block")?.focus());
  }

  function dragStart(e: DragEvent, id: string): void {
    dragging = id;
    e.dataTransfer?.setData("text/plain", id);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
  }

  function dragOver(e: DragEvent, id: string): void {
    if (!dragging || dragging === id) return;
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    over = { id, after: e.clientY > r.top + r.height / 2 };
  }

  function drop(e: DragEvent): void {
    e.preventDefault();
    if (dragging && over) {
      const from = ed.find(dragging)!.index;
      let to = ed.find(over.id)!.index + (over.after ? 1 : 0);
      if (from < to) to -= 1;
      ed.moveTo(dragging, to);
    }
    dragging = null;
    over = null;
  }
</script>

<div class="block-list">
  {#if ed.blocks.length === 0}
    <p>No blocks yet.</p>
  {:else}
    <ol aria-label="Blocks of {ed.item?.block_fields.find((f) => f.field === ed.activeField)?.label ?? ed.activeField}">
      {#each ed.blocks as b, i (b.id)}
        {@const t = ed.typeOf(b)}
        <li
          class:selected={b.id === ed.selectedId}
          class:drop-before={over?.id === b.id && !over.after}
          class:drop-after={over?.id === b.id && over.after}
          draggable={ed.writable}
          ondragstart={(e) => dragStart(e, b.id)}
          ondragover={(e) => dragOver(e, b.id)}
          ondragleave={() => (over = null)}
          ondrop={drop}
          ondragend={() => { dragging = null; over = null; }}
        >
          <button
            type="button"
            class="select"
            id="sel-{b.id}"
            aria-pressed={b.id === ed.selectedId}
            aria-describedby="keys-help"
            onclick={() => ed.select(b.id)}
            onkeydown={(e) => keys(e, b.id)}
          >
            <span class="icon" aria-hidden="true">{(t?.editor?.icon ?? t?.label ?? b.type).slice(0, 2)}</span>
            <span class="name">{i + 1}. {t?.label ?? b.type}</span>
            {#if summary(b, t)}<span class="summary">{summary(b, t)}</span>{/if}
          </button>
          {#if ed.writable}
            <span class="actions">
              <button type="button" id="up-{b.id}" disabled={i === 0} onclick={() => moveBy(b.id, -1, i - 1 === 0 ? `down-${b.id}` : `up-${b.id}`)}>Move up<span class="visually-hidden">: {t?.label} {i + 1}</span></button>
              <button type="button" id="down-{b.id}" disabled={i === ed.blocks.length - 1} onclick={() => moveBy(b.id, 1, i + 2 === ed.blocks.length ? `up-${b.id}` : `down-${b.id}`)}>Move down<span class="visually-hidden">: {t?.label} {i + 1}</span></button>
              <button type="button" onclick={() => { ed.duplicate(b.id); void focusSelect(ed.selectedId!); }}>Copy<span class="visually-hidden">: {t?.label} {i + 1}</span></button>
              <button type="button" onclick={() => remove(b.id)}>Remove<span class="visually-hidden">: {t?.label} {i + 1}</span></button>
            </span>
          {/if}
        </li>
      {/each}
    </ol>
    <p id="keys-help" class="hint">Select a block to edit it. Alt+↑ and Alt+↓ move the selected block; you can also drag blocks here or in the preview.</p>
  {/if}
  {#if ed.writable}
    <p><button type="button" id="add-block" onclick={onadd}>Add a block</button></p>
  {/if}
</div>
