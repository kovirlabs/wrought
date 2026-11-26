<script lang="ts">
  // Someone saved the item after the version this editor started from.
  // Nothing was overwritten; the editor decides what happens next.
  import type { Editor } from "./state.svelte";

  let { ed }: { ed: Editor } = $props();
  let dialog: HTMLDialogElement;
  let busy = $state(false);

  $effect(() => {
    if (ed.conflict && !dialog.open) dialog.showModal();
    if (!ed.conflict && dialog.open) dialog.close();
  });

  async function act(f: () => Promise<unknown>): Promise<void> {
    busy = true;
    try {
      await f();
    } catch (e) {
      ed.problem = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<dialog bind:this={dialog} role="alertdialog" aria-labelledby="conflict-heading" aria-describedby="conflict-text" class="conflict" onclose={() => (ed.conflict = null)}>
  <h2 id="conflict-heading">Your changes were not saved</h2>
  <p id="conflict-text">
    Someone saved this item while you were editing it. You started from version {ed.conflict?.mine};
    it is now version {ed.conflict?.current}. Nothing was overwritten, and your changes are still here.
  </p>
  <p>
    <a href="{ed.editUrl}revisions/" target="_blank" rel="noopener">See what changed in the history</a> (opens in a new tab).
  </p>
  <div class="buttons">
    <button type="button" disabled={busy} onclick={() => dialog.close()}>Keep editing</button>
    <button type="button" disabled={busy} onclick={() => act(() => ed.reloadTheirs())}>Load their version (discard mine)</button>
    <button type="button" disabled={busy} onclick={() => act(() => ed.saveMineOver())}>Save mine over theirs</button>
  </div>
</dialog>
