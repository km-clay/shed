//! Logic for the `ShOptGroup` derive macro

use proc_macro::TokenStream;
use proc_macro2::{Ident, TokenStream as TokenStream2};
use quote::quote;
use syn::{Attribute, DeriveInput, Expr, Meta, Type, parse_macro_input};

pub(super) fn derive_shopt_group(input: TokenStream) -> TokenStream {
  ShOptDerive::derive(input)
}

struct DeriveParts {
  names: Vec<Ident>,
  types: Vec<Type>,
  defaults: Vec<Expr>,
  docs: Vec<String>,
  validators: Vec<Option<Expr>>,
}

/// A full expansion of the `ShOptGroup` derive macro
struct ShOptDerive {
  default_impl: TokenStream2,
  name: Ident,
  set_arms: Vec<TokenStream2>,
  group: String,
  rc_entries_default: Vec<TokenStream2>,
  rc_entries_current: Vec<TokenStream2>,
  get_arms: Vec<TokenStream2>,
  display_entries: Vec<TokenStream2>,
}

impl ShOptDerive {
  pub fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident.clone();
    let group = Self::extract_group_name(&input.attrs);
    let DeriveParts {
      names,
      types,
      defaults,
      docs,
      validators,
    } = Self::derive_parts(&input);

    let params = Self {
      default_impl: Self::derive_default_impl(&name, &names, &defaults),
      set_arms: Self::derive_set_arms(&names, &types, &validators, &group),
      get_arms: Self::derive_get_arms(&names, &docs),
      rc_entries_default: Self::derive_rc_entries_default(&names, &docs, &group),
      rc_entries_current: Self::derive_rc_entries_current(&names, &docs, &group),
      display_entries: Self::derive_display_entries(&names, &group),
      name,
      group,
    };
    params.expand()
  }
  fn derive_parts(input: &DeriveInput) -> DeriveParts {
    // Extract the fields from the struct
    // Each field name represents a shopt
    let named_fields = match &input.data {
      syn::Data::Struct(s) => match &s.fields {
        syn::Fields::Named(f) => f.named.iter().collect::<Vec<_>>(),
        _ => panic!("ShOptGroup can only be derived for structs with named fields"),
      },
      _ => panic!("ShOptGroup can only be derived for structs"),
    };

    // field names
    let names: Vec<_> = named_fields
      .iter()
      .map(|f| f.ident.clone().unwrap())
      .collect();
    // shopts are strongly typed
    let types = named_fields
      .iter()
      .map(|f| f.ty.clone())
      .collect::<Vec<_>>();
    // #[default(...)] attributes
    let defaults = Self::extract_defaults(&named_fields);
    // #[validate(...)] attributes
    let validators = Self::extract_validators(&named_fields);
    // doc comments
    let docs = Self::extract_docs(&named_fields);

    DeriveParts {
      names,
      types,
      defaults,
      docs,
      validators,
    }
  }
  fn extract_docs(fields: &[&syn::Field]) -> Vec<String> {
    fields.iter().map(|f| Self::extract_doc(&f.attrs)).collect()
  }
  fn extract_doc(attrs: &[Attribute]) -> String {
    // look for doc comments (`///`) above the field
    // these are used for the shopt description in the generated default rc file
    let parts: Vec<String> = attrs
      .iter()
      .filter_map(|a| {
        if !a.path().is_ident("doc") {
          return None;
        }
        if let Meta::NameValue(nv) = &a.meta
          && let Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
          }) = &nv.value
        {
          return Some(s.value());
        }
        None
      })
      .collect();

    parts.join(" ")
  }

  fn extract_defaults(fields: &[&syn::Field]) -> Vec<Expr> {
    fields
      .iter()
      .map(|f| {
        Self::extract_default(&f.attrs).unwrap_or_else(|| {
          panic!(
            "field `{}` needs #[default(...)]",
            f.ident.as_ref().unwrap()
          )
        })
      })
      .collect()
  }
  fn extract_default(attrs: &[Attribute]) -> Option<Expr> {
    // look for #[default(...)] attribute and parse the expression inside
    attrs
      .iter()
      .find(|a| a.path().is_ident("default"))
      .and_then(|a| a.parse_args::<Expr>().ok())
  }

  fn extract_validators(fields: &[&syn::Field]) -> Vec<Option<Expr>> {
    fields
      .iter()
      .map(|f| Self::extract_validate(&f.attrs))
      .collect()
  }
  fn extract_validate(attrs: &[Attribute]) -> Option<Expr> {
    // look for #[validate(...)] attribute and parse the expression inside
    attrs
      .iter()
      .find(|a| a.path().is_ident("validate"))
      .and_then(|a| a.parse_args::<Expr>().ok())
  }

  // NOTE: double check this later
  fn extract_group_name(attrs: &[Attribute]) -> String {
    // look for #[group_name("...")] attribute and parse the string inside
    // this attribute goes above the struct definition, not the fields
    for a in attrs {
      if !a.path().is_ident("group_name") {
        continue;
      }
      if let Meta::NameValue(nv) = &a.meta
        && let Expr::Lit(syn::ExprLit {
          lit: syn::Lit::Str(s),
          ..
        }) = &nv.value
      {
        return s.value();
      }
    }
    panic!("group_name attribute is required for ShOptGroup")
  }

  fn derive_default_impl(name: &Ident, idents: &[Ident], defaults: &[Expr]) -> TokenStream2 {
    // create the Default impl for the struct, using the #[default(...)] attributes for each field
    // each field is initialized with the expression from its #[default(...)] attribute
    quote! {
      impl Default for #name {
        fn default() -> Self {
          Self { #( #idents: #defaults, )* }
        }
      }
    }
  }
  fn derive_set_arms(
    idents: &[Ident],
    types: &[Type],
    validators: &[Option<Expr>],
    group: &str,
  ) -> Vec<TokenStream2> {
    // this creates the match arms for the `set` method, which sets a shopt value based on its name and string value
    // the `shopt` builtin hits these when it sets a new value
    // the idents, types, and validators are all parallel arrays, so we can zip them together to create the match arms
    idents
      .iter()
      .zip(types.iter())
      .zip(validators.iter())
      .map(|((ident, ty), validator)| {
        let s = ident.to_string();
        let validate = validator.as_ref().map(|v| quote! {
          let validate: fn(&#ty) -> Result<(), String> = #v;
          // validate the parsed value, and if it fails, set the status to 2 and return an error
          if let Err(e) = validate(&parsed).map_err(|msg| crate::sherr!(SyntaxErr, "shopt: {msg}")) {
            crate::state::Shed::set_status(2);
            return Err(e);
          }
        }).unwrap_or_default();

        // make the match arm
        quote! {
          #s => {
            // run the type's FromStr implementation to parse the string value into the correct type
            let parsed = val.parse::<#ty>().map_err(|_| crate::sherr!(
                SyntaxErr, "shopt: invalid value '{}' for {}.{}", val, #group, opt,
            ))?;
            #validate // run the validator (created above), if there is one
            self.#ident = parsed;
            Ok(())
          }
        }
      })
    .collect()
  }
  fn derive_get_arms(idents: &[Ident], docs: &[String]) -> Vec<TokenStream2> {
    // this creates the match arms for the `get` method, which gets a shopt value based on its name
    idents
      .iter()
      .zip(docs.iter())
      .map(|(ident, doc)| {
        let s = ident.to_string();
        quote! {
          #s => Ok(Some(format!("{}\n{}", #doc, self.#ident))),
        }
      })
      .collect()
  }
  fn derive_display_entries(idents: &[Ident], group: &str) -> Vec<TokenStream2> {
    // this creates the entries for the Display impl, which is used to print the shopt values
    // this is used for the shopt builtin's default output, and is meant to be source-able shell syntax
    // so we run it through shell_quote
    idents
      .iter()
      .map(|ident| {
        let s = ident.to_string();
        quote! {
          format!("{}.{}={}", #group, #s, crate::expand::escape::shell_quote(&self.#ident.to_string()))
        }
      })
    .collect()
  }
  fn derive_rc_entries_default(
    idents: &[Ident],
    docs: &[String],
    group: &str,
  ) -> Vec<TokenStream2> {
    // this creates the default rc file entries. the doc comments are used to
    // generate comments next to the shopt, and the default values are interpolated
    // on the right hand side of the assignment
    idents
      .iter()
      .zip(docs.iter())
      .map(|(ident, doc)| {
        let s = ident.to_string();
        quote! {
          {
            let val = crate::expand::escape::shell_quote(&defaults.#ident.to_string());
            let entry = format!("shopt {}.{}={}", #group, #s, val);
            let doc: Option<String> = if #doc.is_empty() {
              None
            } else {
              Some(#doc.trim().to_string())
            };
            entries.push((format!("{}.{}", #group, #s), entry, doc));
          }
        }
      })
      .collect()
  }
  fn derive_rc_entries_current(
    idents: &[Ident],
    docs: &[String],
    group: &str,
  ) -> Vec<TokenStream2> {
    // the same thing as the previous function, but this one uses the current values of the struct rather than the defaults
    // used by the `genrc` builtin for the shopt section
    idents
      .iter()
      .zip(docs.iter())
      .map(|(ident, doc)| {
        let s = ident.to_string();
        quote! {
          {
            let val = crate::expand::escape::shell_quote(&self.#ident.to_string());
            let entry = format!("shopt {}.{}={}", #group, #s, val);
            let doc: Option<String> = if #doc.is_empty() {
              None
            } else {
              Some(#doc.trim().to_string())
            };
            entries.push((format!("{}.{}", #group, #s), entry, doc));
          }
        }
      })
      .collect()
  }
  fn expand(self) -> TokenStream {
    let Self {
      default_impl,
      name,
      set_arms,
      group,
      rc_entries_default,
      rc_entries_current,
      get_arms,
      display_entries,
    } = self;

    quote! {
      #default_impl

      impl #name {
        pub fn set(&mut self, opt: &str, val: &str) -> crate::util::error::ShResult<()> {
          match opt {
            #( #set_arms )*
            _ => Err(crate::sherr!(SyntaxErr, "shopt: unexpected '{}' option '{opt}'", #group))
          }
        }

        pub fn get(&self, query: &str) -> crate::util::error::ShResult<Option<String>> {
          if query.is_empty() { return Ok(Some(format!("{self}"))); }
          match query {
            #( #get_arms )*
            _ => Err(crate::sherr!(SyntaxErr, "shopt: unexpected '{}' option '{query}'", #group))
          }
        }

        /// Rc entries built from `Self::default()`. Each tuple is
        /// `(fully-qualified key, "shopt key=val" line, optional doc string)`.
        /// Consumers decide whether to render the doc as a trailing comment.
        pub fn rc_entries_default() -> Vec<(String, String, Option<String>)> {
          let defaults = Self::default();
          let mut entries: Vec<(String, String, Option<String>)> = vec![];
          #( #rc_entries_default )*
          entries
        }

        /// Rc entries built from the live values of `self`. Used when
        /// regenerating the rc file to capture the user's current config
        /// rather than factory defaults.
        pub fn rc_entries_current(&self) -> Vec<(String, String, Option<String>)> {
          let mut entries: Vec<(String, String, Option<String>)> = vec![];
          #( #rc_entries_current )*
          entries
        }
      }

      impl ::std::fmt::Display for #name {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
          let output = [ #( #display_entries ),* ];
          writeln!(f, "{}", output.join("\n"))
        }
      }
    }
    .into()
  }
}
