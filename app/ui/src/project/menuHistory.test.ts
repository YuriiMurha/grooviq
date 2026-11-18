// @vitest-environment jsdom
import { expect, it } from "vitest";
import { installMenuHistory } from "./menuHistory";

it("uses browser history even when it refuses, then project history after focus changes", () => {
  let notes = [1, 2];
  const future: number[] = [];
  let blocked = false;
  const router = installMenuHistory(document, (redo) => {
    if (redo) notes.push(future.pop()!);
    else future.push(notes.pop()!);
  }, () => blocked);
  document.execCommand = () => false;
  const input = document.createElement("input");
  const button = document.createElement("button");
  document.body.append(input, button);
  input.focus();
  router.dispatch(false);
  expect(notes).toEqual([1, 2]);
  button.focus();
  router.dispatch(false);
  expect(notes).toEqual([1]);
  router.dispatch(true);
  expect(notes).toEqual([1, 2]);
  blocked = true;
  router.dispatch(false);
  expect(notes).toEqual([1, 2]);
  router.dispose();
  input.remove(); button.remove();
  notes = [];
});
