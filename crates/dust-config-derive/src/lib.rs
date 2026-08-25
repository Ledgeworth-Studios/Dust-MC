//! The derive behind Dust's configuration reference.
//!
//! `#[derive(ConfigSection)]` generates the description of a configuration
//! section — its fields, their types, their defaults and whether changing one
//! takes effect at runtime — so that `docs/configuration.md` is produced from
//! the types rather than maintained beside them.
//!
//! The load-bearing part is not what it generates, it is what it refuses to
//! generate. **A field with no doc comment is a compile error.** That is the
//! Phase 0.3 exit criterion expressed as a type rule: an undocumented setting
//! cannot reach `main`, because it cannot reach a successful `cargo build`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, Data, DeriveInput, Expr, Fields, GenericArgument, Lit, Meta, PathArguments,
    Type,
};

/// Derives `ConfigSection`.
///
/// Field attributes:
/// - `#[config(section)]` — the field is itself a section; recurse into it.
/// - `#[config(map)]` — the field is a map of operator-chosen keys; the value
///   type is documented once as a template.
/// - `#[config(restart)]` — changing this field requires a restart.
/// - `#[config(new_chunks)]` — the change applies to chunks generated after it,
///   and leaves chunks already on disk alone.
#[proc_macro_derive(ConfigSection, attributes(config))]
pub fn derive_config_section(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let section_doc = doc_of(&input.attrs);
    if section_doc.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            format!(
                "configuration section `{ident}` has no doc comment.\n\
                 Every section appears in docs/configuration.md, and a section \
                 with nothing to say about itself is a section nobody can \
                 configure. Add a `///` comment describing what it controls."
            ),
        ));
    }

    let key_label = key_label_of(&input.attrs)?
        .map(|l| quote! { const MAP_KEY_LABEL: &'static str = #l; })
        .unwrap_or_default();

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "ConfigSection may only be derived for structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "ConfigSection requires named fields",
        ));
    };

    let mut field_docs = Vec::new();
    let mut subsections = Vec::new();

    for field in &fields.named {
        let name = field.ident.as_ref().expect("named field");
        let opts = FieldOpts::parse(&field.attrs)?;
        let doc = doc_of(&field.attrs);

        if doc.is_empty() {
            return Err(syn::Error::new_spanned(
                name,
                format!(
                    "configuration field `{ident}::{name}` has no doc comment.\n\
                     Every setting is documented in docs/configuration.md, and \
                     that reference is generated from these comments — so an \
                     undocumented setting fails the build rather than shipping \
                     undocumented. Add a `///` comment saying what it does and \
                     what the value means."
                ),
            ));
        }

        if opts.section {
            let ty = &field.ty;
            subsections.push(quote! {
                {
                    let mut __s = <#ty as ::dust_config::ConfigSection>::describe();
                    __s.key = stringify!(#name);
                    __s.doc = #doc;
                    __s
                }
            });
            continue;
        }

        if opts.map {
            let value_ty = map_value_type(&field.ty)?;
            subsections.push(quote! {
                {
                    let mut __s = <#value_ty as ::dust_config::ConfigSection>::describe();
                    __s.key = stringify!(#name);
                    __s.doc = #doc;
                    __s.keyed_by = ::core::option::Option::Some(
                        <#value_ty as ::dust_config::ConfigSection>::MAP_KEY_LABEL,
                    );
                    __s
                }
            });
            continue;
        }

        let reload = opts.reload_expr();
        let ty_name = type_name(&field.ty);
        field_docs.push(quote! {
            ::dust_config::FieldDoc {
                name: stringify!(#name),
                doc: #doc,
                ty: #ty_name,
                reload: #reload,
                default: ::dust_config::ConfigValue::render_default(&__defaults.#name),
            }
        });
    }

    Ok(quote! {
        impl ::dust_config::ConfigSection for #ident {
            #key_label

            fn describe() -> ::dust_config::SectionDoc {
                let __defaults = <Self as ::core::default::Default>::default();
                ::dust_config::SectionDoc {
                    key: "",
                    doc: #section_doc,
                    keyed_by: ::core::option::Option::None,
                    fields: ::std::vec![ #(#field_docs),* ],
                    subsections: ::std::vec![ #(#subsections),* ],
                }
            }
        }
    })
}

/// Reads `#[config(key_label = "...")]` from the struct, which names what an
/// operator-chosen key means when this section is used as a map value.
fn key_label_of(attrs: &[syn::Attribute]) -> syn::Result<Option<String>> {
    let mut label = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("config")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("key_label") {
                let value: syn::LitStr = meta.value()?.parse()?;
                label = Some(value.value());
                Ok(())
            } else {
                Err(meta.error("unknown `config` option on a struct"))
            }
        })?;
    }
    Ok(label)
}

#[derive(Default)]
struct FieldOpts {
    section: bool,
    map: bool,
    restart: bool,
    new_chunks: bool,
}

impl FieldOpts {
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let mut opts = Self::default();
        for attr in attrs.iter().filter(|a| a.path().is_ident("config")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("section") {
                    opts.section = true;
                } else if meta.path.is_ident("map") {
                    opts.map = true;
                } else if meta.path.is_ident("restart") {
                    opts.restart = true;
                } else if meta.path.is_ident("new_chunks") {
                    opts.new_chunks = true;
                } else {
                    return Err(meta.error("unknown `config` option"));
                }
                Ok(())
            })?;
        }
        if opts.restart && opts.new_chunks {
            return Err(syn::Error::new_spanned(
                attrs.first().expect("attribute present"),
                "`restart` and `new_chunks` describe different reload behaviours; pick one",
            ));
        }
        Ok(opts)
    }

    fn reload_expr(&self) -> TokenStream2 {
        if self.restart {
            quote!(::dust_config::Reload::Restart)
        } else if self.new_chunks {
            quote!(::dust_config::Reload::HotNewChunksOnly)
        } else {
            quote!(::dust_config::Reload::Hot)
        }
    }
}

fn doc_of(attrs: &[syn::Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("doc")) {
        if let Meta::NameValue(nv) = &attr.meta {
            if let Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                    lines.push(s.value().trim().to_owned());
                }
            }
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n").trim().to_owned()
}

/// `BTreeMap<K, V>` and `HashMap<K, V>` describe themselves through `V`.
fn map_value_type(ty: &Type) -> syn::Result<&Type> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "`#[config(map)]` needs a map type",
        ));
    };
    let segment = path
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(ty, "empty type path"))?;
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "`#[config(map)]` needs a map type with two type parameters, e.g. BTreeMap<K, V>",
        ));
    };
    args.args
        .iter()
        .filter_map(|a| match a {
            GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .nth(1)
        .ok_or_else(|| {
            syn::Error::new_spanned(ty, "`#[config(map)]` could not find the map's value type")
        })
}

fn type_name(ty: &Type) -> String {
    quote!(#ty).to_string().replace(' ', "")
}
