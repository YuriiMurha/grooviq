import { paintTheme } from "./apply";
import { readThemePreference, THEME_PREFERENCE_KEY } from "./preference";

/** Read before React mounts so initial CSS and renderer palette reads share one appearance. */
export function initializeTheme() {
  paintTheme(readThemePreference(() => window.localStorage.getItem(THEME_PREFERENCE_KEY)));
}
