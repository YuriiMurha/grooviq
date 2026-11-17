import type { FullConfig, FullResult, Reporter, Suite, TestCase, TestResult } from "@playwright/test/reporter";

/** A skipped replay is not parity evidence, even when Playwright's run status is passed. */
export default class ReplayReporter implements Reporter {
  private expected = 0;
  private passed = 0;

  onBegin(_config: FullConfig, suite: Suite) {
    this.expected = suite.allTests().length;
  }

  onTestEnd(_test: TestCase, result: TestResult) {
    if (result.status === "passed") this.passed++;
  }

  async onEnd(result: FullResult): Promise<{ status: "failed" } | void> {
    if (result.status === "passed" && (this.expected === 0 || this.passed !== this.expected)) {
      console.error(`Replay did not execute every selected case: ${this.passed}/${this.expected} passed.`);
      return { status: "failed" };
    }
  }
}
