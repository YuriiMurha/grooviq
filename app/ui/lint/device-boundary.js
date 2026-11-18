import path from "node:path";
import ts from "typescript";

// A module dependency boundary, not a JavaScript sandbox. Value relays are intentionally
// conservative: even an unused raw capability in a relay makes it unsuitable for editors.
export default {
  meta: { type: "problem", schema: [], messages: { raw: "Device value dependencies must use useDeviceParams; raw project access, unresolved local modules, computed imports and import.meta.glob are forbidden." } },
  create(context) {
    const root = context.cwd;
    const canonical = (file) => {
      const absolute = path.resolve(file);
      try { return ts.sys.realpath?.(absolute) ?? absolute; }
      catch { return absolute; }
    };
    const bridge = canonical(path.resolve(root, "src/devices/useDeviceParams.ts"));
    const filename = canonical(context.filename);
    if (filename === bridge || filename.endsWith(".test.ts")) return {};
    const protectedFiles = new Set(["src/project/context.ts", "src/project/useSession.ts", "src/api/project.ts", "src/devices/paramSink.ts"].map((p) => canonical(path.resolve(root, p))));
    const config = ts.readConfigFile(path.join(root, "tsconfig.json"), ts.sys.readFile);
    const options = ts.parseJsonConfigFileContent(config.config ?? {}, ts.sys, root).options;
    function unsafe(specifier, from, seen) {
      // An asset-looking path can resolve to executable source such as knob.svg.ts.
      // Exempt data assets only when the module resolver finds no source to inspect.
      const dataAsset = /\.(css|svg|png|jpe?g|gif|webp|avif|ico|woff2?|ttf|otf)(\?(url|raw|inline|no-inline))?$/i.test(specifier);
      if (dataAsset && /\?(raw|url)$/.test(specifier)) return false;
      // Vite can execute a .css.ts relay even when the import has an asset query.
      const sourceSpecifier = specifier.split("?", 1)[0];
      const resolved = ts.resolveModuleName(sourceSpecifier, from, options, ts.sys).resolvedModule;
      if (!resolved) return !dataAsset && (specifier.startsWith(".") || specifier.startsWith("/"));
      const file = canonical(resolved.resolvedFileName);
      if (file === bridge) return false;
      if (protectedFiles.has(file)) return true;
      if (resolved.isExternalLibraryImport || seen.has(file)) return false;
      // Local declarations can hide a different runtime .js dependency graph.
      // Type-only imports are allowed, but value imports need inspectable source.
      if (/\.d\.(ts|mts|cts)$/i.test(file)) return true;
      seen.add(file);
      const source = ts.sys.readFile(file);
      if (source === undefined) return true;
      const tree = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
      let bad = false;
      function visit(node) {
        if (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) {
          const clause = ts.isImportDeclaration(node) ? node.importClause : node;
          const bindings = clause && 'namedBindings' in clause ? clause.namedBindings : node.exportClause;
          const typeOnly = clause?.isTypeOnly || (!clause?.name && bindings && ts.isNamedImports(bindings) && bindings.elements.length > 0 && bindings.elements.every((e) => e.isTypeOnly)) || (bindings && ts.isNamedExports(bindings) && bindings.elements.length > 0 && bindings.elements.every((e) => e.isTypeOnly));
          if (!typeOnly && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) bad ||= unsafe(node.moduleSpecifier.text, file, seen);
        }
        if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) {
          const arg = node.arguments[0];
          bad ||= !arg || !ts.isStringLiteral(arg) || unsafe(arg.text, file, seen);
        }
        if (ts.isCallExpression(node)) {
          const expr = node.expression;
          const member = ts.isPropertyAccessExpression(expr) ? expr.name.text : ts.isElementAccessExpression(expr) && ts.isStringLiteral(expr.argumentExpression) ? expr.argumentExpression.text : null;
          const receiver = member !== null ? expr.expression : null;
          if (member === "glob" && receiver && ts.isMetaProperty(receiver) && receiver.keywordToken === ts.SyntaxKind.ImportKeyword && receiver.name.text === "meta") bad = true;
        }
        ts.forEachChild(node, visit);
      }
      visit(tree);
      return bad;
    }
    function check(node, source) {
      if (!source || typeof source.value !== "string" || unsafe(source.value, filename, new Set())) context.report({ node, messageId: "raw" });
    }
    return {
      ImportDeclaration(node) {
        if (node.importKind === "type" || (node.specifiers.length && node.specifiers.every((s) => s.importKind === "type"))) return;
        check(node, node.source);
      },
      ExportNamedDeclaration(node) {
        if (node.source && node.exportKind !== "type" && !(node.specifiers.length && node.specifiers.every((s) => s.exportKind === "type"))) check(node, node.source);
      },
      ExportAllDeclaration(node) { if (node.exportKind !== "type") check(node, node.source); },
      ImportExpression(node) { check(node, node.source); },
      CallExpression(node) {
        const expr = node.callee;
        if (expr.type !== "MemberExpression") return;
        const member = expr.computed ? expr.property.value : expr.property.name;
        const receiver = expr.object;
        if (member === "glob" && receiver.type === "MetaProperty" && receiver.meta.name === "import" && receiver.property.name === "meta") context.report({ node, messageId: "raw" });
      },
    };
  },
};
