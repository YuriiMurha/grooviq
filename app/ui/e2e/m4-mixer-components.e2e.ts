import { test as base, expect, type Locator, type Page } from "@playwright/test";
import type { MixerCommand } from "../src/mixer/mixerModel";

// The mixer components in their synthetic offline fixture (mixer-fixture.html): generated data, a
// stand-in host, no engine and no audio. Each test reads the commands the mixer emitted.

/** Fails any construction of an audio context: the fixture must stay silent. */
function forbidAudio() {
  for (const name of ["AudioContext", "webkitAudioContext", "OfflineAudioContext"] as const) {
    Object.defineProperty(window, name, {
      configurable: true,
      value: function () {
        throw new Error(`${name} in the mixer fixture`);
      },
    });
  }
}

const test = base.extend<{ fixture: (query?: string) => Promise<Page> }>({
  fixture: async ({ page }, use) => {
    const errors: string[] = [];
    page.on("console", (m) => {
      if (/GL Driver Message/.test(m.text())) return;
      if (m.type() === "error" || m.type() === "warning") errors.push(`${m.type()}: ${m.text()}`);
    });
    page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
    await page.addInitScript(forbidAudio);
    await use(async (query = "") => {
      await page.goto(`/mixer-fixture.html${query}`);
      await expect(page.getByRole("region", { name: "Master" })).toBeVisible();
      return page;
    });
    expect(errors, "console errors and warnings").toEqual([]);
  },
});

const log = (page: Page) => page.evaluate(() => window.mixerFixture.log) as Promise<MixerCommand[]>;
const clearLog = (page: Page) => page.evaluate(() => void (window.mixerFixture.log.length = 0));
const inspector = (page: Page) => page.getByRole("complementary", { name: "Selected mixer track" });
const strip = (page: Page, name: string) => page.getByRole("region", { name, exact: true });
const commits = (cmds: MixerCommand[]) => cmds.filter((c) => !(c.type === "param" && c.phase === "live"));
const nodeOf = (c: MixerCommand) => (c.type === "param" ? c.target.nodeId : c.nodeId);

