use proc_macro::TokenStream;
use quote::quote;
use std::collections::{HashMap, HashSet};
use syn::{Expr, LitStr, Token, parse::Parse, parse::ParseStream, parse_macro_input};

struct StyledFormatInput {
  fmt       : LitStr,
  positional: Vec<Expr>,
  named     : Vec<(syn::Ident, Expr)>,
}

impl Parse for StyledFormatInput {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let     fmt: LitStr = input.parse()?;
    let mut positional  = Vec::new();
    let mut named       = Vec::new();
    let mut saw_named   = false;

    while !input.is_empty() {
      input.parse::<Token![,]>()?;
      if input.is_empty() {
        break; // trailing comma
      }

      let fork = input.fork();
      if fork.parse::<syn::Ident>().is_ok() && fork.peek(Token![=]) && !fork.peek2(Token![=]) {
        let name: syn::Ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let value: Expr = input.parse()?;
        named.push((name, value));
        saw_named = true;
      } else {
        if saw_named {
          return Err(input.error("positional arguments must come before named ones"));
        }
        positional.push(input.parse()?);
      }
    }

    Ok(Self {
      fmt,
      positional,
      named,
    })
  }
}

pub fn styled_format(input: TokenStream) -> TokenStream {
  let parsed        = parse_macro_input!(input as StyledFormatInput);
  let fmt_lit       = parsed.fmt.clone();
  let fmt_str       = parsed.fmt.value();

  let scan_result   = scan_refs(&fmt_str);
  let ordered_names = scan_result.named;
  let positional_display: Vec<bool>     = scan_result.positional_display;
  let seen              : HashSet<&str> = ordered_names.iter().map(String::as_str).collect();

  // Map user-provided named args by name for value lookup.
  let user_named: HashMap<String, Expr> = parsed
    .named
    .iter()
    .map(|(i, e)| (i.to_string(), e.clone()))
    .collect();

  let     span               = parsed.fmt.span();
  let mut paint_count: usize = 0;
  let color_call = |i: usize| -> proc_macro2::TokenStream {
    if i == 0 {
      quote! { crate::util::error::next_color() }
    } else {
      quote! { crate::util::error::last_color() }
    }
  };

  let mut positional_tokens: Vec<proc_macro2::TokenStream> = Vec::new();
  for (i, expr) in parsed.positional.iter().enumerate() {
    let display_safe = positional_display.get(i).copied().unwrap_or(false);
    if display_safe {
      let cc = color_call(paint_count);
      paint_count += 1;
      positional_tokens.push(quote! {
        ::ariadne::Fmt::fg((#expr), #cc)
      });
    } else {
      positional_tokens.push(quote! { #expr });
    }
  }

  let mut named_tokens: Vec<proc_macro2::TokenStream> = Vec::new();
  for name in &ordered_names {
    let ident = syn::Ident::new(name, span);
    let cc    = color_call(paint_count);
    paint_count += 1;
    let value: proc_macro2::TokenStream = if let Some(expr) = user_named.get(name) {
      quote! { (#expr) }
    } else {
      quote! { #ident }
    };
    named_tokens.push(quote! {
      #ident = ::ariadne::Fmt::fg(#value, #cc)
    });
  }

  for (name, expr) in &parsed.named {
    if !seen.contains(name.to_string().as_str()) {
      named_tokens.push(quote! { #name = #expr });
    }
  }

  let expanded = quote! {
    format!(#fmt_lit, #(#positional_tokens,)* #(#named_tokens),*)
  };

  expanded.into()
}

struct ScanResult {
  named: Vec<String>,
  positional_display: Vec<bool>,
}
fn scan_refs(fmt_str: &str) -> ScanResult {
  let mut named             : Vec<String>     = Vec::new();
  let mut seen              : HashSet<String> = HashSet::new();
  let mut positional_display: Vec<bool>       = Vec::new();
  let mut chars = fmt_str.chars().peekable();

  while let Some(c) = chars.next() {
    match c {
      '{' => {
        if chars.peek() == Some(&'{') {
          chars.next();
          continue;
        }
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
          if c == '}' || c == ':' {
            break;
          }
          name.push(c);
          chars.next();
        }
        let mut spec = String::new();
        if chars.peek() == Some(&':') {
          chars.next();
          while let Some(&c) = chars.peek() {
            if c == '}' {
              break;
            }
            spec.push(c);
            chars.next();
          }
        }
        chars.next(); // consume `}`

        if name.is_empty() {
          positional_display.push(is_display_spec(&spec));
        } else if name.chars().all(|c| c.is_ascii_digit()) {
        } else if !seen.contains(&name) {
          seen.insert(name.clone());
          named.push(name);
        }
      }
      '}' if chars.peek() == Some(&'}') => {
        chars.next();
      }
      _ => {}
    }
  }

  ScanResult {
    named,
    positional_display,
  }
}

fn is_display_spec(spec: &str) -> bool {
  if spec.is_empty() {
    return true;
  }
  let last = spec.chars().last();
  !matches!(last, Some('?' | 'x' | 'X' | 'o' | 'b' | 'e' | 'E'))
}
