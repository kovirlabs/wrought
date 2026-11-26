// Pure helpers for block trees: ids, new blocks, moves, summaries.

import type { Block, BlockType, FieldDef } from "./api";

/** A new block id: random, in the shape the server accepts. */
export function newId(): string {
  const bytes = new Uint8Array(6);
  crypto.getRandomValues(bytes);
  return "k" + Array.from(bytes, (b) => b.toString(36).padStart(2, "0")).join("").slice(0, 9);
}

/** Give every block an id, unique across all the item's block fields. */
export function withIds(fields: Record<string, Block[]>): Record<string, Block[]> {
  const seen = new Set<string>();
  const out: Record<string, Block[]> = {};
  for (const [name, list] of Object.entries(fields)) {
    out[name] = list.map((b) => {
      let id = typeof b.id === "string" && /^[A-Za-z0-9_-]{1,40}$/.test(b.id) ? b.id : "";
      if (!id || seen.has(id)) id = newId();
      seen.add(id);
      return { ...b, id };
    });
  }
  return out;
}

/** A new block of `t`, with each field's default. */
export function newBlock(t: BlockType): Block {
  const b: Block = { type: t.name, id: newId() };
  for (const f of t.fields) {
    if (f.default !== undefined && f.default !== null) b[f.name] = f.default;
    else if (f.type === "boolean") b[f.name] = false;
  }
  return b;
}

export function move<T>(list: T[], from: number, to: number): T[] {
  const out = list.slice();
  const [x] = out.splice(from, 1);
  out.splice(Math.max(0, Math.min(to, out.length)), 0, x);
  return out;
}

export function fieldLabel(f: FieldDef): string {
  if (f.label) return f.label;
  const s = f.name.replace(/_/g, " ");
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** One line describing a block for the list: its first text. */
export function summary(b: Block, t: BlockType | undefined): string {
  for (const f of t?.fields ?? []) {
    const v = b[f.name];
    if ((f.type === "text" || f.type === "markdown") && typeof v === "string" && v.trim()) {
      const plain = v.replace(/[*_`#>\[\]()]/g, "").replace(/\s+/g, " ").trim();
      return plain.length > 60 ? plain.slice(0, 57) + "…" : plain;
    }
  }
  return "";
}

/** Rows of fields for the panel: the editor hints' layout, then the rest. */
export function layoutRows(t: BlockType): FieldDef[][] {
  const byName = new Map(t.fields.map((f) => [f.name, f]));
  const rows: FieldDef[][] = [];
  const used = new Set<string>();
  for (const row of t.editor?.layout ?? []) {
    const r = row.map((n) => byName.get(n)).filter((f): f is FieldDef => !!f && !used.has(f.name));
    r.forEach((f) => used.add(f.name));
    if (r.length) rows.push(r);
  }
  for (const f of t.fields) if (!used.has(f.name)) rows.push([f]);
  return rows;
}

/** Whether Markdown uses things the rich-text editor cannot keep (raw
 * HTML, tables, images, footnotes, setext headings): those fields are
 * edited as Markdown text instead, so nothing is lost. */
export function needsPlainMarkdown(md: string): boolean {
  return (
    /<\/?[a-zA-Z!]/.test(md) ||
    /^\s*\|.*\|\s*$/m.test(md) ||
    /!\[[^\]]*\]\(/.test(md) ||
    /\[\^[^\]]+\]/.test(md) ||
    /^\s*(=+|-+)\s*$/m.test(md.replace(/^\s*(-\s*){3,}$/gm, "")) ||
    /^#{5,6}\s/m.test(md) ||
    /^\s*- \[[ xX]\]/m.test(md)
  );
}