async function center(el: Locator) {
  const box = (await el.boundingBox())!;
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

test("selection, strip keyboard navigation, and sliders that keep their keys", async ({ fixture }) => {
  const page = await fixture();
  await expect(inspector(page).getByRole("heading", { name: "Drum bus" })).toBeVisible();

  const kick = strip(page, "Kick").getByRole("button", { name: "Kick", exact: true });
  await kick.click();
  await expect(inspector(page).getByRole("heading", { name: "Kick" })).toBeVisible();
  expect(await log(page)).toEqual([{ type: "select", nodeId: "n-kick" }]);
  await kick.focus();

  // Arrows on a strip's name move between strips; Enter selects.
  await page.keyboard.press("ArrowRight");
  await expect(strip(page, "Snare").getByRole("button", { name: "Snare", exact: true })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(inspector(page).getByRole("heading", { name: "Snare" })).toBeVisible();

  // A fader keeps its arrows, Home/End and double-click.
  await clearLog(page);
  const fader = page.getByRole("slider", { name: "Snare level" });
  await fader.focus();
  await page.keyboard.press("ArrowUp");
  await expect(fader).toBeFocused();
  await expect(fader).not.toHaveAttribute("aria-valuenow", "-7");
  await page.keyboard.press("Home");
  await expect(fader).toHaveAttribute("aria-valuetext", "−∞ dB");
  await page.keyboard.press("End");
  await expect(fader).toHaveAttribute("aria-valuenow", "6");
  await fader.dblclick();
  await expect(fader).toHaveAttribute("aria-valuetext", "0.0 dB");
  const cmds = await log(page);
  expect(cmds.every((c) => c.type === "param" && c.target.nodeId === "n-snare" && c.target.kind === "level" && c.phase === "commit")).toBe(true);
  expect(cmds.map((c) => c.type === "param" && c.value).slice(1)).toEqual([-60, 6, 0]);

  // The pan knob keeps its own arrows too.
  const pan = page.getByRole("slider", { name: "Snare pan" });
  await pan.focus();
  await page.keyboard.press("ArrowLeft");
  await expect(pan).toBeFocused();
  await expect(pan).toHaveAttribute("aria-valuetext", "L2");
});

test("fader and pan drags preview live and commit once, Shift drags finely", async ({ fixture }) => {
  const page = await fixture();
  const fader = page.getByRole("slider", { name: "Kick level" });
  const at = await center(fader);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x, at.y + 20, { steps: 4 });
  await page.mouse.move(at.x, at.y + 37, { steps: 4 });
  await page.mouse.up();
  let cmds = await log(page);
  expect(cmds.some((c) => c.type === "param" && c.phase === "live")).toBe(true);
  expect(commits(cmds)).toHaveLength(1);
  const coarse = commits(cmds)[0] as Extract<MixerCommand, { type: "param" }>;
  expect(coarse.target).toEqual({ kind: "level", nodeId: "n-kick" });
  // 37 px of the fader's 148 px travel over 66 dB: one to one.
  expect(coarse.value).toBeCloseTo(-4 - (37 / 148) * 66, 0);
  await expect(fader).toHaveAttribute("aria-valuenow", String(Number(coarse.value.toFixed(2))));

  await clearLog(page);
  await page.keyboard.down("Shift");
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x, at.y + 37, { steps: 6 });
  await page.mouse.up();
  await page.keyboard.up("Shift");
  cmds = commits(await log(page));
  const fine = cmds[0] as Extract<MixerCommand, { type: "param" }>;
  expect(coarse.value - fine.value).toBeCloseTo((37 / 148) * 66 * 0.2, 0);

  await clearLog(page);
  const pan = page.getByRole("slider", { name: "Kick pan" });
  const p = await center(pan);
  await page.mouse.move(p.x, p.y);
  await page.mouse.down();
  await page.mouse.move(p.x, p.y - 30, { steps: 4 });
  await page.mouse.up();
  await expect(pan).not.toHaveAttribute("aria-valuetext", "C");
  await pan.dblclick();
  await expect(pan).toHaveAttribute("aria-valuetext", "C");
  cmds = commits(await log(page));
  expect(cmds.map((c) => c.type === "param" && [c.target.kind, c.target.nodeId])).toEqual([
    ["pan", "n-kick"],
    ["pan", "n-kick"],
  ]);
});

test("mute, solo and clip reset address their own strip", async ({ fixture }) => {
  const page = await fixture();
  await page.getByRole("button", { name: "Mute Hats" }).click();
  await expect(page.getByRole("button", { name: "Mute Hats" })).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: "Solo Keys" }).click();
  await expect(page.getByRole("button", { name: "Solo Keys" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("button", { name: "Kick has not clipped" })).toBeDisabled();
  await page.getByRole("button", { name: "Clear Snare clip" }).click();
  await expect(page.getByRole("button", { name: "Snare has not clipped" })).toBeDisabled();
  expect(await log(page)).toEqual([
    { type: "mute", nodeId: "n-hats", on: true },
    { type: "solo", nodeId: "n-keys", on: true },
    { type: "clearClip", nodeId: "n-snare" },
  ]);
});

