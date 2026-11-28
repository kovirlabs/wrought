<script lang="ts">
  import { onMount, tick } from "svelte";
  import AddBlock from "./AddBlock.svelte";
  import BlockList from "./BlockList.svelte";
  import BlockPanel from "./BlockPanel.svelte";
  import ConflictDialog from "./ConflictDialog.svelte";
  import Preview from "./Preview.svelte";
  import type { Editor } from "./state.svelte";

  let { ed }: { ed: Editor } = $props();
  let library: AddBlock | undefined = $state();

  onMount(() => {
    void ed.load().then(async () => {
      // `?block=N` selects the N-th block of the first field (links, tests).
      const n = Number(new URLSearchParams(location.search).get("block") ?? 0);
      const b = n > 0 ? ed.blocks[n - 1] : undefined;
      if (b) ed.select(b.id);
      await tick();
      document.getElementById("hammerwright-editor")?.setAttribute("data-ready", "");
    });
    const warn = (e: BeforeUnloadEvent) => {
      if (ed.dirty) e.preventDefault();
    };
    addEventListener("beforeunload", warn);
    const undoKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      const typing = el?.closest?.("input, textarea, select, [contenteditable='true']");
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key === "z" && !typing && ed.canUndo) {
        e.preventDefault();
        ed.undo();
      }
    };
    addEventListener("keydown", undoKey);
    return () => {
      removeEventListener("beforeunload", warn);
      removeEventListener("keydown", undoKey);
    };
  });

  async function added(id: string): Promise<void> {
    await tick();
    const first = document.querySelector<HTMLElement>(
      `.panel input:not([type=hidden]), .panel textarea, .panel select, .panel [contenteditable='true']`,
    );
    (first ?? document.getElementById(`sel-${id}`))?.focus();
  }

  async function save(publish: boolean): Promise<void> {
    await ed.save(publish);
  }
</script>

{#if ed.loadError}
  <p role="alert">The editor could not load this item: {ed.loadError} <a href={ed.editUrl}>Edit it with the form</a>.</p>
{:else if !ed.item}
  <p role="status">Loading the editor…</p>
{:else}
  <div class="editor">
    <div class="bar">
      <p class="state">
        Version {ed.version}, {ed.status}{#if ed.dirty}, <strong>unsaved changes</strong>{/if}
        {#if ed.url && ed.status === "published"} · <a href={ed.url} target="_blank" rel="noopener">View the published page</a>{/if}
      </p>
      <div class="buttons">
        <button type="button" disabled={!ed.canUndo} onclick={() => ed.undo()}>Undo{#if ed.undoLabel}<span class="visually-hidden">: {ed.undoLabel}</span>{/if}</button>
        <button type="button" disabled={ed.saving || !ed.dirty} onclick={() => save(false)}>{ed.status === "published" ? "Save and update the site" : "Save draft"}</button>
        {#if ed.item.can_publish && ed.status !== "published"}
          <button type="button" class="primary" disabled={ed.saving} onclick={() => save(true)}>Save and publish</button>
        {/if}
      </div>
    </div>
    <div class="live" role="status" aria-live="polite">{ed.announcement}</div>
    {#if ed.problem}<p class="problem" role="alert">{ed.problem}</p>{/if}

    {#if ed.item.block_fields.length > 1}
      <div class="field-tabs" role="group" aria-label="Block fields">
        {#each ed.item.block_fields as bf (bf.field)}
          <button type="button" aria-pressed={bf.field === ed.activeField} onclick={() => { ed.activeField = bf.field; ed.select(null); }}>{bf.label}</button>
        {/each}
      </div>
    {/if}

    <div class="columns">
      <div class="side">
        <section aria-labelledby="blocks-heading">
          <h2 id="blocks-heading">Blocks</h2>
          {#if !ed.writable}<p>Your roles let you read these blocks but not change them.</p>{/if}
          <BlockList {ed} onadd={() => library?.open()} />
        </section>
        <BlockPanel {ed} />
      </div>
      <Preview {ed} />
    </div>
  </div>
  <AddBlock bind:this={library} {ed} onadded={added} />
  <ConflictDialog {ed} />
{/if}
