/** Read a flat theme rule from source, before the CSS build rewrites values. */
export function parseThemeDeclarations(css: string, selector: string): Map<string, string> {
  const declarations = new Map<string, string>();
  const source = css.replace(/\/\*[\s\S]*?\*\//g, "");
  let depth = 0;
  let start = 0;
  let inThemeRule = false;
  // Strings can contain braces or semicolons without starting a rule or ending a declaration.
  for (const { 1: delimiter, index } of source.matchAll(/"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|([{};])/gs)) {
    if (!delimiter) continue;
    const statement = source.slice(start, index).trim();
    start = index + 1;
    if (delimiter === "{") {
      if (inThemeRule) throw new Error(`${selector} must not contain nested blocks`);
      if (statement.replaceAll("'", '"').replace(/\s*=\s*/g, "=") === selector) {
        if (depth !== 0) throw new Error(`${selector} must be a top-level rule`);
        inThemeRule = true;
      }
      depth++;
      continue;
    }
    if (inThemeRule) {
      const declaration = /^(--[\w-]+)\s*:\s*(.*)$/s.exec(statement);
      if (declaration) declarations.set(declaration[1], declaration[2]);
    }
    if (delimiter === "}") {
      inThemeRule = false;
      depth--;
      if (depth < 0) throw new Error(`Unbalanced braces while reading ${selector}`);
    }
  }
  if (depth !== 0) throw new Error(`Unbalanced braces while reading ${selector}`);
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
