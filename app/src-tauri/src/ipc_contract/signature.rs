use heck::{ToLowerCamelCase, ToSnakeCase};
use std::collections::{BTreeMap, BTreeSet};
use syn::{
    Expr, FnArg, GenericArgument, Item, ItemFn, Lit, Meta, Pat, Path, PathArguments, ReturnType,
    Token, Type, UseTree, ext::IdentExt, punctuated::Punctuated, visit::Visit,
};
use ts_rs::TS;

pub(super) struct Command<'a> {
    pub name: String,
    pub signature: Option<Signature>,
    pub deferred: Option<&'a str>,
}

pub(super) struct Signature {
    pub args: Vec<String>,
    pub reply: String,
}

pub(super) fn commands<'a>(
    root: &str,
    migrated: &[&str],
    deferred: &[(&str, &'a str)],
    read_module: impl Fn(&str) -> Result<String, String>,
) -> Result<Vec<Command<'a>>, String> {
    let root = syn::parse_file(root).map_err(|error| format!("lib.rs: {error}"))?;
    let mut registry = Registry::default();
    registry.visit_file(&root);
    let [handler] = registry.0.as_slice() else {
        return Err("expected exactly one tauri::generate_handler! registry".into());
    };
    let paths = handler
        .parse_body_with(Punctuated::<Path, Token![,]>::parse_terminated)
        .map_err(|error| format!("generate_handler!: {error}"))?;
    if paths.is_empty() {
        return Err("IPC command inventory is empty".into());
    }
    let mut classifications = BTreeMap::new();
    for (path, reason) in migrated
        .iter()
        .map(|path| (*path, None))
        .chain(deferred.iter().map(|(path, reason)| (*path, Some(*reason))))
    {
        if classifications.insert(path, reason).is_some() {
            return Err(format!("IPC command classified twice: {path}"));
        }
    }

    let mut modules = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut commands = Vec::new();
    for path in paths {
        let Some((module, function)) = plain_path(&path)
            .and_then(|path| {
                path.split_once("::")
                    .map(|(a, b)| (a.to_owned(), b.to_owned()))
            })
            .filter(|(_, function)| !function.contains("::"))
        else {
            return Err("expected a module::function path in generate_handler!".into());
        };
        let path = format!("{module}::{function}");
        let reason = classifications
            .remove(path.as_str())
            .ok_or_else(|| format!("unclassified command {path} in generate_handler!"))?;
        if !matches!(root.items.iter().find(|item| matches!(item, Item::Mod(decl)
            if decl.ident == module)),
            Some(Item::Mod(decl)) if decl.content.is_none() && decl.attrs.is_empty())
        {
            return Err(format!("unsupported module declaration for {path}"));
        }
        if let std::collections::btree_map::Entry::Vacant(slot) = modules.entry(module.clone()) {
            let file = syn::parse_file(&read_module(&module)?)
                .map_err(|error| format!("{module}.rs: {error}"))?;
            slot.insert(file);
        }
        let items = &modules[&module].items;
        let mut functions = items.iter().filter_map(|item| match item {
            Item::Fn(item) if item.sig.ident == function => Some(item),
            _ => None,
        });
        let item = functions
            .next()
            .filter(|_| functions.next().is_none())
            .ok_or_else(|| format!("expected one function for {path}"))?;
        let (name, snake_case) =
            command_options(item).map_err(|error| format!("{path}: {error}"))?;
        if !names.insert(name.clone()) {
            return Err(format!("duplicate Tauri command name {name}"));
        }
        let signature = if reason.is_none() {
            Some(signature(item, items, snake_case).map_err(|error| format!("{path}: {error}"))?)
        } else {
            None
        };
        commands.push(Command {
            name,
            signature,
            deferred: reason,
        });
    }
    if !classifications.is_empty() {
        return Err(format!(
            "classified commands absent from generate_handler!: {:?}",
            classifications.keys()
        ));
    }
    if !commands.iter().any(|command| command.signature.is_some()) {
        return Err("migrated IPC command inventory is empty".into());
    }
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(commands)
}

#[derive(Default)]
struct Registry<'ast>(Vec<&'ast syn::Macro>);

