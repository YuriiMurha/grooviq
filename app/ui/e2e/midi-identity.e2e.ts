import { expect, test } from "./fixture";

test("same-name descriptors have independent switches routed by ID", async ({ app }) => {
  await app.evaluate(() => {
    const shell = (window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
    }).__TAURI_INTERNALS__;
    const original = shell.invoke;
    const ports = [
      { id: "opaque-a", name: "Keyboard", enabled: true },
      { id: "opaque-b", name: "Keyboard (2)", enabled: true },
    ];
    shell.invoke = async (cmd, args) => {
      if (cmd === "midi_inputs") return ports.map((p) => ({ ...p }));
      if (cmd === "midi_set_enabled") {
        const port = ports.find((p) => p.id === args?.id);
        if (!port || typeof args?.enabled !== "boolean") throw new Error("Incorrect port identity");
        port.enabled = args.enabled;
        return null;
      }
      return original(cmd, args);
    };
  });
  const open = () => app.getByRole("button", { name: /Silent \(QA mode\)/ }).click();
  await open();
  const first = app.getByRole("switch", { name: "Keyboard", exact: true });
  const second = app.getByRole("switch", { name: "Keyboard (2)", exact: true });
  await first.click();
  await expect(first).not.toBeChecked();
  await expect(second).toBeChecked();
  await app.keyboard.press("Escape");
  await open();
  await expect(first).not.toBeChecked();
  await expect(second).toBeChecked();
  await second.click();
  await first.click();
  await app.keyboard.press("Escape");
  await open();
  await expect(first).toBeChecked();
  await expect(second).not.toBeChecked();
});
