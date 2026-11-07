import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { convert, assemble, split, verifyFragment, search } from "../node.js";

import { existsSync } from "node:fs";

// The repo fixture, or any deck given through the environment.
const deck = process.env.SLIDEPDF_TEST_PPTX ?? new URL("../../tests/fixtures/deck.pptx", import.meta.url).pathname;

test("converts, fragments, reassembles and splits", { skip: !existsSync(deck) && "set SLIDEPDF_TEST_PPTX" }, () => {
  const out = convert(readFileSync(deck), { preset: "light" });
  assert.ok(out.pdf.length > 0);
  assert.equal(out.fragments.length, out.manifest.count);
  for (const f of out.fragments) assert.ok(verifyFragment(f.pdf, f.hash));
  const merged = assemble(out.fragments.map((f) => f.pdf));
  assert.equal(split(merged).length, out.fragments.length);
  assert.ok(typeof out.report.text === "string");
  assert.ok(Array.isArray(search(out.manifest, "a")));
});
