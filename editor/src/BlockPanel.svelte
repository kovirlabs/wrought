<script lang="ts">
  // The selected block's fields, in the editor hints' layout.
  import { layoutRows } from "./blocks";
  import FieldInput from "./FieldInput.svelte";
  import type { Editor } from "./state.svelte";

  let { ed }: { ed: Editor } = $props();

  const where = $derived(ed.selectedId ? ed.find(ed.selectedId) : null);
  const t = $derived(where ? ed.typeOf(where.block) : undefined);
</script>

<section class="panel" aria-labelledby="panel-heading">
  {#if where && t}
    <h2 id="panel-heading" tabindex="-1">{t.label} <small>block {where.index + 1}</small></h2>
    {#if t.description}<p class="hint">{t.description}</p>{/if}
    {#key where.block.id}
      {#each layoutRows(t) as row (row.map((f) => f.name).join(","))}
        <div class="row" class:multi={row.length > 1}>
          {#each row as f (f.name)}
            <FieldInput
              {f}
              id="f-{where.block.id}-{f.name}"
              value={where.block[f.name]}
              readonly={!ed.writable}
              tokens={ed.tokens}
              resolved={ed.resolved}
              onchange={(v) => ed.setField(where.block.id, f.name, v)}
            />
          {/each}
        </div>
      {/each}
    {/key}
  {:else}
    <h2 id="panel-heading">Block</h2>
    <p>Select a block in the list or the preview to edit it.</p>
  {/if}
</section>
