import path from "node:path";
import ts from "typescript";

// A module dependency boundary, not a JavaScript sandbox. Value relays are intentionally
// conservative: even an unused raw capability in a relay makes it unsuitable for editors.
export default {
  meta: { type: "problem", schema: [], messages: { raw: "Use useDeviceParams for project edits. Raw project access, uninspectable local runtime modules, computed loads and import.meta.glob are forbidden in device dependencies." } },
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
    const opaqueRuntime = Symbol("local package entry point");
    // Vite's default executable lookup order; an explicit existing file wins first.
    const runtimeExtensions = [".mjs", ".js", ".mts", ".ts", ".jsx", ".tsx", ".json"];
    function runtimeTarget(specifier, from, resolved) {
      let requested;
      if (specifier.startsWith(".") || path.isAbsolute(specifier)) {
        requested = path.resolve(path.dirname(from), specifier);
      } else if (resolved && !resolved.isExternalLibraryImport) {
        const extension = path.extname(specifier);
        if ([".ts", ".tsx", ".mts", ".cts"].includes(extension)) return canonical(resolved.resolvedFileName);
        for (let folder = path.dirname(canonical(resolved.resolvedFileName)); folder !== canonical(root) && path.dirname(folder) !== folder; folder = path.dirname(folder)) {
          if (ts.sys.fileExists(path.join(folder, "package.json"))) return opaqueRuntime;
        }
        const stem = resolved.resolvedFileName.replace(/\.(?:[cm]?ts|tsx|jsx?)$/i, "");
        requested = [".js", ".mjs", ".cjs", ".jsx"].includes(extension) ? stem + extension : stem;
      } else return undefined;
      const existing = (file) => ts.sys.fileExists(file) ? canonical(file) : undefined;
      const direct = existing(requested);
      if (direct) return direct;
      const extension = path.extname(requested);
      const substitutions = { ".js": [".ts", ".tsx"], ".jsx": [".tsx"], ".mjs": [".mts"], ".cjs": [".cts"] };
      for (const suffix of substitutions[extension] ?? []) {
        const file = existing(requested.slice(0, -extension.length) + suffix);
        if (file) return file;
      }
      for (const suffix of runtimeExtensions) {
        const file = existing(requested + suffix);
        if (file) return file;
      }
      if (ts.sys.directoryExists(requested)) {
        // Conditional package entries need a bundler-aware resolver; require an explicit file.
        if (ts.sys.fileExists(path.join(requested, "package.json"))) return opaqueRuntime;
        for (const suffix of runtimeExtensions) {
          const file = existing(path.join(requested, "index" + suffix));
          if (file) return file;
        }
      }
      return undefined;
    }
    function unsafe(specifier, from, seen) {
      if (specifier.startsWith("#") || /^file:/i.test(specifier)) return true;
      // An asset-looking path can resolve to executable source such as knob.svg.ts.
      // Exempt data assets only when the module resolver finds no source to inspect.
      const dataAsset = /\.(css|svg|png|jpe?g|gif|webp|avif|ico|woff2?|ttf|otf)(\?(url|raw|inline|no-inline))?$/i.test(specifier);
      if (dataAsset && /\?(raw|url)$/.test(specifier)) return false;
      // Vite can execute a .css.ts relay even when the import has an asset query.
      const sourceSpecifier = specifier.split("?", 1)[0];
      const resolved = ts.resolveModuleName(sourceSpecifier, from, options, ts.sys).resolvedModule;
      const runtime = runtimeTarget(sourceSpecifier, from, resolved);
      if (runtime === opaqueRuntime) return true;
      if (!resolved) {
        const actualData = typeof runtime === "string" && /\.(css|svg|png|jpe?g|gif|webp|avif|ico|woff2?|ttf|otf)$/i.test(runtime);
        return !(dataAsset && actualData) && (specifier.startsWith(".") || path.isAbsolute(specifier));
      }
      const file = canonical(resolved.resolvedFileName);
      if (protectedFiles.has(file)) return true;
      if (resolved.isExternalLibraryImport) return false;
      // TypeScript may substitute a .ts sibling while Vite executes the actual .js file.
      if (runtime && runtime !== file) return true;
      if (file === bridge || seen.has(file)) return false;
      // Local declarations can hide a different runtime .js dependency graph.
      // Type-only imports are allowed, but value imports need inspectable source.
      if (resolved.extension.startsWith(".d.")) return true;
      seen.add(file);
      const source = ts.sys.readFile(file);
      if (source === undefined) return true;
      const tree = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
      ts.bindSourceFile(tree, options);
      function localRequire(node) {
        for (let scope = node; scope; scope = scope.parent) {
          const symbol = scope.locals?.get("require");
          if (symbol?.flags & (ts.SymbolFlags.Value | ts.SymbolFlags.Alias)) return true;
        }
        return false;
      }
      let bad = false;
      function visit(node) {
        if (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) {
          const clause = ts.isImportDeclaration(node) ? node.importClause : node;
          // Named type specifiers can emit an empty import/export under verbatimModuleSyntax.
          const typeOnly = clause?.isTypeOnly;
          if (!typeOnly && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) bad ||= unsafe(node.moduleSpecifier.text, file, seen);
        }
        if (ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || (ts.isIdentifier(node.expression) && node.expression.text === "require" && !localRequire(node)))) {
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
        if (node.importKind === "type") return;
        check(node, node.source);
      },
      ExportNamedDeclaration(node) {
        if (node.source && node.exportKind !== "type") check(node, node.source);
      },
      ExportAllDeclaration(node) { if (node.exportKind !== "type") check(node, node.source); },
      ImportExpression(node) { check(node, node.source); },
      CallExpression(node) {
        const expr = node.callee;
        if (expr.type === "Identifier" && expr.name === "require") {
          for (let scope = context.sourceCode.getScope(node); scope; scope = scope.upper) {
            if (scope.set.get("require")?.defs.length) return;
          }
          check(node, node.arguments[0]); return;
        }
        if (expr.type !== "MemberExpression") return;
        const member = expr.computed ? expr.property.value : expr.property.name;
        const receiver = expr.object;
        if (member === "glob" && receiver.type === "MetaProperty" && receiver.meta.name === "import" && receiver.property.name === "meta") context.report({ node, messageId: "raw" });
      },
    };
  },
};
