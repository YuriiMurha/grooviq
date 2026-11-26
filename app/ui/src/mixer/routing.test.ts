import { describe, expect, it } from "vitest";
import type { MixerTrack } from "./mixerModel";
import { classifyRoute, finishRoute, moveRoute } from "./routing";

const target = (id: string) => ({ id, name: id });
const source: Pick<MixerTrack, "id" | "name" | "pending" | "outputId" | "sendDestinations" | "sends"> = {
  ...target("Kick"),
  outputId: "Kit",
  sendDestinations: [target("Room"), target("Echo")],
  sends: [{ id: "send", destinationId: "Echo", amount: 0, pre: false }],
};
const start = { x: 10, y: 10 };
const armed = { start, moving: false };
const moving = moveRoute(armed, { x: 13, y: 14 });

describe("mixer send routing", () => {
  it.each([
    ["Room", "ready"], ["Echo", "existing"], ["Kit", "main"],
    ["Kick", "bad"], ["Master", "bad"], ["Music", "bad"], [null, "idle"],
  ] as const)("classifies %s as %s using host data", (id, kind) => {
    expect(classifyRoute(source, id === null ? null : target(id)).kind).toBe(kind);
  });

  it("keeps an existing send focusable when its destination is absent from the host list", () => {
    expect(classifyRoute({ ...source, sendDestinations: [] }, target("Echo"))).toMatchObject({ kind: "existing", sendId: "send" });
  });

  it("crosses the threshold once, even when the pointer comes back to the handle", () => {
    const short = moveRoute(armed, { x: 13, y: 13 });
    expect(short.moving).toBe(false);
    expect(finishRoute(short, "drop", source, target("Room"))).toBeNull();
    expect(moving.moving).toBe(true);
    expect(moveRoute(moving, start).moving).toBe(true);
  });

  it.each(["escape", "blur", "pointercancel", "lostpointercapture", "pending"] as const)(
    "does not resolve a command after %s", (reason) => {
      expect(finishRoute(moving, reason, source, target("Room"))).toBeNull();
    },
  );

  it("resolves an admitted drop to one zero-gain post-fader send", () => {
    expect(finishRoute(moving, "drop", source, target("Room"))).toMatchObject({
      command: { type: "addSend", nodeId: "Kick", destinationId: "Room", pre: false, amount: 0 },
    });
  });

  it("uses host admission and pending state at release", () => {
    expect(finishRoute(moving, "drop", { ...source, pending: true }, target("Room"))).toBeNull();
    expect(finishRoute(moving, "drop", undefined, target("Room"))).toBeNull();
    expect(finishRoute(moving, "drop", { ...source, sendDestinations: [] }, target("Room"))).toMatchObject({ kind: "bad" });
    expect(finishRoute(moving, "drop", source, target("Echo"))).toMatchObject({ kind: "existing", sendId: "send" });
    expect(finishRoute(moving, "drop", source, target("Kit"))).toMatchObject({ kind: "main" });
  });
});
