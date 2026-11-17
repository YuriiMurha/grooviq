import { describe, expect, it } from "vitest";
import { onProjectReplaced, onProjectReplacing, projectGeneration, projectReplaced, projectReplacing } from "./replacement";

describe("project replacement", () => {
  it("listeners_hear_before_and_after_and_the_generation_moves_on_success_only", () => {
    const heard: string[] = [];
    const offA = onProjectReplacing(() => heard.push(`before ${projectGeneration()}`));
    const offB = onProjectReplaced(() => heard.push(`after ${projectGeneration()}`));
    const start = projectGeneration();
    projectReplacing();
    // A failed Open: only the "before".
    expect(projectGeneration()).toBe(start);
    projectReplacing();
    projectReplaced();
    expect(heard).toEqual([`before ${start}`, `before ${start}`, `after ${start + 1}`]);
    offA();
    offB();
    projectReplacing();
    projectReplaced();
    expect(heard.length).toBe(3);
  });
});
