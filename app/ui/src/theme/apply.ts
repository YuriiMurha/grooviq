import { themeTokenProblems } from "./contract";
import { DEFAULT_THEME, resolveThemeId, THEME_PREFERENCE_KEY, type ThemeId } from "./preference";

/** Paint the theme, returning null if token validation caused a fallback to Riso. */
export function paintTheme(theme: ThemeId): ThemeId | null {
  const root = document.documentElement;
  root.dataset.theme = theme;
  const style = getComputedStyle(root);
  const problems = themeTokenProblems((name) => style.getPropertyValue(name));
  if (problems.length === 0) return theme;

  root.dataset.theme = DEFAULT_THEME;
  console.warn("Theme has missing or incompatible tokens:", problems.map((problem) => problem.token).join(", "));
  return null;
}

export type ThemeApplication = { theme: ThemeId; status: "applied" | "session-only" | "unavailable" };

export function applyTheme(
  requested: unknown,
  write: (theme: ThemeId) => void = (theme) => window.localStorage.setItem(THEME_PREFERENCE_KEY, theme),
): ThemeApplication {
  const theme = paintTheme(resolveThemeId(requested));
  if (theme === null || theme !== requested) return { theme: theme ?? DEFAULT_THEME, status: "unavailable" };

  try {
    write(theme);
    return { theme, status: "applied" };
  } catch {
    return { theme, status: "session-only" };
  }
}
