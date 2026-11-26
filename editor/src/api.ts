// The admin API the editor talks to. Every request is same-origin with the
// admin session's cookie; writes carry the session's CSRF token.

export interface FieldDef {
  name: string;
  type: string;
  label?: string | null;
  help?: string | null;
  required?: boolean;
  max_length?: number | null;
  min?: number | null;
  max?: number | null;
  options?: string[];
  token?: string;
  default?: unknown;
}

export interface EditorHints {
  icon?: string;
  thumbnail?: string;
  layout?: string[][];
  hidden?: boolean;
}

export interface BlockType {
  name: string;
  label: string;
  description?: string | null;
  component?: string | null;
  editor: EditorHints;
  fields: FieldDef[];
}

export interface BlockField {
  field: string;
  label: string;
  help?: string | null;
  blocks: string[];
  writable: boolean;
}

export type Block = { type: string; id: string; [field: string]: unknown };

export interface Item {
  type: string;
  id: number;
  slug: string;
  status: "draft" | "published";
  version: number;
  title: string;
  type_label: string;
  url: string | null;
  can_publish: boolean;
  fields: Record<string, unknown>;
  block_fields: BlockField[];
  block_types: Record<string, BlockType>;
}

export interface TokenChoice {
  name: string;
  property: string | null;
  value: string | { light: string; dark: string } | null;
}

export interface Tokens {
  choices: { group: string; attribute: string; names: TokenChoice[] }[];
}

export interface SaveResult {
  id: number;
  version: number;
  status: "draft" | "published";
  url: string | null;
  fields: Record<string, Block[]>;
}

export interface PreviewResult {
  frame: string;
  fields: Record<string, Block[]>;
}

/** A refusal from the API, with its JSON error. */
export class ApiError extends Error {
  status: number;
  code: string;
  currentVersion?: number;
  constructor(status: number, code: string, message: string, currentVersion?: number) {
    super(message);
    this.status = status;
    this.code = code;
    this.currentVersion = currentVersion;
  }
}

export class Api {
  readonly base: string;
  private csrf: string;

  constructor(
    readonly type: string,
    readonly id: number,
    csrf: string,
  ) {
    this.base = `/admin/api/items/${encodeURIComponent(type)}/${id}/`;
    this.csrf = csrf;
  }

  private async call<T>(method: string, url: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      headers["X-CSRF-Token"] = this.csrf;
    }
    let res: Response;
    try {
      res = await fetch(url, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        credentials: "same-origin",
        cache: "no-store",
      });
    } catch {
      throw new ApiError(0, "network", "The server could not be reached. Check the connection and try again.");
    }
    let data: unknown = null;
    try {
      data = await res.json();
    } catch {
      /* not JSON */
    }
    if (!res.ok) {
      const e = (data as { error?: { code?: string; message?: string; current_version?: number } } | null)
        ?.error;
      throw new ApiError(
        res.status,
        e?.code ?? "error",
        e?.message ?? `The server answered ${res.status}.`,
        e?.current_version,
      );
    }
    return data as T;
  }

  item(): Promise<Item> {
    return this.call("GET", this.base);
  }

  tokens(): Promise<Tokens> {
    return this.call("GET", "/admin/api/tokens");
  }

  preview(tab: string, fields: Record<string, Block[]>): Promise<PreviewResult> {
    return this.call("POST", `${this.base}preview/`, { tab, fields });
  }

  save(version: number, fields: Record<string, Block[]>, status?: "draft" | "published"): Promise<SaveResult> {
    return this.call("PUT", `${this.base}blocks/`, { version, fields, status });
  }
}
