/**
 * Writes the structural schema to Rust, or checks it has not drifted.
 *
 *   npm run schema         # regenerate crates/thought-schema/schema.json
 *   npm run schema:check   # fail if the committed file is stale (CI)
 *
 * Only the *structure* comes from TipTap. The `md` mapping is Proof of Thought's own and
 * is preserved from the existing file, because ProseMirror has no opinion on
 * how a node becomes markdown.
 */
import { getSchema } from "@tiptap/core";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { extensions } from "../src/schema.js";

const TARGET = resolve(import.meta.dirname, "../../crates/thought-schema/schema.json");

type AttrOut = Record<string, { default?: unknown }>;

function attrs(spec: Record<string, { default?: unknown }> | undefined): AttrOut | undefined {
  if (!spec || Object.keys(spec).length === 0) return undefined;
  const out: AttrOut = {};
  for (const [name, attr] of Object.entries(spec)) {
    // An absent `default` key means the attribute is required. Present-but-null
    // means optional with a null default — a distinction Rust's loader restores
    // explicitly, because serde collapses JSON null into None.
    out[name] = "default" in attr ? { default: attr.default ?? null } : {};
  }
  return out;
}

function build() {
  const schema = getSchema(extensions);
  const existing = JSON.parse(readFileSync(TARGET, "utf8"));

  const nodes: Record<string, unknown> = {};
  schema.spec.nodes.forEach((name: string, spec: any) => {
    const previous = existing.nodes?.[name] ?? {};
    const attrSpec = attrs(spec.attrs);
    nodes[name] = {
      ...(spec.content ? { content: spec.content } : {}),
      ...(spec.group ? { group: spec.group } : {}),
      ...(spec.code ? { code: true } : {}),
      ...(previous.md ? { md: previous.md } : {}),
      ...(attrSpec ? { attrs: attrSpec } : {}),
    };
  });

  const marks: Record<string, unknown> = {};
  schema.spec.marks.forEach((name: string, spec: any) => {
    const previous = existing.marks?.[name] ?? {};
    const attrSpec = attrs(spec.attrs);
    marks[name] = {
      ...(previous.md ? { md: previous.md } : {}),
      ...(attrSpec ? { attrs: attrSpec } : {}),
    };
  });

  return { _comment: existing._comment, nodes, marks };
}

const generated = JSON.stringify(build(), null, 2) + "\n";

if (process.argv.includes("--check")) {
  const committed = readFileSync(TARGET, "utf8");
  if (committed !== generated) {
    console.error(
      "schema.json is stale: the editor's extensions and the Rust schema disagree.\n" +
        "Run `npm run schema` in app/ and commit the result.",
    );
    process.exit(1);
  }
  console.log("schema.json matches the editor's extensions");
} else {
  writeFileSync(TARGET, generated);
  console.log(`wrote ${TARGET}`);
}