test("inserts: add, bypass, open, reorder and remove by slot id", async ({ fixture }) => {
  const page = await fixture();
  await strip(page, "Bass").getByRole("button", { name: "Bass", exact: true }).click();
  const insp = inspector(page);
  await insp.getByLabel("Insert chain").selectOption({ label: "Saturation" });
  const chain = insp.getByRole("listitem");
  await expect(chain).toHaveText([/Fattener/, /Compressor/, /Saturation/]);
  await expect(strip(page, "Bass").getByText("+1 more")).toBeVisible();

  const sat = insp.getByRole("listitem", { name: "Saturation" });
  await sat.getByRole("switch", { name: "On" }).click();
  await expect(sat.getByRole("switch", { name: "On" })).toHaveAttribute("aria-checked", "false");
  await sat.getByRole("button", { name: "Move Saturation earlier" }).click();
  await expect(chain).toHaveText([/Fattener/, /Saturation/, /Compressor/]);
  await insp.getByRole("button", { name: "Open Saturation" }).click();
  await expect(page.getByText("Opened Saturation on Bass")).toBeVisible();
  await expect(insp.getByRole("button", { name: "Move Fattener earlier" })).toBeDisabled();
  await expect(insp.getByRole("button", { name: "Move Compressor later" })).toBeDisabled();
  // A detector-capable insert offers the host's sources: a detector input, not an audible send.
  await insp.getByRole("listitem", { name: "Compressor" }).getByLabel("Sidechain input").selectOption({ label: "Kick" });
  await insp.getByRole("button", { name: "Remove Fattener" }).click();
  await expect(chain).toHaveText([/Saturation/, /Compressor/]);

  const cmds = await log(page);
  const added = cmds.find((c) => c.type === "addInsert");
  expect(added).toMatchObject({ nodeId: "n-bass", effectId: "fx-5" });
  const slot = (cmds.find((c) => c.type === "setInsertActive") as { slotId: string }).slotId;
  const compressor = (cmds.find((c) => c.type === "setDetector") as { slotId: string }).slotId;
  expect(cmds.filter((c) => c.type !== "select" && c.type !== "addInsert")).toEqual([
    { type: "setInsertActive", nodeId: "n-bass", slotId: slot, active: false },
    { type: "moveInsert", nodeId: "n-bass", slotId: slot, beforeSlotId: compressor },
    { type: "openInsert", nodeId: "n-bass", slotId: slot },
    { type: "setDetector", nodeId: "n-bass", slotId: compressor, sourceId: "n-kick" },
    { type: "removeInsert", nodeId: "n-bass", slotId: expect.any(String) },
  ]);
  expect(new Set(cmds.map(nodeOf))).toEqual(new Set(["n-bass"]));
});

test("sends: explicit pre/post, new sends post and off, amount by send id", async ({ fixture }) => {
  const page = await fixture();
  await strip(page, "Kick").getByRole("button", { name: "Kick", exact: true }).click();
  const air = inspector(page).getByRole("group", { name: "Send to Air return" });
  await expect(air.getByRole("radio", { name: "Post" })).toHaveAttribute("aria-checked", "true");
  await air.getByRole("radio", { name: "Pre" }).click();
  await expect(air.getByRole("radio", { name: "Pre" })).toHaveAttribute("aria-checked", "true");

  await inspector(page).getByLabel("Add a send").selectOption({ label: "Echo return" });
  const echo = inspector(page).getByRole("group", { name: "Send to Echo return" });
  await expect(echo.getByRole("radio", { name: "Post" })).toHaveAttribute("aria-checked", "true");
  const amount = echo.getByRole("slider", { name: "Send to Echo return amount" });
  await expect(amount).toHaveAttribute("aria-valuetext", "0%");
  await amount.focus();
  await page.keyboard.press("PageUp");
  await expect(amount).toHaveAttribute("aria-valuetext", "10%");
  await expect(inspector(page).getByLabel("Add a send")).toBeDisabled();
  await echo.getByRole("button", { name: "Remove send to Echo return" }).click();
  await expect(echo).toHaveCount(0);

  const cmds = commits(await log(page)).filter((c) => c.type !== "select");
  const echoSend = (cmds.find((c) => c.type === "param") as Extract<MixerCommand, { type: "param" }>).target;
  expect(cmds).toEqual([
    { type: "setSendPre", nodeId: "n-kick", sendId: expect.any(String), pre: true },
    { type: "addSend", nodeId: "n-kick", destinationId: "n-echo", pre: false, amount: 0 },
    { type: "param", target: echoSend, value: expect.closeTo(0.1, 5), phase: "commit" },
    { type: "removeSend", nodeId: "n-kick", sendId: echoSend.kind === "send" ? echoSend.sendId : "" },
  ]);
});

