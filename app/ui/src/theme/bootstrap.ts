import { themeTokenProblems } from "./contract";
import { DEFAULT_THEME, readThemePreference, THEME_PREFERENCE_KEY } from "./preference";

/** Read before React mounts so initial CSS and renderer palette reads share one appearance. */
export function initializeTheme() {
  const root = document.documentElement;
  root.dataset.theme = readThemePreference(() => window.localStorage.getItem(THEME_PREFERENCE_KEY));
  const style = getComputedStyle(root);
  const problems = themeTokenProblems((name) => style.getPropertyValue(name));
  if (problems.length > 0) {
    root.dataset.theme = DEFAULT_THEME;
    console.warn("Theme has missing or incompatible tokens:", problems.map((problem) => problem.token).join(", "));
  }
}
