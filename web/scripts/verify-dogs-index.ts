// Regression guard for the community dog index (web/src/data/dogs.ts).
//
// dogs.ts's validate() is the actual gate: it throws at import time if
// web/public/dogs.json is malformed, which is what fails `astro build`.
// This file exercises that validator directly against deliberately broken
// fixtures, so a refusal is proven rather than assumed, and asserts every
// message names the entry it is about -- a validator that fails without
// saying which of however many entries is wrong is barely better than none.
//
// Runs under Node's built-in test runner and native TypeScript type
// stripping (`node --test scripts/verify-dogs-index.ts`), so this project
// adds no test framework for one module.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  validate,
  unwrapIndex,
  dogs,
  CATEGORIES,
  REQUIRED_FIELDS,
  SUPPORTED_INDEX_VERSION,
  SEMVER_PATTERN,
} from "../src/data/dogs.ts";

/** A valid entry, cloned and broken per case. */
function good(): Record<string, unknown> {
  return {
    name: "Rex",
    package: "shep-example",
    adopt_as: "example",
    description: "Does an example thing.",
    repo: "https://github.com/someone/shep-example",
    license: "MIT",
    category: "other",
    source: { kind: "cargo-git", url: "https://github.com/someone/shep-example" },
  };
}

test("the real index passes its own validator", () => {
  assert.ok(dogs.length >= 1, "the index should not be empty");
});

test("a duplicate dog name is refused, naming both entries", () => {
  const a = good();
  const b = { ...good(), package: "shep-other" };
  assert.throws(() => validate([a, b]), (err: Error) => {
    assert.match(err.message, /Rex/);
    assert.match(err.message, /shep-example/);
    assert.match(err.message, /shep-other/);
    return true;
  });
});

test("an unknown category is refused and lists the ones that exist", () => {
  assert.throws(() => validate([{ ...good(), category: "logz" }]), (err: Error) => {
    assert.match(err.message, /logz/);
    for (const category of CATEGORIES) assert.match(err.message, new RegExp(category));
    return true;
  });
});

test("a missing required field names the field and the entry", () => {
  const entry = good();
  delete entry.adopt_as;
  assert.throws(() => validate([entry]), (err: Error) => {
    assert.match(err.message, /adopt_as/);
    assert.match(err.message, /shep-example/);
    return true;
  });
});

test("an empty required field is refused like a missing one", () => {
  assert.throws(() => validate([{ ...good(), description: "   " }]), /description/);
});

test("a repo that is not https is refused", () => {
  assert.throws(() => validate([{ ...good(), repo: "http://github.com/a/b" }]), /https/);
});

test("an unknown source kind is refused and lists the three that exist", () => {
  assert.throws(() => validate([{ ...good(), source: { kind: "brew", formula: "x" } }]), (err: Error) => {
    assert.match(err.message, /brew/);
    assert.match(err.message, /cargo-git/);
    assert.match(err.message, /go-install/);
    assert.match(err.message, /manual/);
    return true;
  });
});

test("a manual source with no instructions is refused", () => {
  assert.throws(() => validate([{ ...good(), source: { kind: "manual", instructions: "" } }]), /instructions/);
});

test("a cargo-git source with no url is refused", () => {
  assert.throws(() => validate([{ ...good(), source: { kind: "cargo-git" } }]), /url/);
});

test("the top level must be an array", () => {
  assert.throws(() => validate({ dogs: [good()] }), /array/);
});

test("no entry carries an em dash or an en dash", () => {
  for (const dog of dogs) {
    assert.doesNotMatch(dog.description, /—|–/, `${dog.package}'s description`);
  }
});

test("the published schema and the validator agree", async () => {
  const schema = JSON.parse(await readFile(new URL("../public/dogs.schema.json", import.meta.url), "utf8"));
  const entrySchema = schema.properties.dogs.items;
  assert.deepEqual(entrySchema.properties.category.enum, [...CATEGORIES]);
  assert.deepEqual(entrySchema.required.sort(), REQUIRED_FIELDS.toSorted());
});

test("the schema and the wrapper version agree", async () => {
  const schema = JSON.parse(await readFile(new URL("../public/dogs.schema.json", import.meta.url), "utf8"));
  assert.equal(schema.properties.version.const, SUPPORTED_INDEX_VERSION);
});

// --- unwrapIndex: the object wrapper around the entries array -----------

