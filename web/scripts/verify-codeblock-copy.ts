import assert from "node:assert/strict";
import test from "node:test";

import { bindCodeCopyButtons } from "../src/scripts/code-copy.ts";

function fixture() {
  const label = { textContent: "copy" };
  const copyIcon = { style: { display: "" } };
  const copiedIcon = { style: { display: "none" } };
  const errorIcon = { style: { display: "none" } };
  const pre = { textContent: "echo hello" };
  let clickHandler: (() => Promise<void>) | undefined;
  const attributes = new Map<string, string>();
  const button = {
    dataset: {} as Record<string, string>,
    addEventListener(_event: string, handler: () => Promise<void>) {
      clickHandler = handler;
    },
    closest: () => ({ querySelector: () => pre }),
    querySelector(selector: string) {
      return new Map([
        ["[data-copy-label]", label],
        ["[data-copy-icon]", copyIcon],
        ["[data-copied-icon]", copiedIcon],
        ["[data-copy-error-icon]", errorIcon],
      ]).get(selector);
    },
    setAttribute(name: string, value: string) {
      attributes.set(name, value);
    },
  } as unknown as HTMLButtonElement;
  const root = {
    querySelectorAll: () => [button],
  } as unknown as ParentNode;

  return {
    attributes,
    button,
    click: async () => {
      assert.ok(clickHandler, "copy click handler should be attached");
      await clickHandler();
    },
    copyIcon,
    copiedIcon,
    errorIcon,
    label,
    pre,
    root,
  };
}

test("missing clipboard reports failure instead of silently doing nothing", async () => {
  const ui = fixture();
  bindCodeCopyButtons(ui.root, null);

  await ui.click();

  assert.equal(ui.label.textContent, "failed");
  assert.equal(ui.attributes.get("aria-label"), "Copy failed");
  assert.equal(ui.button.dataset.copyFailed, "true");
  assert.equal(ui.copyIcon.style.display, "none");
  assert.equal(ui.errorIcon.style.display, "");
});

test("a rejected clipboard write reports failure", async () => {
  const ui = fixture();
  bindCodeCopyButtons(ui.root, {
    writeText: async () => {
      throw new Error("permission denied");
    },
  });

  await ui.click();

  assert.equal(ui.label.textContent, "failed");
  assert.equal(ui.attributes.get("aria-label"), "Copy failed");
  assert.equal(ui.button.dataset.copyFailed, "true");
});

test("successful copy keeps the existing copied confirmation", async () => {
  const ui = fixture();
  const copied: string[] = [];
  bindCodeCopyButtons(ui.root, {
    writeText: async (text) => {
      copied.push(text);
    },
  });

  await ui.click();

  assert.deepEqual(copied, ["echo hello"]);
  assert.equal(ui.label.textContent, "copied");
  assert.equal(ui.attributes.get("aria-label"), "Copied");
  assert.equal(ui.button.dataset.copyFailed, "false");
  assert.equal(ui.copiedIcon.style.display, "");
});
