<script lang="ts">
  // One field of a block, by its type. Design options (`token` fields)
  // are radio buttons over the site's design tokens; nothing takes a free
  // CSS value.
  import type { FieldDef, Tokens } from "./api";
  import { fieldLabel, needsPlainMarkdown } from "./blocks";
  import RichText from "./RichText.svelte";

  let {
    f,
    id,
    value,
    readonly,
    tokens,
    resolved,
    onchange,
  }: {
    f: FieldDef;
    id: string;
    value: unknown;
    readonly: boolean;
    tokens: Tokens | null;
    resolved: Record<string, string>;
    onchange: (v: unknown) => void;
  } = $props();

  const label = $derived(fieldLabel(f));
  const help = $derived(f.help ? `${id}-help` : undefined);
  const str = $derived(value === undefined || value === null ? "" : String(value));
  // Markdown the rich-text editor cannot keep is edited as text; the
  // editor may also switch to Markdown text at any time.
  let plain = $state(false);
  $effect.pre(() => {
    if (f.type === "markdown" && needsPlainMarkdown(str)) plain = true;
  });

  const choices = $derived.by(() => {
    if (f.type !== "token") return [];
    const g = tokens?.choices.find((c) => c.group === f.token);
    const names = g?.names ?? [];
    return names.filter((n) => !f.options?.length || f.options.includes(n.name));
  });

  function swatch(property: string | null, fallback: unknown): string {
    if (!property) return "";
    const r = resolved[property];
    if (r) return r;
    if (typeof fallback === "string") return fallback;
    if (fallback && typeof fallback === "object" && "light" in fallback) return String((fallback as { light: string }).light);
    return "";
  }

  function pretty(n: string): string {
    return n.replace(/_/g, " ");
  }
</script>

{#if f.type === "boolean"}
  <p class="field">
    <input {id} type="checkbox" checked={value === true} disabled={readonly} aria-describedby={help} onchange={(e) => onchange(e.currentTarget.checked)} />
    <label for={id}>{label}</label>
    {#if f.help}<small id={help}>{f.help}</small>{/if}
  </p>
{:else if f.type === "token"}
  <fieldset class="field tokens" aria-describedby={help}>
    <legend>{label}{#if f.required}<small> (required)</small>{/if}</legend>
    <div class="token-list">
      {#if !f.required}
        <label class="token">
          <input type="radio" name={id} value="" checked={!str} disabled={readonly} onchange={() => onchange(null)} />
          <span class="swatch none" aria-hidden="true"></span>
          <span>None</span>
        </label>
      {/if}
      {#each choices as c (c.name)}
        <label class="token">
          <input type="radio" name={id} value={c.name} checked={str === c.name} disabled={readonly} onchange={() => onchange(c.name)} />
          {#if f.token === "font"}
            <span class="swatch font" aria-hidden="true" style:font-family={swatch(c.property, c.value) || undefined}>Aa</span>
          {:else if f.token === "spacing"}
            <span class="swatch space" aria-hidden="true"><span style:inline-size={swatch(c.property, null) || undefined}></span></span>
          {:else}
            <span class="swatch" aria-hidden="true" style:background-color={swatch(c.property, c.value) || undefined}></span>
          {/if}
          <span>{pretty(c.name)}</span>
        </label>
      {/each}
    </div>
    <small id={help}>{f.help ?? "From the site's design tokens."}</small>
  </fieldset>
{:else}
  <div class="field">
    {#if f.type === "markdown" && !plain}
      <span class="label" id="{id}-label">{label}{#if f.required && !readonly}<small> (required)</small>{/if}</span>
    {:else}
      <label for={id}>{label}{#if f.required && !readonly}<small> (required)</small>{/if}</label>
    {/if}
    {#if f.type === "markdown" && !plain}
      <RichText {id} {label} value={str} describedby={help} {readonly} onchange={(md) => onchange(md)} />
      {#if !readonly}<button type="button" class="link-button" onclick={() => (plain = true)}>Edit as Markdown text</button>{/if}
    {:else if f.type === "markdown" || (f.type === "text" && (f.max_length ?? 301) > 300)}
      <textarea {id} rows={f.type === "markdown" ? 8 : 3} value={str} {readonly} required={f.required} maxlength={f.max_length ?? undefined} aria-describedby={help} oninput={(e) => onchange(e.currentTarget.value)}></textarea>
      {#if f.type === "markdown" && !needsPlainMarkdown(str) && !readonly}
        <button type="button" class="link-button" onclick={() => (plain = false)}>Edit as rich text</button>
      {/if}
    {:else if f.type === "select"}
      <select {id} value={str} disabled={readonly} required={f.required} aria-describedby={help} onchange={(e) => onchange(e.currentTarget.value)}>
        <option value="">{f.required ? "Choose…" : "(none)"}</option>
        {#each f.options ?? [] as o (o)}<option value={o}>{o}</option>{/each}
      </select>
    {:else if f.type === "integer" || f.type === "media"}
      <input {id} type="number" value={str} {readonly} required={f.required} min={f.type === "media" ? 1 : (f.min ?? undefined)} max={f.max ?? undefined} aria-describedby={help}
        oninput={(e) => onchange(e.currentTarget.value === "" ? null : Number(e.currentTarget.value))} />
    {:else if f.type === "date"}
      <input {id} type="date" value={str} {readonly} required={f.required} aria-describedby={help} oninput={(e) => onchange(e.currentTarget.value)} />
    {:else}
      <input {id} type="text" inputmode={f.type === "url" ? "url" : undefined} value={str} {readonly} required={f.required} maxlength={f.max_length ?? undefined} aria-describedby={help} oninput={(e) => onchange(e.currentTarget.value)} />
    {/if}
    {#if f.help}<small id={help}>{f.help}</small>{/if}
    {#if f.type === "markdown"}<small>Markdown is cleaned when the page is published.</small>{/if}
    {#if f.type === "media"}<small>The number of a file in the <a href="/admin/media/" target="_blank" rel="noopener">media library</a> (opens in a new tab).</small>{/if}
    {#if f.type === "url"}<small>https://, mailto: or a path on this site, such as /about/.</small>{/if}
  </div>
{/if}
