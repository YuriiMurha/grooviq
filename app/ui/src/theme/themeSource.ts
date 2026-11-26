const CSS_STRING = /"(?:\\(?:\r\n|.)|[^"\\\n\r\f])*"|'(?:\\(?:\r\n|.)|[^'\\\n\r\f])*'/;
// Consume complete comments and strings before looking for unsupported tokens or delimiters.
const THEME_SYNTAX = new RegExp([
  /(\/\*.*?\*\/)/.source,
  CSS_STRING.source,
  /((?<![\w\u0080-\uFFFF-])url\(|[!"'\\]|\/\*)/.source,
  /([{};()[\]])/.source,
].join("|"), "gis");
const THEME_VALUE_TOKEN = new RegExp([
  CSS_STRING.source,
  /\bvar\(\s*(--[\w-]+)\s*\)/.source,
].join("|"), "gs");

/** Read a theme file's single flat rule before the CSS build rewrites values. */
export function parseThemeDeclarations(css: string, selector: string): Map<string, string> {
  const declarations = new Map<string, string>();
  const groups: string[] = [];
  let start = 0;
  let statement = "";
  let inThemeRule = false;
  let seenThemeRule = false;
  for (const match of css.matchAll(THEME_SYNTAX)) {
    const [token, comment, unsupported, delimiter] = match;
    if (unsupported === "!") throw new Error(`Unsupported CSS "!" while reading ${selector}`);
    if (unsupported === "\\") throw new Error(`Unsupported CSS escape while reading ${selector}`);
    if (unsupported === '"' || unsupported === "'") throw new Error(`Unterminated CSS string while reading ${selector}`);
    if (unsupported === "/*") throw new Error(`Unterminated CSS comment while reading ${selector}`);
    if (unsupported) throw new Error(`Unsupported CSS url() while reading ${selector}`);
    // Let the next structural token collect the whole quoted value.
    if (!comment && !delimiter) continue;
    if (delimiter === "(" || delimiter === "[") {
      groups.push(delimiter === "(" ? ")" : "]");
      continue;
    }
    if (delimiter === ")" || delimiter === "]") {
      if (groups.pop() !== delimiter) throw new Error(`Unbalanced delimiters while reading ${selector}`);
      continue;
    }
    if (delimiter === ";" && groups.length) continue;
    const { index } = match;
    statement += css.slice(start, index);
    start = index + token.length;
    if (comment) statement += " ";
    if (!delimiter) continue;
    const text = statement.trim();
    statement = "";
    if (delimiter === "{") {
      if (inThemeRule) throw new Error(`${selector} must not contain nested blocks`);
      if (text.replaceAll("'", '"').replace(/\s*=\s*/g, "=") !== selector) {
        throw new Error(`${selector} must be a top-level rule; unexpected selector ${JSON.stringify(text)}`);
      }
      if (seenThemeRule) throw new Error(`${selector} must be the only rule`);
      seenThemeRule = true;
      inThemeRule = true;
      continue;
    }
    if (!inThemeRule) {
      if (delimiter === "}") throw new Error(`Unbalanced braces while reading ${selector}`);
      throw new Error(`Unexpected top-level statement ${JSON.stringify(text)} while reading ${selector}`);
    }
    if (groups.length) throw new Error(`Unbalanced delimiters while reading ${selector}`);
    const declaration = /^(--[\w-]+)\s*:\s*(.*)$/s.exec(text);
    if (text && !declaration) {
      throw new Error(`Unsupported declaration ${JSON.stringify(text)} while reading ${selector}`);
    }
    if (declaration) {
      const [, name, value] = declaration;
      declarations.set(name, value);
    }
    if (delimiter === "}") inThemeRule = false;
  }
  if (inThemeRule || groups.length) throw new Error(`Unclosed theme rule or group while reading ${selector}`);
  const trailing = (statement + css.slice(start)).trim();
  if (trailing) throw new Error(`Unexpected top-level statement ${JSON.stringify(trailing)} while reading ${selector}`);
  if (!seenThemeRule) throw new Error(`Missing theme rule ${selector}`);
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
    const expanded = value.replace(THEME_VALUE_TOKEN, (token: string, reference: string | undefined) => {
      if (reference === undefined) return token;
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
