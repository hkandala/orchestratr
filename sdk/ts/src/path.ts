// The identity-path grammar, ported 1:1 from the Rust `src/path.rs`. The SDK resolves
// scopes and patterns client-side (into absolute paths) and sends the absolute result to the
// server, so the composed effective paths match what the CLI would produce — `orcr.scope`
// nesting composes the same effective paths as the CLI path (property-tested).

import { InvalidRequest } from "./errors.js";

export const MAX_SEGMENTS = 8;
export const MAX_PATH_LEN = 256;
export const MAX_SEGMENT_LEN = 64;
export const RESERVED_LEVEL1 = ["idle", "unmanaged"];

const SEGMENT_RE = /^[a-z0-9_]+$/;

/** True if a segment is a legal identity segment (`[a-z0-9_]{1,64}`). */
export function validSegment(s: string): boolean {
  return s.length >= 1 && s.length <= MAX_SEGMENT_LEN && SEGMENT_RE.test(s);
}

function randomToken(): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789";
  let out = "";
  for (let i = 0; i < 5; i++) {
    out += alphabet[Math.floor(Math.random() * alphabet.length)];
  }
  return out;
}

/** Replace every `{rand}` placeholder with 5 random `[a-z0-9]` chars (creation only). */
export function expandRand(input: string): string {
  return input.replace(/\{rand\}/g, () => randomToken());
}

function joinScope(scope: string | undefined, rel: string): string {
  return scope && scope.length > 0 ? `${scope}/${rel}` : rel;
}

/** The agent's name = the last path segment. */
export function nameOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}

/** An agent's scope = its path minus its name; undefined for a single-segment path. */
export function scopeOfAgent(path: string): string | undefined {
  const i = path.lastIndexOf("/");
  return i < 0 ? undefined : path.slice(0, i);
}

/** A loop's name is the first segment of a run path. */
export function loopNameFrom(path: string): string {
  const i = path.indexOf("/");
  return i < 0 ? path : path.slice(0, i);
}

/** Validate an absolute path's grammar, depth, and length. */
export function validatePath(path: string): void {
  if (path.length === 0) throw new InvalidRequest("path is empty", { reason: "empty_path" });
  if (path.length > MAX_PATH_LEN) {
    throw new InvalidRequest(`path \`${path}\` exceeds ${MAX_PATH_LEN} chars`, {
      reason: "path_too_long",
      path,
      len: path.length,
    });
  }
  const segments = path.split("/");
  if (segments.length > MAX_SEGMENTS) {
    throw new InvalidRequest(
      `path \`${path}\` has ${segments.length} segments, exceeding the limit of ${MAX_SEGMENTS}`,
      { reason: "path_too_deep", path, segments: segments.length },
    );
  }
  for (const seg of segments) {
    if (!validSegment(seg)) {
      throw new InvalidRequest(
        `path \`${path}\` has an invalid segment \`${seg}\` (segments are [a-z0-9_], 1-${MAX_SEGMENT_LEN} chars)`,
        { reason: "invalid_segment" },
      );
    }
  }
}

function checkReservedLevel1(path: string): void {
  const first = path.split("/")[0];
  if (RESERVED_LEVEL1.includes(first)) {
    throw new InvalidRequest(`\`${first}\` is a reserved level-1 name owned by orcr`, {
      reason: "reserved_name",
      name: first,
    });
  }
}

/** One of `--name` (single segment) or `--path` (relative, leading `/` = absolute). */
export type NameOrPath = { name: string } | { path: string };

/**
 * Resolve a creation target into an absolute effective path (scope resolution + `{rand}`),
 * validating grammar + depth + reserved level-1 — mirrors Rust `resolve_create`.
 */
export function resolveCreate(scope: string | undefined, input: NameOrPath): string {
  let effective: string;
  if ("name" in input) {
    const name = expandRand(input.name);
    if (name.includes("/")) {
      throw new InvalidRequest(
        `name \`${name}\` must be a single segment (no \`/\`); use a path instead`,
        { reason: "invalid_name" },
      );
    }
    effective = joinScope(scope, name);
  } else {
    const path = expandRand(input.path);
    effective = path.startsWith("/") ? path.slice(1) : joinScope(scope, path);
  }
  validatePath(effective);
  checkReservedLevel1(effective);
  return effective;
}

/** True if `s` contains a wildcard segment (`*` or `**`). */
export function isPattern(s: string): boolean {
  return s.split("/").some((seg) => seg === "*" || seg === "**");
}

/**
 * Resolve a selector (path or pattern) against the caller's scope into an absolute form
 * (leading `/` = absolute), validating every literal segment — mirrors Rust `resolve_selector`.
 */
export function resolveSelector(scope: string | undefined, raw: string): string {
  const effective = raw.startsWith("/") ? raw.slice(1) : joinScope(scope, raw);
  if (effective.length === 0) {
    throw new InvalidRequest("selector is empty", { reason: "empty_selector" });
  }
  for (const seg of effective.split("/")) {
    if (seg === "*" || seg === "**") continue;
    if (!validSegment(seg)) {
      throw new InvalidRequest(`selector \`${effective}\` has an invalid segment \`${seg}\``, {
        reason: "invalid_segment",
      });
    }
  }
  return effective;
}

type Seg = { kind: "lit"; value: string } | { kind: "star" } | { kind: "dstar" };

/** A compiled glob pattern: whole-segment `*`/`**`, matched anchored — mirrors Rust. */
export class Pattern {
  private constructor(private readonly segs: Seg[]) {}

  static compile(pattern: string): Pattern {
    if (pattern.length === 0) {
      throw new InvalidRequest("empty pattern", { reason: "empty_pattern" });
    }
    const segs: Seg[] = pattern.split("/").map((seg) => {
      if (seg === "*") return { kind: "star" } as Seg;
      if (seg === "**") return { kind: "dstar" } as Seg;
      if (validSegment(seg)) return { kind: "lit", value: seg } as Seg;
      throw new InvalidRequest(
        `pattern segment \`${seg}\` is invalid (whole-segment \`*\`/\`**\` only)`,
        { reason: "invalid_pattern" },
      );
    });
    return new Pattern(segs);
  }

  hasWildcard(): boolean {
    return this.segs.some((s) => s.kind === "star" || s.kind === "dstar");
  }

  matches(path: string): boolean {
    const parts = path.split("/");
    return this.matchFrom(0, parts, 0);
  }

  private matchFrom(pi: number, path: string[], si: number): boolean {
    if (pi === this.segs.length) return si === path.length;
    const seg = this.segs[pi];
    if (seg.kind === "dstar") {
      const remaining = Math.max(0, path.length - si);
      for (let t = 1; t <= remaining; t++) {
        if (this.matchFrom(pi + 1, path, si + t)) return true;
      }
      return false;
    }
    if (seg.kind === "star") {
      return si < path.length && this.matchFrom(pi + 1, path, si + 1);
    }
    return si < path.length && path[si] === seg.value && this.matchFrom(pi + 1, path, si + 1);
  }
}
