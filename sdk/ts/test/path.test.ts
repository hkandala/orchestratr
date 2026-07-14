import { test } from "node:test";
import assert from "node:assert/strict";
import {
  Pattern,
  resolveCreate,
  resolveSelector,
  validSegment,
  nameOf,
  scopeOfAgent,
  expandRand,
} from "../src/path.js";
import { InvalidRequest } from "../src/errors.js";

// These mirror the Rust `src/path.rs` unit tests 1:1 — the grammar must match byte-for-byte
// so SDK-resolved paths equal what the server would resolve (spec §8 property).

test("segment grammar", () => {
  assert.ok(validSegment("file_1"));
  assert.ok(validSegment("a"));
  assert.ok(validSegment("a".repeat(64)));
  assert.ok(!validSegment(""));
  assert.ok(!validSegment("a".repeat(65)));
  assert.ok(!validSegment("File"));
  assert.ok(!validSegment("a-b"));
  assert.ok(!validSegment("a/b"));
});

test("name lands in scope", () => {
  assert.equal(resolveCreate("review", { name: "worker" }), "review/worker");
  assert.equal(resolveCreate(undefined, { name: "worker" }), "worker");
});

test("relative path resolves against scope; absolute ignores it", () => {
  assert.equal(resolveCreate("review", { path: "fanout/file_1" }), "review/fanout/file_1");
  assert.equal(resolveCreate("review", { path: "/verify/file_1" }), "verify/file_1");
});

test("name with slash rejected", () => {
  assert.throws(() => resolveCreate(undefined, { name: "a/b" }), InvalidRequest);
});

test("depth limit enforced", () => {
  const deep = Array.from({ length: 9 }, (_, i) => `s${i}`).join("/");
  assert.throws(() => resolveCreate(undefined, { path: "/" + deep }), (e: unknown) => {
    assert.ok(e instanceof InvalidRequest);
    assert.equal((e.details as Record<string, unknown>).reason, "path_too_deep");
    return true;
  });
});

test("reserved level-1 rejected only at level 1", () => {
  assert.throws(() => resolveCreate(undefined, { path: "idle/x" }), InvalidRequest);
  assert.throws(() => resolveCreate(undefined, { path: "unmanaged/x" }), InvalidRequest);
  assert.equal(resolveCreate("review", { name: "idle" }), "review/idle");
});

test("{rand} expands to five chars", () => {
  const e = expandRand("review_{rand}/file_1");
  assert.ok(e.startsWith("review_"));
  assert.ok(e.endsWith("/file_1"));
  const mid = e.slice("review_".length, e.length - "/file_1".length);
  assert.equal(mid.length, 5);
  assert.ok(/^[a-z0-9]+$/.test(mid));
  assert.notEqual(expandRand("{rand}"), expandRand("{rand}"));
});

test("derived helpers", () => {
  assert.equal(nameOf("review/fanout/file_1"), "file_1");
  assert.equal(nameOf("worker"), "worker");
  assert.equal(scopeOfAgent("review/fanout/file_1"), "review/fanout");
  assert.equal(scopeOfAgent("worker"), undefined);
});

test("star matches one level; doublestar any depth but not self", () => {
  const star = Pattern.compile("review/*");
  assert.ok(star.matches("review/worker"));
  assert.ok(!star.matches("review/fanout/worker"));
  assert.ok(!star.matches("review"));
  assert.ok(!star.matches("reviewer/x"));

  const ds = Pattern.compile("review/**");
  assert.ok(ds.matches("review/worker"));
  assert.ok(ds.matches("review/fanout/file_1"));
  assert.ok(!ds.matches("review"));
  assert.ok(!ds.matches("reviewer/x"));
});

test("doublestar between; mid star; underscore literal; bare **", () => {
  const between = Pattern.compile("a/**/b");
  assert.ok(between.matches("a/x/b"));
  assert.ok(between.matches("a/x/y/b"));
  assert.ok(!between.matches("a/b"));

  const mid = Pattern.compile("a/*/b");
  assert.ok(mid.matches("a/x/b"));
  assert.ok(!mid.matches("a/x/y/b"));

  const lit = Pattern.compile("review/file_1");
  assert.ok(lit.matches("review/file_1"));
  assert.ok(!lit.matches("review/fileX1"));
  assert.ok(!lit.hasWildcard());

  const all = Pattern.compile("**");
  assert.ok(all.matches("a"));
  assert.ok(all.matches("a/b/c"));
});

test("selector resolution", () => {
  assert.equal(resolveSelector("review", "*"), "review/*");
  assert.equal(resolveSelector("review", "/verify/**"), "verify/**");
  assert.equal(resolveSelector(undefined, "review/worker"), "review/worker");
});
