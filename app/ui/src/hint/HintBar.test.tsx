import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { HINT_IDLE, HintBar } from "./HintBar";

function setup() {
  render(
    <div>
      <button type="button" data-hint="Plays the pattern.">
        Play
      </button>
      <button type="button" data-hint="Stops and rewinds.">
        Stop
      </button>
      <p>No hint here</p>
      <HintBar />
    </div>,
  );
  return {
    bar: screen.getByLabelText("Hint"),
    play: screen.getByRole("button", { name: "Play" }),
    stop: screen.getByRole("button", { name: "Stop" }),
    plain: screen.getByText("No hint here"),
  };
}

describe("HintBar", () => {
  it("shows_the_hint_under_the_pointer", async () => {
    const { bar, play, stop, plain } = setup();
    const user = userEvent.setup();
    expect(bar.textContent).toBe(HINT_IDLE);
    await user.hover(play);
    expect(bar.textContent).toBe("Plays the pattern.");
    await user.hover(stop);
    expect(bar.textContent).toBe("Stops and rewinds.");
    // Over text without a hint, and off the window, the bar goes back to idle.
    await user.hover(plain);
    expect(bar.textContent).toBe(HINT_IDLE);
  });

  it("shows the hint of the closest ancestor that has one", async () => {
    render(
      <div data-hint="The whole group.">
        <span>inner</span>
        <HintBar />
      </div>,
    );
    await userEvent.setup().hover(screen.getByText("inner"));
    expect(screen.getByLabelText("Hint").textContent).toBe("The whole group.");
  });

  it("follows_a_hint_that_changes_while_hovering", async () => {
    const { bar, play } = setup();
    const user = userEvent.setup();
    await user.hover(play);
    expect(bar.textContent).toBe("Plays the pattern.");

    // The element rewrites its hint as the pointer moves over it (a step grid does this).
    play.setAttribute("data-hint", "Hats, step 3.");
    fireEvent.pointerMove(play);
    expect(bar.textContent).toBe("Hats, step 3.");

    // And a change with no pointer movement is picked up as well.
    play.setAttribute("data-hint", "Hats, step 4.");
    await waitFor(() => expect(bar.textContent).toBe("Hats, step 4."));
  });

  it("falls_back_to_the_focused_element", async () => {
    const { bar, play, stop } = setup();
    const user = userEvent.setup();
    act(() => play.focus());
    expect(bar.textContent).toBe("Plays the pattern.");
    // The pointer wins while it is over something with a hint...
    await user.hover(stop);
    expect(bar.textContent).toBe("Stops and rewinds.");
    // ...and the focused element's hint returns when it leaves.
    await user.unhover(stop);
    expect(bar.textContent).toBe("Plays the pattern.");
    // Blur: nothing focused, nothing hovered.
    act(() => play.blur());
    expect(bar.textContent).toBe(HINT_IDLE);
  });
});
