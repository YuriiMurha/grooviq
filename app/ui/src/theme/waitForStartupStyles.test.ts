import { describe, expect, it } from "vitest";
import { waitForStartupStyles } from "./waitForStartupStyles";

describe("startup styles", () => {
  it("waits through the interactive state until the document load event", async () => {
    const window = new EventTarget();
    let ready = false;
    const startup = waitForStartupStyles({ readyState: "interactive" }, window).then(() => { ready = true; });

    await Promise.resolve();
    expect(ready).toBe(false);

    window.dispatchEvent(new Event("load"));
    await startup;
    expect(ready).toBe(true);
  });

  it("proceeds if the document already finished loading", async () => {
    await waitForStartupStyles({ readyState: "complete" }, new EventTarget());
  });
});