impl<'ast> Visit<'ast> for Registry<'ast> {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if plain_path(&mac.path).as_deref() == Some("tauri::generate_handler") {
            self.0.push(mac);
        }
    }
}

fn plain_path(path: &Path) -> Option<String> {
    if path.segments.iter().any(|s| !s.arguments.is_empty()) {
        return None;
    }
    Some(
        path.segments
            .iter()
            .map(|s| s.ident.unraw().to_string())
            .collect::<Vec<_>>()
            .join("::"),
    )
}

fn command_options(item: &ItemFn) -> Result<(String, bool), String> {
    if item
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr"))
    {
        return Err("conditional commands are unsupported".into());
    }
    let mut attrs = item
        .attrs
        .iter()
        .filter(|attr| plain_path(attr.path()).as_deref() == Some("tauri::command"));
    let Some(attr) = attrs.next().filter(|_| attrs.next().is_none()) else {
        return Err("expected a single #[tauri::command]".into());
    };
    let mut name = item.sig.ident.unraw().to_string();
    let mut snake_case = false;
    match &attr.meta {
        Meta::Path(_) => {}
        Meta::List(_) => {
            let options = attr
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .map_err(|error| error.to_string())?;
            for option in options {
                match option {
                    Meta::Path(path) if path.is_ident("async") => {}
                    Meta::NameValue(option) => {
                        let Expr::Lit(literal) = option.value else {
                            return Err("expected a string Tauri command option".into());
                        };
                        let Lit::Str(value) = literal.lit else {
                            return Err("expected a string Tauri command option".into());
                        };
                        if option.path.is_ident("rename_all") {
                            snake_case = match value.value().as_str() {
                                "snake_case" => true,
                                "camelCase" => false,
                                _ => return Err("unsupported Tauri argument casing".into()),
                            };
                        } else if option.path.is_ident("rename") {
                            name = value.value();
                        } else {
                            return Err("unsupported Tauri command option".into());
                        }
                    }
                    _ => return Err("unsupported Tauri command option".into()),
                }
            }
        }
        Meta::NameValue(_) => return Err("unsupported Tauri command attribute".into()),
    }
    if syn::parse_str::<syn::Ident>(&name).is_err() || name.contains("r#") {
        return Err(format!("unsupported Tauri command name {name:?}"));
    }
    Ok((name, snake_case))
}

fn imports(items: &[Item]) -> BTreeMap<String, String> {
    fn walk(tree: &UseTree, prefix: &str, found: &mut BTreeMap<String, String>) {
        match tree {
            UseTree::Path(path) => walk(&path.tree, &format!("{prefix}{}::", path.ident), found),
            UseTree::Group(group) => group
                .items
                .iter()
                .for_each(|tree| walk(tree, prefix, found)),
            UseTree::Name(name) => {
                found.insert(name.ident.to_string(), format!("{prefix}{}", name.ident));
            }
            UseTree::Rename(alias) => {
                found.insert(alias.rename.to_string(), format!("{prefix}{}", alias.ident));
            }
            UseTree::Glob(_) => {}
        }
    }
    let mut found = BTreeMap::new();
    for item in items {
        let local = match item {
            Item::Use(item) => {
                walk(&item.tree, "", &mut found);
                continue;
            }
            Item::Enum(item) => &item.ident,
            Item::Mod(item) => &item.ident,
            Item::Struct(item) => &item.ident,
            Item::Trait(item) => &item.ident,
            Item::Type(item) => &item.ident,
            Item::Union(item) => &item.ident,
            _ => continue,
        };
        found.insert(local.to_string(), format!("self::{local}"));
    }
    found
}

