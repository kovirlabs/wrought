<script lang="ts">
  // Rich text for `markdown` fields: Tiptap (ProseMirror) with a toolbar,
  // reading and writing Markdown (the field's stored form). Only what
  // Markdown and the engine's sanitiser keep is offered: headings,
  // emphasis, links, lists, quotes, code. Text the editor could not keep
  // (raw HTML, tables, images) is edited as Markdown instead.
  import { onDestroy, onMount } from "svelte";
  import { Editor } from "@tiptap/core";
  import StarterKit from "@tiptap/starter-kit";
  import { Markdown } from "@tiptap/markdown";

  let {
    id,
    label,
    value,
    describedby,
    readonly = false,
    onchange,
  }: {
    id: string;
    label: string;
    value: string;
    describedby?: string;
    readonly?: boolean;
    onchange: (md: string) => void;
  } = $props();

  let host: HTMLDivElement;
  let editor: Editor | null = $state(null);
  // Bumped on every transaction so toolbar states refresh.
  let tick = $state(0);
  let linkOpen = $state(false);
  let linkUrl = $state("");
  let linkInput: HTMLInputElement | undefined = $state();
  let lastEmitted = "";

  onMount(() => {
    editor = new Editor({
      element: host,
      editable: !readonly,
      extensions: [
        StarterKit.configure({
          heading: { levels: [2, 3, 4] },
          codeBlock: false,
          horizontalRule: false,
          strike: false,
          underline: false,
          link: { openOnClick: false, autolink: false, protocols: ["mailto"], HTMLAttributes: { rel: null, target: null } },
        }),
        Markdown,
      ],
      content: value ?? "",
      contentType: "markdown",
      editorProps: {
        attributes: {
          id,
          role: "textbox",
          "aria-multiline": "true",
          "aria-label": label,
          ...(describedby ? { "aria-describedby": describedby } : {}),
          ...(readonly ? { "aria-readonly": "true" } : {}),
          class: "rich-text-area",
        },
      },
      onTransaction: () => tick++,
      onUpdate: ({ editor }) => {
        const md = editor.getMarkdown();
        lastEmitted = md;
        onchange(md);
      },
    });
    lastEmitted = editor.getMarkdown();
  });

  // Another value from outside (undo, a reload): show it.
  $effect(() => {
    const v = value ?? "";
    if (editor && v !== lastEmitted) {
      lastEmitted = v;
      editor.commands.setContent(v, { contentType: "markdown", emitUpdate: false });
    }
  });

  onDestroy(() => editor?.destroy());

  function is(name: string, attrs?: Record<string, unknown>): boolean {
    void tick;
    return !!editor?.isActive(name, attrs);
  }

  type Cmd = () => void;
  const run = (f: Cmd) => () => {
    f();
  };

  function openLink(): void {
    linkUrl = (editor?.getAttributes("link").href as string | undefined) ?? "";
    linkOpen = true;
    queueMicrotask(() => linkInput?.focus());
  }

  function applyLink(e: SubmitEvent): void {
    e.preventDefault();
    const url = linkUrl.trim();
    const chain = editor!.chain().focus().extendMarkRange("link");
    if (!url) chain.unsetLink().run();
    else if (/^(https?:\/\/|mailto:|\/(?!\/)|#)/i.test(url)) chain.setLink({ href: url }).run();
    else return;
    linkOpen = false;
  }
</script>

<div class="rich-text">
  {#if !readonly}
    <div class="rt-toolbar" role="toolbar" aria-label="Formatting: {label}" aria-controls={id}>
      <button type="button" aria-pressed={is("bold")} onclick={run(() => editor?.chain().focus().toggleBold().run())}><b>B</b><span class="visually-hidden"> Bold</span></button>
      <button type="button" aria-pressed={is("italic")} onclick={run(() => editor?.chain().focus().toggleItalic().run())}><i>I</i><span class="visually-hidden"> Italic</span></button>
      <button type="button" aria-pressed={is("code")} onclick={run(() => editor?.chain().focus().toggleCode().run())}><code>&lt;&gt;</code><span class="visually-hidden"> Code</span></button>
      <button type="button" aria-pressed={is("heading", { level: 2 })} onclick={run(() => editor?.chain().focus().toggleHeading({ level: 2 }).run())}>H2<span class="visually-hidden"> Heading</span></button>
      <button type="button" aria-pressed={is("heading", { level: 3 })} onclick={run(() => editor?.chain().focus().toggleHeading({ level: 3 }).run())}>H3<span class="visually-hidden"> Subheading</span></button>
      <button type="button" aria-pressed={is("bulletList")} onclick={run(() => editor?.chain().focus().toggleBulletList().run())}>• List<span class="visually-hidden"> (bulleted)</span></button>
      <button type="button" aria-pressed={is("orderedList")} onclick={run(() => editor?.chain().focus().toggleOrderedList().run())}>1. List<span class="visually-hidden"> (numbered)</span></button>
      <button type="button" aria-pressed={is("blockquote")} onclick={run(() => editor?.chain().focus().toggleBlockquote().run())}>“ Quote</button>
      <button type="button" aria-pressed={is("link")} aria-expanded={linkOpen} onclick={openLink}>Link</button>
    </div>
    {#if linkOpen}
      <form class="rt-link" onsubmit={applyLink}>
        <label for="{id}-link">Link address</label>
        <input id="{id}-link" bind:this={linkInput} bind:value={linkUrl} type="text" inputmode="url" placeholder="https://… or /page/" aria-describedby="{id}-link-help" />
        <small id="{id}-link-help">https://, mailto: or a path on this site. Empty removes the link.</small>
        <button type="submit">Apply</button>
        <button type="button" onclick={() => { linkOpen = false; editor?.commands.focus(); }}>Cancel</button>
      </form>
    {/if}
  {/if}
  <div bind:this={host}></div>
</div>
