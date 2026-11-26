import { parseColor } from "../rack/palette";

/** Resolved role colors shared by CSS, the grid/roll renderers and built-in devices.
 * Keep these hex/rgb compatible: computed custom properties can still contain color-mix. */
export const THEME_COLOR_TOKENS = [
  "--bg", "--panel", "--raised", "--line", "--line-soft", "--text", "--muted", "--faint",
  "--accent", "--on-accent", "--select", "--ok", "--mute", "--warn", "--error", "--paper", "--well", "--rec", "--hair",
  "--ink-1", "--ink-1b", "--ink-2", "--ink-2b", "--ink-3", "--ink-3b",
  "--ink-4", "--ink-4b", "--ink-5", "--ink-5b", "--ink-6", "--ink-6b",
  "--step-a", "--step-b", "--step-lit", "--grid-focus", "--grid-playhead", "--knob-track", "--knob-gap",
  "--roll-row", "--roll-row-black", "--roll-outside", "--roll-dim", "--roll-line", "--roll-beat", "--roll-bar",
  "--roll-select", "--roll-box", "--roll-playhead", "--roll-key-white", "--roll-key-black", "--roll-key-held", "--roll-key-scale",
  "--dev-osc1", "--dev-osc2", "--dev-filter", "--dev-amp", "--dev-lfo", "--dev-mix", "--dev-voice", "--dev-card",
] as const;

type TokenProblem = Readonly<{ token: string; reason: "color" | "pixels" }>;

/** Run once after applying a theme, never in an animation frame. */
export function themeTokenProblems(read: (name: string) => string): TokenProblem[] {
  const problems: TokenProblem[] = [];
  for (const token of THEME_COLOR_TOKENS) {
    if (!parseColor(read(token))) problems.push({ token, reason: "color" });
  }
  for (const token of ["--reg-x", "--reg-y"]) {
    const pixels = read(token).trim();
    if (!/^-?\d+(?:\.\d+)?(?:px)?$/.test(pixels) || !Number.isFinite(parseFloat(pixels))) {
      problems.push({ token, reason: "pixels" });
    }
  }
  for (const token of ["--radius-s", "--radius-m", "--radius-l"]) {
    const pixels = read(token).trim();
    if (!/^\d+(?:\.\d+)?px$/.test(pixels) || !Number.isFinite(parseFloat(pixels))) {
      problems.push({ token, reason: "pixels" });
    }
  }
  return problems;
}