test("shep 0.1.0's own bare-array shape is refused, not silently read", () => {
  assert.throws(() => unwrapIndex([good()]), (err: Error) => {
    assert.match(err.message, /object/);
    return true;
  });
});

test("a version this build does not understand is refused", () => {
  assert.throws(
    () => unwrapIndex({ version: 99, dogs: [good()] }),
    (err: Error) => {
      assert.match(err.message, /99/);
      assert.match(err.message, new RegExp(String(SUPPORTED_INDEX_VERSION)));
      return true;
    },
  );
});

test("a missing version is refused the same as an unsupported one", () => {
  assert.throws(() => unwrapIndex({ dogs: [good()] }), /version/);
});

test("a supported version with no dogs array is refused", () => {
  assert.throws(() => unwrapIndex({ version: SUPPORTED_INDEX_VERSION }), /dogs/);
});

test("a wrapped, supported-version document unwraps to its dogs array", () => {
  const entries = [good()];
  assert.deepEqual(unwrapIndex({ version: SUPPORTED_INDEX_VERSION, dogs: entries }), entries);
});

test("a shep that is not a version is refused", () => {
  for (const shep of ["latest", "0.24", "v0.24.0", "", 24, null, ["0.24.0"]]) {
    assert.throws(
      () => unwrapIndex({ version: SUPPORTED_INDEX_VERSION, shep, dogs: [good()] }),
      /shep/,
      JSON.stringify(shep),
    );
  }
});

test("a missing shep is accepted", () => {
  const entries = [good()];
  assert.deepEqual(unwrapIndex({ version: SUPPORTED_INDEX_VERSION, dogs: entries }), entries);
  assert.deepEqual(
    unwrapIndex({ version: SUPPORTED_INDEX_VERSION, shep: "0.24.0", dogs: entries }),
    entries,
  );
});

test("the schema declares the top-level shep", async () => {
  const schema = JSON.parse(await readFile(new URL("../public/dogs.schema.json", import.meta.url), "utf8"));
  assert.equal(schema.properties.shep.type, "string");
  assert.equal(schema.properties.shep.pattern, SEMVER_PATTERN.source);
  assert.ok(!schema.required.includes("shep"));
});

// --- Extra refusal cases, beyond the brief's list. See task-1-report.md for
// what each protects and why it was made a refusal rather than allowed. ---

test("a name that collides with an existing one only by case is refused", () => {
  const a = good();
  const b = { ...good(), package: "shep-other", name: "rex" };
  assert.throws(() => validate([a, b]), (err: Error) => {
    assert.match(err.message, /rex/i);
    assert.match(err.message, /shep-example/);
    assert.match(err.message, /shep-other/);
    return true;
  });
});

test("a duplicate package under a different display name is refused", () => {
  const a = good();
  const b = { ...good(), name: "Rex II" };
  assert.throws(() => validate([a, b]), (err: Error) => {
    assert.match(err.message, /shep-example/);
    return true;
  });
});

test("a repo that is a valid URL but not a repository is refused", () => {
  assert.throws(() => validate([{ ...good(), repo: "https://github.com" }]), /repository/);
  assert.throws(() => validate([{ ...good(), repo: "https://github.com/someone" }]), /repository/);
});

test("an entry that is not an object is refused, naming its index", () => {
  assert.throws(() => validate(["not an object"]), (err: Error) => {
    assert.match(err.message, /index 0/);
    return true;
  });
});

test("a source that is missing entirely is refused", () => {
  const entry = good();
  delete entry.source;
  assert.throws(() => validate([entry]), /source/);
});

// --- The cargo source's three optional fields ---------------------------

const HASH = "0123456789abcdef".repeat(4);

function cargo(source: Record<string, unknown>): Record<string, unknown> {
  return { ...good(), source: { kind: "cargo", ...source } };
}

test("a cargo source with no version still validates", () => {
  assert.equal(validate([cargo({})]).length, 1);
});

test("a cargo source keeps its version and its hashes", () => {
  const [dog] = validate([cargo({ version: "0.2.4", sha256: { "aarch64-apple-darwin": HASH } })]);
  assert.deepEqual(dog.source, {
    kind: "cargo",
    version: "0.2.4",
    sha256: { "aarch64-apple-darwin": HASH },
  });
});

test("hashes without a version are refused", () => {
  assert.throws(() => validate([cargo({ sha256: { "aarch64-apple-darwin": HASH } })]), /version/);
});

