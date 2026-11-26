// The editor's state: the item's block trees as edited, which block is
// selected, the saved version, and messages for the live regions.

import { Api, ApiError, type Block, type BlockType, type Item, type Tokens } from "./api";
import { move, newBlock, newId, withIds } from "./blocks";

export type Conflict = { mine: number; current: number };

export class Editor {
  api: Api;
  item = $state<Item | null>(null);
  tokens = $state<Tokens | null>(null);
  fields = $state<Record<string, Block[]>>({});
  activeField = $state("");
  selectedId = $state<string | null>(null);
  version = $state(0);
  status = $state<"draft" | "published">("draft");
  url = $state<string | null>(null);
  dirty = $state(false);
  saving = $state(false);
  /** Polite announcements (moves, saves). */
  announcement = $state("");
  /** Problems the editor must see (validation, refusals). */
  problem = $state("");
  conflict = $state<Conflict | null>(null);
  loadError = $state("");
  /** Bumped on every change, so the preview knows to re-render. */
  revision = $state(0);
  /** Resolved token values, read from the preview frame. */
  resolved = $state<Record<string, string>>({});
  private undoStack: { fields: Record<string, Block[]>; selected: string | null; what: string }[] = [];
  canUndo = $state(false);
  undoLabel = $state("");

  constructor(
    readonly type: string,
    readonly id: number,
    csrf: string,
    readonly editUrl: string,
    readonly frameCss: string,
  ) {
    this.api = new Api(type, id, csrf);
  }

  async load(): Promise<void> {
    try {
      const [item, tokens] = await Promise.all([this.api.item(), this.api.tokens().catch(() => null)]);
      this.apply(item);
      this.tokens = tokens;
    } catch (e) {
      this.loadError = e instanceof Error ? e.message : String(e);
    }
  }

  private apply(item: Item): void {
    const fields: Record<string, Block[]> = {};
    for (const bf of item.block_fields) {
      const v = item.fields[bf.field];
      fields[bf.field] = Array.isArray(v) ? (v as Block[]) : [];
    }
    this.item = item;
    this.fields = withIds(fields);
    this.version = item.version;
    this.status = item.status;
    this.url = item.url;
    this.dirty = false;
    this.undoStack = [];
    this.canUndo = false;
    if (!this.activeField || !(this.activeField in fields)) {
      this.activeField = item.block_fields.find((f) => f.writable)?.field ?? item.block_fields[0]?.field ?? "";
    }
    if (this.selectedId && !this.find(this.selectedId)) this.selectedId = null;
    this.revision++;
  }

  get writable(): boolean {
    return !!this.item?.block_fields.find((f) => f.field === this.activeField)?.writable;
  }

  get blocks(): Block[] {
    return this.fields[this.activeField] ?? [];
  }

  get allowedTypes(): BlockType[] {
    const bf = this.item?.block_fields.find((f) => f.field === this.activeField);
    return (bf?.blocks ?? [])
      .map((n) => this.item?.block_types[n])
      .filter((t): t is BlockType => !!t && !t.editor?.hidden);
  }

  typeOf(b: Block): BlockType | undefined {
    return this.item?.block_types[b.type];
  }

  find(id: string): { field: string; index: number; block: Block } | null {
    for (const [field, list] of Object.entries(this.fields)) {
      const index = list.findIndex((b) => b.id === id);
      if (index >= 0) return { field, index, block: list[index] };
    }
    return null;
  }

  label(b: Block): string {
    const where = this.find(b.id);
    const t = this.typeOf(b);
    return `${t?.label ?? b.type} ${where ? where.index + 1 : ""}`.trim();
  }

  private remember(what: string): void {
    this.undoStack.push({ fields: $state.snapshot(this.fields), selected: this.selectedId, what });
    if (this.undoStack.length > 50) this.undoStack.shift();
    this.canUndo = true;
    this.undoLabel = what;
  }

  undo(): void {
    const last = this.undoStack.pop();
    if (!last) return;
    this.fields = last.fields;
    this.selectedId = last.selected;
    this.canUndo = this.undoStack.length > 0;
    this.undoLabel = this.undoStack.at(-1)?.what ?? "";
    this.changed();
    this.announce(`Undone: ${last.what}.`);
  }

  private changed(): void {
    this.dirty = true;
    this.revision++;
  }

  announce(msg: string): void {
    // Re-announce identical messages too.
    this.announcement = "";
    queueMicrotask(() => (this.announcement = msg));
  }

