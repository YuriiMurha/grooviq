import { expect, presetValues, project, test } from "./fixture";

test.use({
  seed: {
    project: project([], {}, {
      channels: [{
        id: 1,
        name: "Bass",
        color: 3,
        sample: null,
        volume: 1,
        pan: 0,
        mute: false,
        solo: false,
        sampler: {},
        kind: "synth",
        synth: presetValues("808 Glide"),
        preset: "808 Glide",
      }],
      nextId: 2,
    }),
  },
});

test("the active synth scope polls at silence without drawing or writing to the DOM", async ({ app, mock }) => {
  await app.getByRole("button", { name: "Edit the Bass synth" }).click();
  const editor = app.getByRole("dialog", { name: "Bass synth" });
  const scope = editor.getByLabel("Synth output waveform");
  const status = editor.locator(".synth-output-status");
  await expect(status).toContainText("0 voices");

  const probe = await scope.evaluateHandle((canvas: HTMLCanvasElement) => {
    const ctx = canvas.getContext("2d");
    const dialog = canvas.closest('[role="dialog"]');
    const text = dialog?.querySelector(".synth-output-status");
    if (!ctx || !dialog || !text) throw new Error("No synth output to observe");
    const stroke = ctx.stroke;
    const requestFrame = window.requestAnimationFrame;
    const drawingCallbacks = new Set<FrameRequestCallback>();
    const scheduled = new Map<FrameRequestCallback, number>();
    let running: FrameRequestCallback | undefined;
    let strokes = 0;
    let mutations = 0;

    window.requestAnimationFrame = (callback) => {
      scheduled.set(callback, (scheduled.get(callback) ?? 0) + 1);
      return requestFrame.call(window, (time) => {
        running = callback;
        try {
          callback(time);
        } finally {
          running = undefined;
        }
      });
    };
    ctx.stroke = (...args: [] | [Path2D]) => {
      strokes++;
      if (running) drawingCallbacks.add(running);
      Reflect.apply(stroke, ctx, args);
    };
    const observer = new MutationObserver((records) => { mutations += records.length; });
    observer.observe(text, { childList: true, characterData: true, subtree: true });
    for (const element of dialog.querySelectorAll("[data-synth-meter], [data-synth-stage]")) {
      observer.observe(element, { attributes: true });
    }
    return {
      sample: () => ({
        strokes,
        scopeFrames: [...drawingCallbacks].reduce((sum, callback) => sum + (scheduled.get(callback) ?? 0), 0),
        mutations,
      }),
      reset: () => {
        scheduled.clear();
        strokes = 0;
        mutations = 0;
      },
      waitFrames: async (count: number) => {
        for (let frame = 0; frame < count; frame++) {
          await new Promise((resolve) => requestFrame.call(window, resolve));
        }
      },
      restore: () => {
        window.requestAnimationFrame = requestFrame;
        ctx.stroke = stroke;
        observer.disconnect();
      },
    };
  });

  try {
    // Identify callbacks by watching which ones stroke this canvas during keyboard audition.
    await app.keyboard.down("z");
    try {
      await expect(status).toContainText("1 voice");
      await probe.evaluate((meter) => meter.waitFrames(8));
      expect((await probe.evaluate((meter) => meter.sample())).scopeFrames).toBeGreaterThan(0);
    } finally {
      await app.keyboard.up("z");
    }
    await expect(status).toContainText("0 voices");
    await probe.evaluate((meter) => meter.waitFrames(20));
    const readsBefore = (await mock.calls("synth_meter")).length;
    const counts = await probe.evaluate(async (meter) => {
      meter.reset();
      await meter.waitFrames(32);
      return meter.sample();
    });
    expect((await mock.calls("synth_meter")).length).toBeGreaterThan(readsBefore + 1);
    expect(counts, "the silent scope schedules no frames and leaves its display alone").toEqual({
      strokes: 0,
      scopeFrames: 0,
      mutations: 0,
    });
  } finally {
    await probe.evaluate((meter) => meter.restore());
    await probe.dispose();
  }
});
