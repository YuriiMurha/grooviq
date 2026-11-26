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
  await expect(inspector(page).getByRole("heading", { name: "Snare" })).toBeVisible();

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
  await expect(fader).not.toHaveAttribute("aria-valuenow", "-8");
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
  await page.getByRole("button", { name: "Mute Kit bus" }).click();
  await expect(page.getByRole("button", { name: "Mute Kit bus" })).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: "Solo Keys" }).click();
  await expect(page.getByRole("button", { name: "Solo Keys" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("button", { name: "Kick has not clipped" })).toBeDisabled();
  await page.getByRole("button", { name: "Clear Snare clip" }).click();
  await expect(page.getByRole("button", { name: "Snare has not clipped" })).toBeDisabled();
  expect(await log(page)).toEqual([
    { type: "mute", nodeId: "n-drums", on: true },
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

test("visible sends use host options and send their own pre/post, amount and remove commands", async ({ fixture }) => {
  const page = await fixture();
  const snare = strip(page, "Snare");
  const room = snare.getByRole("group", { name: "Send to Room" });
  const tap = room.getByRole("button", { name: "Pre-fader send to Room" });
  await expect(tap).toHaveAttribute("aria-pressed", "false");
  await tap.click();
  await expect(tap).toHaveAttribute("aria-pressed", "true");

  const add = snare.getByRole("combobox", { name: "Add send from Snare" });
  await expect(add.getByRole("option", { name: "Room", exact: true })).toHaveCount(0);
  await expect(add.getByRole("option", { name: "Echo", exact: true })).toHaveCount(1);
  await add.selectOption({ label: "Echo" });
  const echo = snare.getByRole("group", { name: "Send to Echo" });
  await expect(echo.getByRole("button", { name: "Pre-fader send to Echo" })).toHaveAttribute("aria-pressed", "false");
  const amount = echo.getByRole("slider", { name: "Send to Echo amount" });
  await expect(amount).toHaveAttribute("aria-valuetext", "0%");
  await amount.press("PageUp");
  await expect(amount).toHaveAttribute("aria-valuetext", "10%");
  await expect(add).toBeDisabled();
  await echo.getByRole("button", { name: "Remove send to Echo" }).press("Enter");
  await expect(echo).toHaveCount(0);
  await room.getByRole("button", { name: "Remove send to Room" }).click();
  await expect(room).toHaveCount(0);

  const cmds = await log(page);
  const echoAmount = cmds.find((c) => c.type === "param");
  expect(echoAmount?.target).toEqual({ kind: "send", nodeId: "n-snare", sendId: expect.any(String) });
  expect(cmds).toEqual([
    { type: "setSendPre", nodeId: "n-snare", sendId: expect.any(String), pre: true },
    { type: "addSend", nodeId: "n-snare", destinationId: "n-echo", pre: false, amount: 0 },
    { type: "param", target: echoAmount?.target, value: expect.closeTo(0.1, 5), phase: "commit" },
    { type: "removeSend", nodeId: "n-snare", sendId: echoAmount?.target.kind === "send" ? echoAmount.target.sendId : "" },
    { type: "removeSend", nodeId: "n-snare", sendId: expect.any(String) },
  ]);
});

test("the strip output picker changes the single main output and the rack reflects it", async ({ fixture }) => {
  const page = await fixture();
  const snare = strip(page, "Snare");
  const output = snare.getByRole("combobox", { name: "Main output for Snare" });
  await expect(output).toHaveValue("n-drums");
  await output.selectOption({ label: "Master" });
  await expect(inspector(page).getByLabel("Main output")).toHaveValue("n-master");
  expect(await log(page)).toEqual([{ type: "setOutput", nodeId: "n-snare", outputId: "n-master" }]);
});

test("unsupported controls explain why and emit no command", async ({ fixture }) => {
  const page = await fixture("?unsupported=1");
  const snare = strip(page, "Snare");
  const controls = [
    [snare.getByRole("button", { name: "Mute Snare" }), /Mute.*unavailable/],
    [snare.getByRole("button", { name: "Solo Snare" }), /Solo.*unavailable/],
    [inspector(page).getByRole("switch", { name: "On" }).first(), /Bypass.*unavailable/],
    [inspector(page).getByRole("combobox", { name: "Sidechain input" }), /Detector.*unavailable/],
  ] as const;
  for (const [control] of controls) {
    await control.evaluate((el) => {
      (el as HTMLButtonElement).click();
      el.dispatchEvent(new Event("change", { bubbles: true }));
    });
  }
  expect(await log(page)).toEqual([]);
  for (const [control, reason] of controls) {
    await expect(control).toBeDisabled();
    await expect(control).toHaveAccessibleDescription(reason);
  }
});

for (const finish of ["release", "Escape", "blur"] as const) {
  test(`send drag streams live values and handles ${finish} without a late commit`, async ({ fixture }) => {
    const page = await fixture();
    const amount = strip(page, "Snare").getByRole("slider", { name: "Send to Room amount" });
    const at = await center(amount);
    await page.mouse.move(at.x, at.y);
    await page.mouse.down();
    await page.mouse.move(at.x, at.y - 22, { steps: 3 });
    const live = (await log(page)).filter((command) => command.type === "param" && command.phase === "live");
    expect(live.length).toBeGreaterThan(0);
    await expect(amount).not.toHaveAttribute("aria-valuetext", "16%");
    if (finish === "Escape") await page.keyboard.press("Escape");
    if (finish === "blur") await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    await page.mouse.up();
    expect(commits(await log(page))).toEqual([{
      type: "param",
      phase: finish === "release" ? "commit" : "cancel",
      target: { kind: "send", nodeId: "n-snare", sendId: expect.any(String) },
      value: finish === "release" ? expect.any(Number) : 0.16,
    }]);
    if (finish !== "release") await expect(amount).toHaveAttribute("aria-valuetext", "16%");
  });
}

test("meters change through MeterSource without rendering the mixer on each frame", async ({ fixture }) => {
  const page = await fixture("?measure=1");
  const stats = () => page.evaluate(() => window.mixerFixture.stats());
  const meter = strip(page, "Snare").getByRole("img", { name: "Snare meter" });
  await expect.poll(async () => (await stats()).meterReads).toBeGreaterThan(20);
  const before = await stats();
  const bars = await meter.locator("span").evaluateAll((els) => els.map((el) => (el as HTMLElement).style.transform));
  await expect.poll(async () => (await stats()).meterReads).toBeGreaterThan(before.meterReads + 50);
  await expect.poll(() => meter.locator("span").evaluateAll((els) => els.map((el) => (el as HTMLElement).style.transform))).not.toEqual(bars);
  expect((await stats()).mixerRenders).toBe(before.mixerRenders);
  await strip(page, "Kick").getByRole("button", { name: "Kick", exact: true }).click();
  await expect.poll(async () => (await stats()).mixerRenders).toBeGreaterThan(before.mixerRenders);
});

test("the master: pinned, output disabled, no sends", async ({ fixture }) => {
  const page = await fixture("?tracks=35");
  await page.getByRole("region", { name: "Master" }).getByRole("button", { name: "Master", exact: true }).click();
  await expect(inspector(page).getByLabel("Main output")).toBeDisabled();
  await expect(strip(page, "Master").getByRole("combobox")).toHaveCount(0);
  // Scrolling the tracks never moves the master.
  const before = await page.getByRole("region", { name: "Master" }).boundingBox();
  await page.getByRole("group", { name: /Mixer tracks/ }).evaluate((el) => (el.scrollLeft = el.scrollWidth));
  expect(await page.getByRole("region", { name: "Master" }).boundingBox()).toEqual(before);
});

test("a refused change keeps the supplied state, shows the host's error and sends nothing more", async ({ fixture }) => {
  const page = await fixture();
  await strip(page, "Keys").getByRole("button", { name: "Keys", exact: true }).click();
  const output = inspector(page).getByLabel("Main output");
  await expect(output).toHaveValue("n-music");
  await page.evaluate(() => window.mixerFixture.refuseNext("That route would feed back into Keys."));
  await clearLog(page);
  await output.selectOption({ label: "Kit bus" });
  await expect(inspector(page).getByRole("status")).toHaveText("That route would feed back into Keys.");
  await expect(output).toHaveValue("n-music");

  const fader = page.getByRole("slider", { name: "Keys level" });
  await page.evaluate(() => window.mixerFixture.refuseNext("The engine is busy."));
  await fader.focus();
  await page.keyboard.press("End");
  await expect(fader).toHaveAttribute("aria-valuenow", "-9");
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
  await expect(strip(page, "Kick").getByRole("combobox", { name: "Main output for Kick" })).toBeDisabled();
  await expect(strip(page, "Kick").getByRole("combobox", { name: "Add send from Kick" })).toBeDisabled();
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
  await expect(page.getByRole("slider", { name: "Snare level" })).toHaveAttribute("aria-valuenow", "-8");

  // A send removed during an amount drag, its strip still there.
  await clearLog(page);
  const amount = strip(page, "Vocal take").getByRole("slider", { name: "Send to Echo amount" });
  at = await center(amount);
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  await page.mouse.move(at.x, at.y - 20, { steps: 3 });
  const live = (await log(page)).find((c) => c.type === "param") as Extract<MixerCommand, { type: "param" }>;
  expect(live.target.kind).toBe("send");
  await page.evaluate((id) => window.mixerFixture.removeSend("n-vocal", id), live.target.kind === "send" ? live.target.sendId : "");
  await page.mouse.move(at.x, at.y - 40, { steps: 3 });
  await page.mouse.up();
  cmds = await log(page);
  expect(cmds.filter((c) => c.type === "param" && c.phase !== "live")).toEqual([]);
  await expect(strip(page, "Vocal take").getByRole("group", { name: "Send to Echo" })).toHaveCount(0);
});

test("a removed selection shows its own state, and a renamed strip keeps its id", async ({ fixture }) => {
  const page = await fixture();
  await strip(page, "Kit bus").getByRole("button", { name: "Kit bus", exact: true }).click();
  await page.evaluate(() => window.mixerFixture.rename("n-drums", "Percussion bus"));
  await expect(inspector(page).getByRole("heading", { name: "Percussion bus" })).toBeVisible();
  await page.evaluate(() => window.mixerFixture.remove("n-drums"));
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

for (const settleDuringDrag of [false, true]) {
  test(`an earlier refused host commit and a newer cancelled drag (settle during drag: ${settleDuringDrag})`, async ({ fixture }) => {
    const page = await fixture();
    await strip(page, "Kick").getByRole("button", { name: "Kick", exact: true }).click();
    await clearLog(page);
    await page.evaluate(() => window.mixerFixture.deferNextCommit());
    const fader = page.getByRole("slider", { name: "Kick level" });
    await fader.focus();
    await page.keyboard.press("ArrowDown");
    const issued = (await log(page)).find((c): c is Extract<MixerCommand, { type: "param" }> => c.type === "param" && c.phase === "commit")!;
    const preview = String(Number(issued.value.toFixed(2)));
    await expect(fader).toHaveAttribute("aria-valuenow", preview);
    const at = await center(fader);
    await page.mouse.move(at.x, at.y);
    await page.mouse.down();
    await page.mouse.move(at.x, at.y - 20, { steps: 3 });
    if (settleDuringDrag) {
      await page.evaluate(() => window.mixerFixture.settleCommit(false));
      await expect(inspector(page).getByRole("status")).toBeVisible();
    }
    await page.keyboard.press("Escape");
    await page.mouse.up();
    await expect(fader).toHaveAttribute("aria-valuenow", settleDuringDrag ? "-4" : preview);
    const cancelled = (await log(page)).filter((c): c is Extract<MixerCommand, { type: "param" }> => c.type === "param" && c.phase === "cancel");
    expect(cancelled).toHaveLength(1);
    expect(cancelled[0].value).toBe(settleDuringDrag ? -4 : issued.value);
    if (!settleDuringDrag) await page.evaluate(() => window.mixerFixture.settleCommit(false));
    await expect(fader).toHaveAttribute("aria-valuenow", "-4");
    expect((await log(page)).filter(c => c.type === "param" && c.phase === "commit")).toHaveLength(1);
  });
}

test("a host pending flag suppresses an outstanding commit preview", async ({ fixture }) => {
  const page = await fixture();
  await strip(page, "Kick").getByRole("button", { name: "Kick", exact: true }).click();
  await clearLog(page);
  await page.evaluate(() => window.mixerFixture.deferNextCommit());
  const fader = page.getByRole("slider", { name: "Kick level" });
  await fader.focus();
  await page.keyboard.press("ArrowDown");
  await expect(fader).not.toHaveAttribute("aria-valuenow", "-4");
  await page.evaluate(() => window.mixerFixture.setPending("n-kick", true));
  await expect(fader).toHaveAttribute("aria-disabled", "true");
  await expect(fader).toHaveAttribute("aria-valuenow", "-4");
  await page.evaluate(() => window.mixerFixture.settleCommit(false));
  await page.evaluate(() => window.mixerFixture.setPending("n-kick", false));
  await expect(fader).toHaveAttribute("aria-valuenow", "-4");
  expect((await log(page)).filter(c => c.type === "param")).toHaveLength(1);
});

for (const cause of ["Escape", "blur"] as const) {
  test(`${cause} cancels drag and wheel without a late commit, then new input works`, async ({ fixture }) => {
    const page = await fixture();
    const cancel = async () => {
      if (cause === "Escape") await page.keyboard.press("Escape");
      else await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    };
    const fader = page.getByRole("slider", { name: "Kick level" });
    const at = await center(fader);
    await page.mouse.move(at.x, at.y);
    await page.mouse.down();
    await page.mouse.move(at.x, at.y - 20, { steps: 3 });
    expect((await log(page)).some(c => c.type === "param" && c.phase === "live")).toBe(true);
    await cancel();
    await page.mouse.move(at.x, at.y - 40, { steps: 3 });
    await page.mouse.up();
    let params = (await log(page)).filter((c): c is Extract<MixerCommand, { type: "param" }> => c.type === "param");
    expect(params.filter(c => c.phase === "cancel")).toEqual([{ type: "param", target: { kind: "level", nodeId: "n-kick" }, value: -4, phase: "cancel" }]);
    expect(params.filter(c => c.phase === "commit")).toEqual([]);
    await expect(fader).toHaveAttribute("aria-valuenow", "-4");

    await clearLog(page);
    const pan = page.getByRole("slider", { name: "Kick pan" });
    const point = await center(pan);
    await page.mouse.move(point.x, point.y);
    const now = new Date();
    await page.clock.install({ time: now });
    await page.clock.pauseAt(new Date(now.getTime() + 10_000));
    await page.mouse.wheel(0, -80);
    await expect.poll(async () => (await log(page)).filter(c => c.type === "param" && c.phase === "live").length).toBeGreaterThan(0);
    await cancel();
    // Advance past the wheel timer after cancelling, independently of browser scheduling.
    await page.clock.runFor(400);
    params = (await log(page)).filter((c): c is Extract<MixerCommand, { type: "param" }> => c.type === "param");
    expect(params.filter(c => c.phase === "cancel")).toEqual([{ type: "param", target: { kind: "pan", nodeId: "n-kick" }, value: 0, phase: "cancel" }]);
    expect(params.filter(c => c.phase === "commit")).toEqual([]);
    await expect(pan).toHaveAttribute("aria-valuenow", "0");

    await clearLog(page);
    await pan.focus();
    await page.keyboard.press("ArrowRight");
    expect(commits(await log(page))).toHaveLength(1);
    await expect(pan).toHaveAttribute("aria-valuetext", "R2");
  });
}

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
    if (width <= 850) expect(insp.y).toBeGreaterThanOrEqual(console.y + console.height - 1);
    else expect(insp.x).toBeGreaterThanOrEqual(console.x + console.width - 1);
    // Every insert row fits the inspector.
    for (const box of await inspector(page).getByRole("listitem").evaluateAll((els) => els.map((e) => e.getBoundingClientRect().right))) {
      expect(box).toBeLessThanOrEqual(insp.x + insp.width + 0.5);
    }
    await page.screenshot({ path: info.outputPath(`mixer-${width}.png`), fullPage: true });
  });
}
