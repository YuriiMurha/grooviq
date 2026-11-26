import { parseColor } from "../rack/palette";

/** Resolved role colors shared by CSS, the grid/roll renderers and built-in devices.
 * Keep these hex/rgb compatible: computed custom properties can still contain color-mix. */
export const THEME_COLOR_TOKENS = [
  "--bg", "--panel", "--raised", "--line", "--line-soft", "--text", "--muted", "--faint",
  "--accent", "--on-accent", "--play", "--on-play", "--select", "--ok", "--mute", "--warn", "--error",
  "--paper", "--well", "--rec", "--on-rec", "--hair",
  "--ink-1", "--ink-1b", "--ink-2", "--ink-2b", "--ink-3", "--ink-3b",
  "--ink-4", "--ink-4b", "--ink-5", "--ink-5b", "--ink-6", "--ink-6b",
  "--step-a", "--step-b", "--step-lit", "--grid-focus", "--grid-playhead", "--knob-track", "--knob-gap",
  "--roll-row", "--roll-row-black", "--roll-outside", "--roll-dim", "--roll-line", "--roll-beat", "--roll-bar",
  "--roll-select", "--roll-box", "--roll-playhead", "--roll-key-white", "--roll-key-black", "--roll-key-held", "--roll-key-scale",
  "--dev-osc1", "--dev-osc2", "--dev-filter", "--dev-amp", "--dev-lfo", "--dev-mix", "--dev-voice", "--dev-card",
  "--solo",
] as const;

const OFFSET_PATTERN = /^-?\d+(?:\.\d+)?(?:px)?$/;
const RADIUS_PATTERN = /^\d+(?:\.\d+)?px$/;
const PIXEL_PATTERNS = new Map<string, RegExp>([
  ["--reg-x", OFFSET_PATTERN],
  ["--reg-y", OFFSET_PATTERN],
  ["--radius-s", RADIUS_PATTERN],
  ["--radius-m", RADIUS_PATTERN],
  ["--radius-l", RADIUS_PATTERN],
]);
export const REQUIRED_THEME_TOKENS = [...THEME_COLOR_TOKENS, ...PIXEL_PATTERNS.keys()] as const;

type TokenProblem = Readonly<{ token: string; reason: "color" | "pixels" }>;

/** Run once after applying a theme, never in an animation frame. */
export function themeTokenProblems(read: (name: string) => string): TokenProblem[] {
  const problems: TokenProblem[] = [];
  for (const token of REQUIRED_THEME_TOKENS) {
    const value = read(token);
    const pixelPattern = PIXEL_PATTERNS.get(token);
    if (!pixelPattern) {
      if (!parseColor(value)) problems.push({ token, reason: "color" });
    } else if (!pixelPattern.test(value.trim()) || !Number.isFinite(parseFloat(value))) {
      problems.push({ token, reason: "pixels" });
    }
  }
  return problems;
}
