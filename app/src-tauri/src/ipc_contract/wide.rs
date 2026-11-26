//! The TS derive's default wide-integer output is not valid for Tauri's JSON numbers.

use syn::{
    Attribute, Expr, Field, Item, Lit, Meta, Token, TypePath, punctuated::Punctuated, visit::Visit,
};

use std::collections::{BTreeMap, BTreeSet};
use syn::parse::Parser;

type Aliases<'a> = BTreeMap<String, Vec<&'a syn::Type>>;

pub fn validate_sources<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), String> {
    let files: Vec<_> = sources
        .into_iter()
        .map(|(path, source)| {
            syn::parse_file(source)
                .map(|file| (path, file))
                .map_err(|error| format!("{path}: {error}"))
        })
        .collect::<Result<_, _>>()?;
    let mut aliases = Aliases::new();
    for (_, file) in &files {
        collect_aliases(&file.items, &mut aliases);
    }
    for (path, file) in &files {
        validate_items(&file.items, &aliases).map_err(|error| format!("{path}: {error}"))?;
    }
    Ok(())
}

fn collect_aliases<'a>(items: &'a [Item], aliases: &mut Aliases<'a>) {
    for item in items {
        match item {
            Item::Type(item) => aliases
                .entry(item.ident.to_string())
                .or_default()
                .push(&item.ty),
            Item::Mod(item) if let Some((_, items)) = &item.content => {
                collect_aliases(items, aliases)
            }
            _ => {}
        }
    }
}

fn validate_items(items: &[Item], aliases: &Aliases<'_>) -> Result<(), String> {
    for item in items {
        match item {
            Item::Struct(item) if derives_ts(&item.attrs)? => {
                validate_fields(&item.ident.to_string(), &item.fields, aliases)?;
            }
            Item::Enum(item) if derives_ts(&item.attrs)? => {
                for variant in &item.variants {
                    let owner = format!("{}::{}", item.ident, variant.ident);
                    validate_fields(&owner, &variant.fields, aliases)?;
                }
            }
            Item::Mod(item) if let Some((_, items)) = &item.content => {
                validate_items(items, aliases)?
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_fields<'a>(
    owner: &str,
    fields: impl IntoIterator<Item = &'a Field>,
    aliases: &Aliases<'_>,
) -> Result<(), String> {
    for (index, field) in fields.into_iter().enumerate() {
        let mut wide = WideInteger {
            aliases,
            seen: BTreeSet::new(),
            found: false,
        };
        wide.visit_type(&field.ty);
        let label = field
            .ident
            .as_ref()
            .map_or_else(|| index.to_string(), ToString::to_string);
        let override_type = type_override(&field.attrs)?;
        if wide.found {
            let ts_type = number_type(&field.ty, aliases, &mut BTreeSet::new()).ok_or_else(|| {
                format!("{owner}.{label}: unsupported container or ambiguous alias for wide IPC integer")
            })?;
            if override_type.as_deref() != Some(ts_type) || !has_range(&field.attrs) {
                return Err(format!(
                    "{owner}.{label}: wide IPC integer requires ts(type = \"{ts_type}\") and a JSON range: doc comment"
                ));
            }
        } else if matches!(override_type.as_deref(), Some("number" | "number | null"))
            && !has_range(&field.attrs)
        {
            return Err(format!(
                "{owner}.{label}: IPC number override requires a JSON range: doc comment"
            ));
        }
    }
    Ok(())
}

struct WideInteger<'a> {
    aliases: &'a Aliases<'a>,
    seen: BTreeSet<String>,
    found: bool,
}

fn is_wide(part: &syn::PathSegment) -> bool {
    matches!(
        part.ident.to_string().as_str(),
        "u64"
            | "i64"
            | "u128"
            | "i128"
            | "NonZeroU64"
            | "NonZeroI64"
            | "NonZeroU128"
            | "NonZeroI128"
    )
}

fn number_type(
    ty: &syn::Type,
    aliases: &Aliases<'_>,
    seen: &mut BTreeSet<String>,
) -> Option<&'static str> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let part = path.path.segments.last()?;
    if path.qself.is_some() {
        return None;
    }
    match &part.arguments {
        syn::PathArguments::None if is_wide(part) => Some("number"),
        syn::PathArguments::None => {
            let name = part.ident.to_string();
            if !seen.insert(name.clone()) {
                return None;
            }
            let [target] = aliases.get(&name)?.as_slice() else {
                return None;
            };
            number_type(target, aliases, seen)
        }
        syn::PathArguments::AngleBracketed(args) if part.ident == "Option" => {
            let Some(syn::GenericArgument::Type(inner)) = args.args.first() else {
                return None;
            };
            (args.args.len() == 1 && number_type(inner, aliases, seen) == Some("number"))
                .then_some("number | null")
        }
        _ => None,
    }
}

impl<'ast> Visit<'ast> for WideInteger<'_> {
    fn visit_type_path(&mut self, path: &'ast TypePath) {
        if let Some(part) = path.path.segments.last() {
            self.found |= is_wide(part);
            let name = part.ident.to_string();
            // A source-only check resolves workspace aliases conservatively by their last
            // segment. An ambiguous wide alias must be spelled using the concrete integer.
            if let Some(targets) = self.aliases.get(&name)
                && self.seen.insert(name)
            {
                for target in targets {
                    self.visit_type(target);
                }
            }
        }
        syn::visit::visit_type_path(self, path);
    }
}