fn signature(item: &ItemFn, items: &[Item], snake_case: bool) -> Result<Signature, String> {
    let imports = imports(items);
    let mut args = Vec::new();
    let mut names = BTreeSet::new();
    if !item.sig.generics.params.is_empty() || item.sig.variadic.is_some() {
        return Err("generic or variadic IPC command".into());
    }
    for arg in &item.sig.inputs {
        let FnArg::Typed(arg) = arg else {
            return Err("unexpected IPC receiver".into());
        };
        let Pat::Ident(ident) = arg.pat.as_ref() else {
            return Err("unsupported IPC argument pattern".into());
        };
        if !arg.attrs.is_empty() || ident.subpat.is_some() {
            return Err("unsupported IPC argument modifiers".into());
        }
        let Ty { name, params } = ty(&arg.ty, &imports)?;
        if matches!(name.as_str(), "tauri::State" | "tauri::AppHandle") {
            continue;
        }
        let key = ident.ident.unraw().to_string();
        let key = if snake_case {
            key.to_snake_case()
        } else {
            key.to_lower_camel_case()
        };
        if !names.insert(key.clone()) {
            return Err(format!("duplicate IPC argument {key}"));
        }
        let optional = if name == "Option" { "?" } else { "" };
        args.push(format!(
            "{key}{optional}: {}",
            ts_type(&name, &params, &imports)?
        ));
    }
    let reply = match &item.sig.output {
        ReturnType::Default => "null".into(),
        ReturnType::Type(_, value) => {
            let Ty { name, params } = ty(value, &imports)?;
            match (name.as_str(), params.as_slice()) {
                ("Result", [ok, error]) => {
                    let error = ty(error, &imports)?;
                    if error.name != "String" || !error.params.is_empty() {
                        return Err("expected Result<T, String> for an IPC reply".into());
                    }
                    render_type(ok, &imports)?
                }
                _ => ts_type(&name, &params, &imports)?,
            }
        }
    };
    Ok(Signature { args, reply })
}

struct Ty<'a> {
    name: String,
    params: Vec<&'a Type>,
}

fn ty<'a>(value: &'a Type, imports: &BTreeMap<String, String>) -> Result<Ty<'a>, String> {
    if matches!(value, Type::Tuple(tuple) if tuple.elems.is_empty()) {
        return Ok(Ty {
            name: "()".into(),
            params: vec![],
        });
    }
    let Type::Path(path) = value else {
        return Err("unsupported IPC type syntax".into());
    };
    if path.qself.is_some()
        || path
            .path
            .segments
            .iter()
            .rev()
            .skip(1)
            .any(|s| !s.arguments.is_empty())
    {
        return Err("unsupported IPC type path".into());
    }
    let mut segments = path
        .path
        .segments
        .iter()
        .map(|s| s.ident.unraw().to_string());
    let first = segments.next().ok_or("empty IPC type path")?;
    let mut name = if path.path.leading_colon.is_none() {
        imports.get(&first).cloned().unwrap_or(first)
    } else {
        first
    };
    for segment in segments {
        name.push_str("::");
        name.push_str(&segment);
    }
    let params = match &path.path.segments.last().unwrap().arguments {
        PathArguments::None => vec![],
        _ if matches!(name.as_str(), "tauri::State" | "tauri::AppHandle") => vec![],
        PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .map(|arg| match arg {
                GenericArgument::Type(value) => Ok(value),
                _ => Err("unsupported IPC type parameter".into()),
            })
            .collect::<Result<_, String>>()?,
        PathArguments::Parenthesized(_) => return Err("unsupported IPC type parameters".into()),
    };
    Ok(Ty { name, params })
}

fn render_type(value: &Type, imports: &BTreeMap<String, String>) -> Result<String, String> {
    let Ty { name, params } = ty(value, imports)?;
    ts_type(&name, &params, imports)
}

fn ts_type(
    name: &str,
    params: &[&Type],
    imports: &BTreeMap<String, String>,
) -> Result<String, String> {
    match (name, params) {
        ("()", []) => Ok("null".into()),
        ("bool", []) => Ok("boolean".into()),
        ("String", []) => Ok("string".into()),
        ("u8" | "u16" | "u32" | "i8" | "i16" | "i32" | "f32" | "f64", []) => Ok("number".into()),
        ("Option", [value]) => Ok(format!("{} | null", render_type(value, imports)?)),
        ("Vec", [value]) => Ok(format!("Array<{}>", render_type(value, imports)?)),
        ("grooviq_project::session::sampler_trim::TrimView", []) => {
            use grooviq_project::session::sampler_trim::TrimView;
            Ok(TrimView::name(&ts_rs::Config::default()))
        }
        _ => Err(format!("unknown IPC type {name}")),
    }
}
