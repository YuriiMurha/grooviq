import { describe, expect, it } from "vitest";
import {
  isProjectReplacing,
  isProjectReplacementPending,
  onProjectReplaced,
  onProjectReplacing,
  projectGeneration,
  projectReplaceFailed,
  projectReplaced,
  projectReplacing,
} from "./replacement";
import { beginProjectTransition } from "./pending";

describe("project replacement", () => {
  it("Save fences new admission without retiring work it is waiting to settle", () => {
    const finish = beginProjectTransition();
    try {
      expect(isProjectReplacing()).toBe(true);
      expect(isProjectReplacementPending()).toBe(false);
      projectReplacing();
      expect(isProjectReplacementPending()).toBe(true);
      projectReplaceFailed();
      expect(isProjectReplacementPending()).toBe(false);
      expect(isProjectReplacing()).toBe(true);
    } finally { projectReplaceFailed(); finish(); }
    expect(isProjectReplacing()).toBe(false);
  });
  it("listeners_hear_before_and_after_and_the_generation_moves_on_success_only", () => {
    const heard: string[] = [];
    const offA = onProjectReplacing(() => heard.push(`before ${projectGeneration()} ${isProjectReplacing()}`));
    const offB = onProjectReplaced(() => heard.push(`after ${projectGeneration()} ${isProjectReplacing()}`));
    const start = projectGeneration();
    projectReplacing();
    // A failed Open: only the "before", and the old project goes on.
    expect(isProjectReplacing()).toBe(true);
    projectReplaceFailed();
    expect(isProjectReplacing()).toBe(false);
    expect(projectGeneration()).toBe(start);
    projectReplacing();
    projectReplaced();
    expect(isProjectReplacing()).toBe(false);
    // Listeners run before the replacing state starts, so they can still reach the old project.
    expect(heard).toEqual([`before ${start} false`, `before ${start} false`, `after ${start + 1} false`]);
    offA();
    offB();
    projectReplacing();
    projectReplaced();
    expect(heard.length).toBe(3);
  });
});