fn derives_ts(attrs: &[Attribute]) -> Result<bool, String> {
    for meta in feature_attributes(attrs)? {
        if let Meta::List(list) = meta
            && list.path.is_ident("derive")
            && list
                .parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)
                .map_err(|error| error.to_string())?
                .iter()
                .any(|path| path.segments.last().is_some_and(|part| part.ident == "TS"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn type_override(attrs: &[Attribute]) -> Result<Option<String>, String> {
    let mut found = None;
    for meta in feature_attributes(attrs)? {
        if let Meta::List(list) = meta
            && list.path.is_ident("ts")
        {
            syn::meta::parser(|option| {
                if option.path.is_ident("type") {
                    let value: syn::LitStr = option.value()?.parse()?;
                    found = Some(value.value());
                } else if option.input.peek(Token![=]) {
                    let _: Expr = option.value()?.parse()?;
                }
                Ok(())
            })
            .parse2(list.tokens)
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(found)
}

fn has_range(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        matches!(&attr.meta, Meta::NameValue(pair)
        if pair.path.is_ident("doc") && string(&pair.value).is_some_and(|doc|
            doc.trim().strip_prefix("JSON range:").is_some_and(|range| !range.trim().is_empty())))
    })
}

fn string(value: &Expr) -> Option<String> {
    let Expr::Lit(expr) = value else { return None };
    let Lit::Str(text) = &expr.lit else {
        return None;
    };
    Some(text.value())
}

fn feature_attributes(attrs: &[Attribute]) -> Result<Vec<Meta>, String> {
    let mut found = Vec::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("cfg_attr")) {
        let args = attr
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .map_err(|error| error.to_string())?;
        let mut args = args.into_iter();
        if matches!(args.next(), Some(Meta::NameValue(pair))
            if pair.path.is_ident("feature") && string(&pair.value).as_deref() == Some("ipc-contract"))
        {
            found.extend(args);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::validate_sources;

    fn validate(source: &str) -> Result<(), String> {
        validate_sources([("sample.rs", source)])
    }

    const WITH_RANGE: &str = r#"
        #[cfg_attr(feature = "ipc-contract", derive(ts_rs::TS))]
        struct Stamp {
            /// JSON range: 0..=2^53 - 1.
            #[cfg_attr(feature = "ipc-contract", ts(type = "number | null"))]
            count: Option<u64>,
        }
    "#;

    #[test]
    fn wide_integers_require_an_explicit_number_override_and_range() {
        validate(WITH_RANGE).unwrap();
        let aliased = WITH_RANGE.replace("Option<u64>", "Option<Count>");
        let validate_alias =
            |source| validate_sources([("sample.rs", source), ("count.rs", "type Count = u64;")]);
        validate_alias(&aliased).unwrap();
        let missing_range = aliased.replace("JSON range:", "Comment:");
        assert!(
            validate_alias(&missing_range)
                .unwrap_err()
                .contains("Stamp.count: wide IPC integer")
        );
        let missing_override =
            aliased.replace("ts(type = \"number | null\")", "ts(rename = \"count\")");
        assert!(
            validate_alias(&missing_override)
                .unwrap_err()
                .contains("Stamp.count: wide IPC integer")
        );
        for mutation in [
            WITH_RANGE.replace("ts(type = \"number | null\")", "ts(rename = \"count\")"),
            WITH_RANGE.replace("JSON range:", "Comment:"),
            WITH_RANGE.replace("number | null", "number"),
        ] {
            let error = validate(&mutation).unwrap_err();
            assert!(error.contains("Stamp.count: wide IPC integer"), "{error}");
        }
    }
}
