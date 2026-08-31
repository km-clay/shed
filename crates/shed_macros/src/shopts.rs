use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, DeriveInput, Expr, Meta, parse_macro_input};

fn extract_doc(attrs: &[Attribute]) -> String {
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

fn extract_default(attrs: &[Attribute]) -> Option<Expr> {
  attrs
    .iter()
    .find(|a| a.path().is_ident("default"))
    .and_then(|a| a.parse_args::<Expr>().ok())
}

fn extract_validate(attrs: &[Attribute]) -> Option<Expr> {
  attrs
    .iter()
    .find(|a| a.path().is_ident("validate"))
    .and_then(|a| a.parse_args::<Expr>().ok())
}

// NOTE: double check this later
fn extract_group_name(attrs: &[Attribute]) -> String {
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

#[expect(clippy::too_many_lines)]
pub fn derive_shopt_group(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  let name = &input.ident;
  let group = extract_group_name(&input.attrs);

  let named_fields = match &input.data {
    syn::Data::Struct(s) => match &s.fields {
      syn::Fields::Named(f) => f.named.iter().collect::<Vec<_>>(),
      _ => panic!("ShOptGroup can only be derived for structs with named fields"),
    },
    _ => panic!("ShOptGroup can only be derived for structs"),
  };

  let idents: Vec<_> = named_fields
    .iter()
    .map(|f| f.ident.as_ref().unwrap())
    .collect();
  let types = named_fields.iter().map(|f| &f.ty).collect::<Vec<_>>();
  let defaults: Vec<Expr> = named_fields
    .iter()
    .map(|f| {
      extract_default(&f.attrs).unwrap_or_else(|| {
        panic!(
          "field `{}` needs #[default(...)]",
          f.ident.as_ref().unwrap()
        )
      })
    })
    .collect();
  let docs: Vec<String> = named_fields.iter().map(|f| extract_doc(&f.attrs)).collect();
  let validators: Vec<Option<Expr>> = named_fields
    .iter()
    .map(|f| extract_validate(&f.attrs))
    .collect();

  let default_impl = quote! {
    impl Default for #name {
      fn default() -> Self {
        Self { #( #idents: #defaults, )* }
      }
    }
  };

  let set_arms: Vec<TokenStream2> = idents
    .iter()
    .zip(types.iter())
    .zip(validators.iter())
    .map(|((ident, ty), validator)| {
      let s = ident.to_string();
      let validate = validator.as_ref().map(|v| quote! {
        let validate: fn(&#ty) -> Result<(), String> = #v;
        if let Err(e) = validate(&parsed).map_err(|msg| crate::sherr!(SyntaxErr, "shopt: {msg}")) {
          crate::state::Shed::set_status(2);
          return Err(e);
        }
      }).unwrap_or_default();

      quote! {
        #s => {
          let parsed = val.parse::<#ty>().map_err(|_| crate::sherr!(
            SyntaxErr, "shopt: invalid value '{}' for {}.{}", val, #group, opt,
          ))?;
          #validate
          self.#ident = parsed;
          Ok(())
        }
      }
    })
    .collect();

  let get_arms: Vec<TokenStream2> = idents
    .iter()
    .zip(docs.iter())
    .map(|(ident, doc)| {
      let s = ident.to_string();
      quote! {
        #s => Ok(Some(format!("{}\n{}", #doc, self.#ident))),
      }
    })
    .collect();

  let rc_entries_default: Vec<TokenStream2> = idents
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
    .collect();

  let rc_entries_current: Vec<TokenStream2> = idents
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
    .collect();

  let display_entries: Vec<TokenStream2> = idents
    .iter()
    .map(|ident| {
      let s = ident.to_string();
      quote! {
        format!("{}.{}={}", #group, #s, crate::expand::escape::shell_quote(&self.#ident.to_string()))
      }
    })
    .collect();

  let expanded = quote! {
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
  };

  expanded.into()
}