test("the master: pinned, output disabled, no sends", async ({ fixture }) => {
  const page = await fixture();
  await page.getByRole("region", { name: "Master" }).getByRole("button", { name: "Master", exact: true }).click();
  await expect(inspector(page).getByLabel("Main output")).toBeDisabled();
  await expect(inspector(page).getByText("The master has no sends.")).toBeVisible();
  await expect(inspector(page).getByLabel("Add a send")).toHaveCount(0);
  // Scrolling the tracks never moves the master.
  const before = await page.getByRole("region", { name: "Master" }).boundingBox();
  await page.getByRole("group", { name: /Mixer tracks/ }).evaluate((el) => (el.scrollLeft = el.scrollWidth));
  expect(await page.getByRole("region", { name: "Master" }).boundingBox()).toEqual(before);
});

test("a refused change keeps the supplied state, shows the host's error and sends nothing more", async ({ fixture }) => {
  const page = await fixture();
  await strip(page, "Keys").getByRole("button", { name: "Keys", exact: true }).click();
  const output = inspector(page).getByLabel("Main output");
  await expect(output).toHaveValue("n-master");
  await page.evaluate(() => window.mixerFixture.refuseNext("That route would feed back into Keys."));
  await clearLog(page);
  await output.selectOption({ label: "Drum bus" });
  await expect(inspector(page).getByRole("status")).toHaveText("That route would feed back into Keys.");
  await expect(output).toHaveValue("n-master");

  const fader = page.getByRole("slider", { name: "Keys level" });
  await page.evaluate(() => window.mixerFixture.refuseNext("The engine is busy."));
  await fader.focus();
  await page.keyboard.press("End");
  await expect(fader).toHaveAttribute("aria-valuenow", "-8");
  await expect(inspector(page).getByRole("status")).toHaveText("The engine is busy.");
  await page.waitForTimeout(400);
  expect(await log(page)).toEqual([
    { type: "setOutput", nodeId: "n-keys", outputId: "n-drums" },
    { type: "param", target: { kind: "level", nodeId: "n-keys" }, value: 6, phase: "commit" },
  ]);
});

test("a pending track shows the supplied state and takes no input", async ({ fixture }) => {
  const page = await fixture();
  await page.evaluate(() => window.mixerFixture.setPending("n-kick", true));
  await expect(strip(page, "Kick")).toHaveAttribute("aria-busy", "true");
  await expect(page.getByRole("button", { name: "Mute Kick" })).toBeDisabled();
  const fader = page.getByRole("slider", { name: "Kick level" });
  await fader.focus();
  await page.keyboard.press("End");
  const at = await center(fader);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x, at.y - 40, { steps: 4 });
  await page.mouse.up();
  await expect(fader).toHaveAttribute("aria-valuenow", "-4");
  expect(await log(page)).toEqual([]);
});

test("a strip or send removed or replaced mid-gesture: nothing reaches it or its replacement", async ({ fixture }) => {
  const page = await fixture();
  // Removed during a fader drag.
  let fader = page.getByRole("slider", { name: "Kick level" });
  let at = await center(fader);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x, at.y - 20, { steps: 3 });
  await page.evaluate(() => window.mixerFixture.remove("n-kick"));
  await expect(strip(page, "Kick")).toHaveCount(0);
  await page.mouse.move(at.x, at.y - 40, { steps: 3 });
  await page.mouse.up();
  let cmds = await log(page);
  expect(cmds.filter((c) => c.type === "param" && c.phase !== "live")).toEqual([]);
  expect(new Set(cmds.map(nodeOf))).toEqual(new Set(["n-kick"]));

  // Replaced under a new id during a drag: the replacement hears nothing.
  await clearLog(page);
  fader = page.getByRole("slider", { name: "Snare level" });
  at = await center(fader);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x, at.y - 20, { steps: 3 });
  await page.evaluate(() => window.mixerFixture.replace("n-snare", "n-snare-2"));
  await page.mouse.move(at.x, at.y - 40, { steps: 3 });
  await page.mouse.up();
  cmds = await log(page);
  expect(cmds.some((c) => nodeOf(c) === "n-snare-2")).toBe(false);
  expect(cmds.filter((c) => c.type === "param" && c.phase !== "live")).toEqual([]);
  await expect(page.getByRole("slider", { name: "Snare level" })).toHaveAttribute("aria-valuenow", "-7");

  // A send removed during an amount drag, its strip still there.
  await strip(page, "Keys").getByRole("button", { name: "Keys", exact: true }).click();
  await clearLog(page);
  const amount = inspector(page).getByRole("slider", { name: "Send to Echo return amount" });
  at = await center(amount);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x, at.y - 20, { steps: 3 });
  const live = (await log(page)).find((c) => c.type === "param") as Extract<MixerCommand, { type: "param" }>;
  expect(live.target.kind).toBe("send");
  await page.evaluate((id) => window.mixerFixture.removeSend("n-keys", id), live.target.kind === "send" ? live.target.sendId : "");
  await page.mouse.move(at.x, at.y - 40, { steps: 3 });
  await page.mouse.up();
  cmds = await log(page);
  expect(cmds.filter((c) => c.type === "param" && c.phase !== "live")).toEqual([]);
  await expect(inspector(page).getByText("No sends.")).toBeVisible();
});

