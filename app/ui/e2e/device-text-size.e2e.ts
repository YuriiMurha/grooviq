import type { Locator } from "@playwright/test";
import { expect, presetValues, project, soundsFor, test } from "./fixture";

const beat = project(["Percussion", "Missing", "Bass"]);
beat.channels[1].embedded = false;
Object.assign(beat.channels[2], {
  kind: "synth", sample: null, preset: "808 Glide", synth: presetValues("808 Glide"),
});
test.use({ seed: { project: beat, files: soundsFor(["Percussion"]) } });

async function undersizedText(dialog: Locator): Promise<string[]> {
  return dialog.evaluate((root) => {
    const issues: string[] = [];
    for (const element of root.querySelectorAll("*")) {
      const style = getComputedStyle(element);
      if (style.visibility !== "visible" || !element.getClientRects().length) continue;
      if (element.closest("details:not([open])") && !element.closest("summary")) continue;
      const size = Number.parseFloat(style.fontSize);
      if (size >= 11) continue;
      // Read each element's own text, so a caption inherits its own computed size even when
      // its container or neighbouring value already meets the floor.
      const text = (element instanceof SVGTextElement ? element.textContent : [...element.childNodes]
        .filter((node) => node.nodeType === Node.TEXT_NODE).map((node) => node.textContent).join(" "))
        ?.replace(/\s+/g, " ").trim();
      if (text) issues.push(`${element.localName}.${[...element.classList].join(".")} "${text}": ${size}px`);
    }
    return issues;
  });
}

for (const viewport of [{ width: 1280, height: 800 }, { width: 900, height: 560 }]) {
  test.describe(`${viewport.width}×${viewport.height}`, () => {
    test.use({ viewport });

    test("visible sampler and synth text is at least 11 design px", async ({ app }) => {
      const findings: string[] = [];
      for (const name of ["Percussion sampler", "Missing sampler", "Bass synth"]) {
        await app.getByRole("button", { name: `Edit the ${name}`, exact: true }).click();
        const dialog = app.getByRole("dialog", { name, exact: true });
        if (name === "Percussion sampler") {
          await expect(dialog.getByRole("img", { name: "Percussion waveform" })).toBeVisible();
          await dialog.getByText("File details", { exact: true }).click();
          await expect(dialog.locator("dd").first()).toBeVisible();
        } else if (name === "Missing sampler") {
          await expect(dialog.getByRole("button", { name: "Relink Missing" })).toBeVisible();
        } else {
          await expect(dialog.locator(".synth-output-status")).toContainText("0 voices");
        }
        await expect(dialog.locator("svg text").first()).toBeVisible();
        findings.push(...(await undersizedText(dialog)).map((issue) => `${name} | ${issue}`));
        await dialog.getByRole("button", { name: `Close the ${name}` }).click();
      }
      expect(findings, "Editor text below 11 CSS px").toEqual([]);
    });
  });
}