  select(id: string | null): void {
    this.selectedId = id;
    const where = id ? this.find(id) : null;
    if (where) this.activeField = where.field;
  }

  add(t: BlockType): Block {
    this.remember(`add ${t.label}`);
    const b = newBlock(t);
    const list = this.blocks.slice();
    const sel = this.selectedId ? list.findIndex((x) => x.id === this.selectedId) : -1;
    list.splice(sel >= 0 ? sel + 1 : list.length, 0, b);
    this.fields[this.activeField] = list;
    this.selectedId = b.id;
    this.changed();
    this.announce(`${t.label} added as block ${list.indexOf(b) + 1} of ${list.length}.`);
    return b;
  }

  duplicate(id: string): void {
    const w = this.find(id);
    if (!w) return;
    this.remember(`duplicate ${this.label(w.block)}`);
    const copy = { ...$state.snapshot(w.block), id: newId() } as Block;
    const list = this.fields[w.field].slice();
    list.splice(w.index + 1, 0, copy);
    this.fields[w.field] = list;
    this.selectedId = copy.id;
    this.changed();
    this.announce(`Copied. The copy is block ${w.index + 2} of ${list.length}.`);
  }

  remove(id: string): void {
    const w = this.find(id);
    if (!w) return;
    const name = this.label(w.block);
    this.remember(`remove ${name}`);
    const list = this.fields[w.field].filter((b) => b.id !== id);
    this.fields[w.field] = list;
    if (this.selectedId === id) this.selectedId = list[Math.min(w.index, list.length - 1)]?.id ?? null;
    this.changed();
    this.announce(`${name} removed. Undo brings it back.`);
  }

  /** Move block `id` to position `to` (0-based) in its field. */
  moveTo(id: string, to: number): void {
    const w = this.find(id);
    if (!w) return;
    const list = this.fields[w.field];
    const target = Math.max(0, Math.min(to, list.length - 1));
    if (target === w.index) return;
    this.remember(`move ${this.label(w.block)}`);
    this.fields[w.field] = move(list, w.index, target);
    this.changed();
    this.announce(`${this.typeOf(w.block)?.label ?? "Block"} moved to position ${target + 1} of ${list.length}.`);
  }

  setField(id: string, name: string, value: unknown): void {
    const w = this.find(id);
    if (!w) return;
    const list = this.fields[w.field].slice();
    const b = { ...list[w.index] };
    if (value === "" || value === null || value === undefined) delete b[name];
    else b[name] = value;
    list[w.index] = b as Block;
    this.fields[w.field] = list;
    this.changed();
  }

  snapshot(): Record<string, Block[]> {
    const out: Record<string, Block[]> = {};
    for (const bf of this.item?.block_fields ?? []) {
      if (bf.writable) out[bf.field] = $state.snapshot(this.fields[bf.field] ?? []) as Block[];
    }
    return out;
  }

  /** Save the block fields; `publish` also publishes. A stale version opens
   * the conflict dialog: nothing is overwritten unless the editor says so. */
  async save(publish = false, overVersion?: number): Promise<boolean> {
    if (!this.item || this.saving) return false;
    this.saving = true;
    this.problem = "";
    try {
      const r = await this.api.save(overVersion ?? this.version, this.snapshot(), publish ? "published" : undefined);
      this.version = r.version;
      this.status = r.status;
      this.url = r.url;
      this.dirty = false;
      this.conflict = null;
      this.announce(
        publish
          ? `Published as version ${r.version}. The page on the site is rewritten in a moment.`
          : `Saved as version ${r.version}.${r.status === "published" ? " The page on the site is rewritten in a moment." : ""}`,
      );
      return true;
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        this.conflict = { mine: this.version, current: e.currentVersion ?? this.version + 1 };
      } else {
        this.problem = `Not saved: ${e instanceof Error ? e.message : String(e)}`;
      }
      return false;
    } finally {
      this.saving = false;
    }
  }

  /** Conflict: drop my changes and load the saved item. */
  async reloadTheirs(): Promise<void> {
    const item = await this.api.item();
    this.apply(item);
    this.conflict = null;
    this.announce(`Loaded version ${item.version}. Your changes were discarded.`);
  }

  /** Conflict: after review, save mine over the current version. */
  async saveMineOver(): Promise<void> {
    const current = this.conflict?.current;
    if (current === undefined) return;
    await this.save(false, current);
  }
}