test("a hash that is not 64 lowercase hex digits is refused, naming the target", () => {
  for (const bad of ["abc", HASH.toUpperCase(), `${HASH}0`]) {
    assert.throws(
      () => validate([cargo({ version: "0.2.4", sha256: { "aarch64-apple-darwin": bad } })]),
      /aarch64-apple-darwin/,
    );
  }
});

test("a target that is not a triple is refused", () => {
  assert.throws(() => validate([cargo({ version: "0.2.4", sha256: { "Mac OS": HASH } })]), /Mac OS/);
});

test("the schema describes each cargo field", async () => {
  const schema = JSON.parse(await readFile(new URL("../public/dogs.schema.json", import.meta.url), "utf8"));
  const cargoSchema = schema.properties.dogs.items.properties.source.oneOf[0];
  assert.deepEqual(Object.keys(cargoSchema.properties).sort(), ["kind", "permissions", "sha256", "version"]);
  assert.deepEqual(cargoSchema.dependentRequired, { sha256: ["version"] });

  const permission = cargoSchema.properties.permissions.items;
  assert.deepEqual(Object.keys(permission.properties).sort(), ["name", "required"]);
  assert.deepEqual(permission.required, ["name", "required"]);
  assert.equal(permission.additionalProperties, false);
});

// --- The cargo source's permissions list --------------------------------

test("a cargo source keeps the permissions it lists", () => {
  const permissions = [
    { name: "flock.read", required: true },
    { name: "logs.read", required: false },
  ];
  const [dog] = validate([cargo({ version: "0.2.4", permissions })]);
  assert.deepEqual(dog.source, { kind: "cargo", version: "0.2.4", permissions });
});

test("an empty permissions list validates, as a version declaring none", () => {
  const [dog] = validate([cargo({ version: "0.2.4", permissions: [] })]);
  assert.deepEqual(dog.source, { kind: "cargo", version: "0.2.4", permissions: [] });
});

test("a permissions value that is not a list is refused", () => {
  assert.throws(() => validate([cargo({ version: "0.2.4", permissions: "flock.read" })]), /permissions/);
});

test("a permission that is not a name and a required is refused", () => {
  for (const bad of [
    { name: "flock.read" },
    { required: true },
    { name: "flock.read", required: "yes" },
    { name: "", required: true },
    "flock.read",
  ]) {
    assert.throws(() => validate([cargo({ version: "0.2.4", permissions: [bad] })]), /permissions/, JSON.stringify(bad));
  }
});

// --- An entry's needs ----------------------------------------------------

test("a row may name its needs", () => {
  const [dog] = validate([{ ...good(), needs: ["logs", "alerts"] }]);
  assert.deepEqual(dog.needs, ["logs", "alerts"]);
});

test("a needs field that is not a list of names is refused", () => {
  for (const bad of ["logs", { logs: true }, null, ["logs", 7], [["logs"]]]) {
    assert.throws(() => validate([{ ...good(), needs: bad }]), /needs/, JSON.stringify(bad));
  }
});

test("a need that is not a lowercase word is refused", () => {
  for (const bad of ["", "Logs", "9logs", "log rotate", "-logs", "logs_"]) {
    assert.throws(() => validate([{ ...good(), needs: [bad] }]), /needs/, JSON.stringify(bad));
  }
});

test("a need no shep knows yet is accepted", () => {
  const [dog] = validate([{ ...good(), needs: ["metrics-export"] }]);
  assert.deepEqual(dog.needs, ["metrics-export"]);
});

test("the schema describes needs and does not require them", async () => {
  const schema = JSON.parse(await readFile(new URL("../public/dogs.schema.json", import.meta.url), "utf8"));
  const entrySchema = schema.properties.dogs.items;
  assert.equal(entrySchema.properties.needs.type, "array");
  assert.equal(entrySchema.properties.needs.uniqueItems, true);
  assert.equal(entrySchema.properties.needs.items.type, "string");
  assert.equal(entrySchema.properties.needs.items.pattern, "^[a-z][a-z0-9-]*$");
  assert.ok(!entrySchema.required.includes("needs"));
  assert.ok(!REQUIRED_FIELDS.includes("needs"));
});

test("a need named twice is refused", () => {
  assert.throws(() => validate([{ ...good(), needs: ["logs", "logs"] }]), /twice/);
});
