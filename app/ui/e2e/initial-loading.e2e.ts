import { expect, test, workspaceReady } from "./fixture";

test.use({
  seed: { holdProjectView: true },
  viewport: { width: 900, height: 560 },
  reducedMotion: "reduce",
});

const loading = "Loading project…";

test("announces a slow initial project load until the rack mounts", async ({ startupPage: page }) => {
  const time = new Date("2026-01-01T00:00:00Z");
  await page.clock.install({ time });
  await page.clock.pauseAt(new Date("2026-01-01T00:01:00Z"));
  await page.goto("/");

  const stage = page.getByRole("main");
  const status = stage.getByRole("status");
  await expect(status).toBeEmpty();
  await page.clock.resume();
  await expect(status.filter({ hasText: loading })).toBeVisible();
  await expect(status).toHaveCSS("font-size", "14px");
  await expect(page.getByRole("region", { name: "Channel rack" })).toHaveCount(0);

  await page.evaluate("window.__grooviqMock.releaseCommand('project_view')");
  await workspaceReady(page);
  await expect(stage.getByText(loading, { exact: true })).toHaveCount(0);
});

test("clears loading and shows the existing toast when the initial project load fails", async ({ startupPage: page }) => {
  const error = "The engine could not read the initial project.";
  await page.goto("/");

  const stage = page.getByRole("main");
  await expect(stage.getByRole("status").filter({ hasText: loading })).toBeVisible();
  await page.evaluate((message) => {
    const mock = (window as unknown as { __grooviqMock: {
      refuseNext: (command: string, error: string) => void;
      releaseCommand: (command: string) => void;
    } }).__grooviqMock;
    mock.refuseNext("project_view", message);
    mock.releaseCommand("project_view");
  }, error);

  await expect(page.getByRole("status", { name: "Messages" }).getByText(error)).toBeVisible();
  await expect(stage.getByText(loading, { exact: true })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Channel rack" })).toHaveCount(0);
});
