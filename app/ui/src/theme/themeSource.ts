/** Read a flat theme rule from source, before the CSS build rewrites values. */
export function parseThemeDeclarations(css: string, selector: string): Map<string, string> {
  const declarations = new Map<string, string>();
  const source = css.replace(/\/\*[\s\S]*?\*\//g, "");
  for (const [, ruleSelector, body] of source.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    if (ruleSelector.trim().replaceAll("'", '"').replace(/\s*=\s*/g, "=") !== selector) continue;
    for (const [, name, value] of body.matchAll(/(?:^|;)\s*(--[\w-]+)\s*:\s*([^;]*)/g)) {
      declarations.set(name, value.trim());
    }
  }
  return declarations;
}

/** Resolve the theme files' var(--token) references after applying the variant overrides. */
export function resolveThemeDeclarations(declarations: ReadonlyMap<string, string>): Map<string, string> {
  const resolving = new Set<string>();
  const resolved = new Map<string, string>();
  function resolve(name: string): string {
    const cached = resolved.get(name);
    if (cached !== undefined) return cached;
    const value = declarations.get(name);
    if (value === undefined || resolving.has(name)) return "";

    resolving.add(name);
    let missing = false;
    const expanded = value.replace(/\bvar\(\s*(--[\w-]+)\s*\)/g, (_, reference: string) => {
      const replacement = resolve(reference);
      missing ||= replacement === "";
      return replacement;
    });
    resolving.delete(name);

    const result = missing ? "" : expanded;
    resolved.set(name, result);
    return result;
  }
  for (const name of declarations.keys()) resolve(name);
  return resolved;
}
