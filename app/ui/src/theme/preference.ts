/** Only shipped themes are registered. Proposed variants must not resolve until ready. */
export const themes = [{ id: "riso", name: "Riso Ink" }] as const;
export type ThemeId = typeof themes[number]["id"];
export const DEFAULT_THEME: ThemeId = "riso";
export const THEME_PREFERENCE_KEY = "grooviq.theme";

export function resolveThemeId(value: unknown): ThemeId {
  return themes.find((theme) => theme.id === value)?.id ?? DEFAULT_THEME;
}

/** The getter itself may throw (private/blocked storage). Never rewrite an unavailable value. */
export function readThemePreference(read: () => string | null): ThemeId {
  try {
    return resolveThemeId(read());
  } catch {
    return DEFAULT_THEME;
  }
}
