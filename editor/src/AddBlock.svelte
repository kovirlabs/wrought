<script lang="ts">
  // The block library: every block type the field allows, as the server
  // describes it (label, description, icon, thumbnail from the block type
  // file or the component manifest). A native modal <dialog>: focus moves
  // in and returns, Escape closes.
  import type { BlockType } from "./api";
  import type { Editor } from "./state.svelte";

  let { ed, onadded }: { ed: Editor; onadded: (id: string) => void } = $props();
  let dialog: HTMLDialogElement;

  export function open(): void {
    dialog.showModal();
  }

  function pick(t: BlockType): void {
    const b = ed.add(t);
    dialog.close();
    onadded(b.id);
  }
</script>

<dialog bind:this={dialog} aria-labelledby="add-heading" class="add-block">
  <h2 id="add-heading">Add a block</h2>
  <p>It goes after the selected block, or at the end.</p>
  <ul class="library">
    {#each ed.allowedTypes as t (t.name)}
      <li>
        <button type="button" onclick={() => pick(t)} aria-describedby={t.description ? `desc-${t.name}` : undefined}>
          {#if t.editor?.thumbnail}<img src={t.editor.thumbnail} alt="" width="64" height="40" />{:else}<span class="icon" aria-hidden="true">{(t.editor?.icon ?? t.label).slice(0, 2)}</span>{/if}
          <span class="name">{t.label}</span>
        </button>
        {#if t.description}<small id="desc-{t.name}">{t.description}</small>{/if}
      </li>
    {:else}
      <li>No block types are allowed here.</li>
    {/each}
  </ul>
  <form method="dialog"><button type="submit">Cancel</button></form>
</dialog>