test("a removed selection shows its own state, and a renamed strip keeps its id", async ({ fixture }) => {
  const page = await fixture();
  await strip(page, "Hats").getByRole("button", { name: "Hats", exact: true }).click();
  await page.evaluate(() => window.mixerFixture.rename("n-hats", "Open hats"));
  await expect(inspector(page).getByRole("heading", { name: "Open hats" })).toBeVisible();
  await page.evaluate(() => window.mixerFixture.remove("n-hats"));
  await expect(inspector(page)).toContainText("The selected track is no longer in the mixer.");
});

test("no tracks: the master alone stays operable", async ({ fixture }) => {
  const page = await fixture("?tracks=0");
  await expect(page.getByText("No tracks yet. The master is always here.")).toBeVisible();
  await page.getByRole("button", { name: "Mute Master" }).click();
  await expect(page.getByRole("button", { name: "Mute Master" })).toHaveAttribute("aria-pressed", "true");
});

test("a large list draws a bounded window and still reaches every strip", async ({ fixture }) => {
  const page = await fixture("?tracks=5000");
  const row = page.locator(".mx-row > .mx-strip");
  expect(await row.count()).toBeLessThan(30);
  await strip(page, "Track 1").getByRole("button", { name: "Track 1", exact: true }).focus();
  await page.keyboard.press("End");
  await expect(page.getByRole("region", { name: "Master" }).getByRole("button", { name: "Master", exact: true })).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  const last = strip(page, "Track 5000").getByRole("button", { name: "Track 5000", exact: true });
  await expect(last).toBeFocused();
  await expect(last).toBeInViewport();
  expect(await row.count()).toBeLessThan(30);
  await page.keyboard.press("Enter");
  await expect(inspector(page).getByRole("heading", { name: "Track 5000" })).toBeVisible();
  await page.getByRole("button", { name: "Mute Track 5000" }).click();
  expect((await log(page)).at(-1)).toEqual({ type: "mute", nodeId: "n-5000", on: true });
});

for (const width of [1000, 728, 390]) {
  test(`layout at ${width} px: no page overflow, master and selection legible; labelled capture`, async ({ fixture }, info) => {
    const page = await fixture("?long=1");
    await page.setViewportSize({ width, height: 1100 });
    await strip(page, "Kick drum layered with a long sub tail, take three (final)").getByRole("button").first().click();
    await expect(inspector(page).getByRole("heading", { level: 2 })).toContainText("take three");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    const master = (await page.getByRole("region", { name: "Master" }).boundingBox())!;
    expect(master.x + master.width).toBeLessThanOrEqual(width);
    const console = (await page.locator(".mx-console").boundingBox())!;
    const insp = (await inspector(page).boundingBox())!;
    if (width <= 960) expect(insp.y).toBeGreaterThanOrEqual(console.y + console.height - 1);
    else expect(insp.x).toBeGreaterThanOrEqual(console.x + console.width - 1);
    // Every insert row fits the inspector.
    for (const box of await inspector(page).getByRole("listitem").evaluateAll((els) => els.map((e) => e.getBoundingClientRect().right))) {
      expect(box).toBeLessThanOrEqual(insp.x + insp.width + 0.5);
    }
    await page.screenshot({ path: info.outputPath(`mixer-${width}.png`), fullPage: true });
  });
}
