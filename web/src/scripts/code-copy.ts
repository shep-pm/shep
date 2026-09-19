const REVERT_DELAY_MS = 1800;

type CopyState = "copy" | "copied" | "failed";

export function bindCodeCopyButtons(
  root: ParentNode,
  clipboard: Pick<Clipboard, "writeText"> | null | undefined = navigator.clipboard,
): void {
  root.querySelectorAll<HTMLButtonElement>("[data-code-copy]").forEach((button) => {
    const label = button.querySelector<HTMLSpanElement>("[data-copy-label]");
    const copyIcon = button.querySelector<SVGElement>("[data-copy-icon]");
    const copiedIcon = button.querySelector<SVGElement>("[data-copied-icon]");
    const errorIcon = button.querySelector<SVGElement>("[data-copy-error-icon]");
    const pre = button.closest(".code-block")?.querySelector<HTMLPreElement>(".code-block-pre");
    let revertTimer: ReturnType<typeof setTimeout> | undefined;

    const setState = (state: CopyState): void => {
      if (label) label.textContent = state;
      if (copyIcon) copyIcon.style.display = state === "copy" ? "" : "none";
      if (copiedIcon) copiedIcon.style.display = state === "copied" ? "" : "none";
      if (errorIcon) errorIcon.style.display = state === "failed" ? "" : "none";
      button.dataset.copyFailed = String(state === "failed");
      button.setAttribute(
        "aria-label",
        state === "copied" ? "Copied" : state === "failed" ? "Copy failed" : "Copy command",
      );
    };

    const showFailure = (): void => {
      clearTimeout(revertTimer);
      setState("failed");
    };

    button.addEventListener("click", async () => {
      const text = pre?.textContent ?? "";
      if (!clipboard) {
        showFailure();
        return;
      }

      try {
        await clipboard.writeText(text);
      } catch {
        showFailure();
        return;
      }

      setState("copied");
      clearTimeout(revertTimer);
      revertTimer = setTimeout(() => setState("copy"), REVERT_DELAY_MS);
    });
  });
}
