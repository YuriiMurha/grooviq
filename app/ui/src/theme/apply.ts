import { themeTokenProblems } from "./contract";
import { DEFAULT_THEME, resolveThemeId, THEME_PREFERENCE_KEY, type ThemeId } from "./preference";

/** Set the root used by CSS and canvas subscribers, then validate the resolved tokens once. */
export function paintTheme(theme: ThemeId): ThemeId {
  const root = document.documentElement;
  root.dataset.theme = theme;
  const style = getComputedStyle(root);
  const problems = themeTokenProblems((name) => style.getPropertyValue(name));
  if (problems.length === 0) return theme;

  root.dataset.theme = DEFAULT_THEME;
  console.warn("Theme has missing or incompatible tokens:", problems.map((problem) => problem.token).join(", "));
  return DEFAULT_THEME;
}

export function applyTheme(
  requested: unknown,
  write: (theme: ThemeId) => void = (theme) => window.localStorage.setItem(THEME_PREFERENCE_KEY, theme),
): { theme: ThemeId; persisted: boolean } {
  const theme = paintTheme(resolveThemeId(requested));
  if (theme !== requested) return { theme, persisted: false };

  try {
    write(theme);
    return { theme, persisted: true };
  } catch {
    return { theme, persisted: false };
  }
}
