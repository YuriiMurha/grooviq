import { expect, test } from "vitest";
import { focusInto, returnFocus, type Focusable, type FocusRoot } from "./focus";

function spy() {
  const calls: string[] = [];
  const el = (name: string): Focusable => ({ focus: () => void calls.push(name) });
  return { calls, el };
}

test("focus goes to the first focusable control", () => {
  const { calls, el } = spy();
  const root: FocusRoot = { ...el("root"), querySelector: () => el("first") };
  focusInto(root);
  expect(calls).toEqual(["first"]);
});

test("focus falls back to the dialog itself when it holds no control", () => {
  const { calls, el } = spy();
  focusInto({ ...el("root"), querySelector: () => null });
  expect(calls).toEqual(["root"]);
});

test("closing returns focus to the opener", () => {
  const { calls, el } = spy();
  returnFocus(el("chip"), false);
  expect(calls).toEqual(["chip"]);
});

test("closing does not steal focus the user moved elsewhere, or fail without an opener", () => {
  const { calls, el } = spy();
  returnFocus(el("chip"), true);
  returnFocus(null, false);
  expect(calls).toEqual([]);
});
