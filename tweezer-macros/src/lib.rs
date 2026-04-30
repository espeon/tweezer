use proc_macro::TokenStream;
use quote::{quote, format_ident};
use syn::{parse_macro_input, Attribute, ItemFn, Expr, ExprLit, ExprArray, ExprTuple, Lit};

#[derive(Default)]
struct CommandAttrs {
    name: Option<String>,
    description: Option<String>,
    category: Option<String>,
    aliases: Vec<String>,
    channel: Option<String>,
    platform: Option<String>,
    cooldown_secs: Option<u64>,
    user_cooldown_secs: Option<u64>,
    rate_limit: Option<(u32, u32)>,
    user_rate_limit: Option<(u32, u32)>,
    rate_limit_strategy: Option<syn::Ident>,
}

#[proc_macro_attribute]
pub fn command(args: TokenStream, input: TokenStream) -> TokenStream {
    let func = parse_macro_input!(input as ItemFn);

    // Must be async fn
    if func.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            &func.sig.fn_token,
            "#[command] handlers must be async fn",
        )
        .to_compile_error()
        .into();
    }

    // Must take exactly one argument
    if func.sig.inputs.len() != 1 {
        return syn::Error::new_spanned(
            &func.sig,
            "#[command] handlers must take exactly one argument",
        )
        .to_compile_error()
        .into();
    }

    // Parse attribute args
    let attrs = match parse_attrs(args) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    let cmd_name = attrs.name.unwrap_or_else(|| func.sig.ident.to_string());
    let description = attrs.description.or_else(|| extract_doc_comments(&func.attrs));

    // Build builder method calls
    let mut builder_methods = Vec::new();

    if let Some(desc) = description {
        builder_methods.push(quote!(.description(#desc)));
    }
    if let Some(cat) = attrs.category {
        builder_methods.push(quote!(.category(#cat)));
    }
    for alias in attrs.aliases {
        builder_methods.push(quote!(.alias(#alias)));
    }
    if let Some(ch) = attrs.channel {
        builder_methods.push(quote!(.channel(#ch)));
    }
    if let Some(p) = attrs.platform {
        builder_methods.push(quote!(.platform(#p)));
    }
    if let Some(secs) = attrs.cooldown_secs {
        builder_methods.push(quote!(.cooldown(::std::time::Duration::from_secs(#secs))));
    }
    if let Some(secs) = attrs.user_cooldown_secs {
        builder_methods.push(quote!(.user_cooldown(::std::time::Duration::from_secs(#secs))));
    }
    if let Some((max, secs)) = attrs.rate_limit {
        builder_methods.push(quote!(.rate_limit(#max, ::std::time::Duration::from_secs(#secs))));
    }
    if let Some((max, secs)) = attrs.user_rate_limit {
        builder_methods.push(quote!(.user_rate_limit(#max, ::std::time::Duration::from_secs(#secs))));
    }
    if let Some(ref ident) = attrs.rate_limit_strategy {
        builder_methods.push(quote!(.rate_limit_strategy(::tweezer::RateLimitStrategy::#ident)));
    }

    // Preserve non-doc, non-command attributes on the handler
    let preserved_attrs: Vec<_> = func
        .attrs
        .iter()
        .filter(|attr| {
            let path = attr.path();
            !path.is_ident("doc") && !path.is_ident("command")
        })
        .collect();

    // Generate renamed handler with original body
    let handler_ident = format_ident!("__tweezer_cmd_{}", func.sig.ident);
    let mut handler_sig = func.sig.clone();
    handler_sig.ident = handler_ident.clone();
    let handler_vis = &func.vis;
    let handler_block = &func.block;

    // The wrapper function name
    let wrapper_ident = &func.sig.ident;

    let expanded = quote! {
        #(#preserved_attrs)*
        #[allow(non_snake_case)]
        #handler_vis #handler_sig {
            #handler_block
        }

        #handler_vis fn #wrapper_ident() -> impl ::tweezer::IntoCommand {
            ::tweezer::Command::new(#cmd_name, #handler_ident)
                #(#builder_methods)*
        }
    };

    expanded.into()
}

fn parse_attrs(args: TokenStream) -> Result<CommandAttrs, syn::Error> {
    if args.is_empty() {
        return Ok(CommandAttrs::default());
    }

    use syn::parse::Parser;
    let punctuated =
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated.parse(args)?;

    let mut attrs = CommandAttrs::default();

    for meta in punctuated {
        let nv = match meta {
            syn::Meta::NameValue(nv) => nv,
            _ => return Err(syn::Error::new_spanned(meta, "expected name = value")),
        };

        let key = nv.path.get_ident().ok_or_else(|| {
            syn::Error::new_spanned(&nv.path, "expected identifier key")
        })?;

        match key.to_string().as_str() {
            "name" => {
                attrs.name = Some(extract_str(&nv.value)?);
            }
            "description" => {
                attrs.description = Some(extract_str(&nv.value)?);
            }
            "category" => {
                attrs.category = Some(extract_str(&nv.value)?);
            }
            "aliases" => {
                attrs.aliases = extract_string_array(&nv.value)?;
            }
            "channel" => {
                attrs.channel = Some(extract_str(&nv.value)?);
            }
            "platform" => {
                attrs.platform = Some(extract_str(&nv.value)?);
            }
            "cooldown_secs" => {
                attrs.cooldown_secs = Some(extract_u64(&nv.value)?);
            }
            "user_cooldown_secs" => {
                attrs.user_cooldown_secs = Some(extract_u64(&nv.value)?);
            }
            "rate_limit" => {
                attrs.rate_limit = Some(extract_u32_tuple(&nv.value)?);
            }
            "user_rate_limit" => {
                attrs.user_rate_limit = Some(extract_u32_tuple(&nv.value)?);
            }
            "rate_limit_strategy" => {
                let expr = &nv.value;
                if let Expr::Path(path) = expr {
                    if let Some(ident) = path.path.get_ident() {
                        attrs.rate_limit_strategy = Some(ident.clone());
                    } else {
                        return Err(syn::Error::new_spanned(
                            expr,
                            "expected FixedWindow or LeakyBucket",
                        ));
                    }
                } else {
                    return Err(syn::Error::new_spanned(
                        expr,
                        "expected FixedWindow or LeakyBucket",
                    ));
                }
            }
            other => {
                return Err(syn::Error::new_spanned(
                    key,
                    format!("unknown attribute '{other}'"),
                ));
            }
        }
    }

    Ok(attrs)
}

fn extract_str(expr: &Expr) -> Result<String, syn::Error> {
    match expr {
        Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => Ok(s.value()),
        _ => Err(syn::Error::new_spanned(expr, "expected string literal")),
    }
}

fn extract_u64(expr: &Expr) -> Result<u64, syn::Error> {
    match expr {
        Expr::Lit(ExprLit { lit: Lit::Int(i), .. }) => i.base10_parse(),
        _ => Err(syn::Error::new_spanned(expr, "expected integer literal")),
    }
}

fn extract_u32_tuple(expr: &Expr) -> Result<(u32, u32), syn::Error> {
    match expr {
        Expr::Tuple(ExprTuple { elems, .. }) if elems.len() == 2 => {
            let first = match &elems[0] {
                Expr::Lit(ExprLit { lit: Lit::Int(i), .. }) => i.base10_parse::<u32>()?,
                _ => return Err(syn::Error::new_spanned(&elems[0], "expected u32")),
            };
            let second = match &elems[1] {
                Expr::Lit(ExprLit { lit: Lit::Int(i), .. }) => i.base10_parse::<u32>()?,
                _ => return Err(syn::Error::new_spanned(&elems[1], "expected u32")),
            };
            Ok((first, second))
        }
        _ => Err(syn::Error::new_spanned(
            expr,
            "expected (u32, u32) tuple",
        )),
    }
}

fn extract_string_array(expr: &Expr) -> Result<Vec<String>, syn::Error> {
    match expr {
        Expr::Array(ExprArray { elems, .. }) => {
            elems.iter().map(|e| extract_str(e)).collect()
        }
        _ => Err(syn::Error::new_spanned(
            expr,
            "expected array of string literals",
        )),
    }
}

fn extract_doc_comments(attrs: &[Attribute]) -> Option<String> {
    let mut parts = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let Ok(meta) = attr.meta.require_name_value() {
                if let Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) = &meta.value {
                    parts.push(s.value().trim().to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        let joined = parts.join(" ");
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}
