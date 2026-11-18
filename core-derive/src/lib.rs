//! Derives for the core's shared traits. Proc macros run on the build machine, inside the
//! compiler, and write Rust code that the compiler then compiles as if it had been typed.
//!
//! `#[derive(Code)]`: see the example on `derive_code`.

use proc_macro::TokenStream;

/// Writes `impl vee_core_api::event::Code` for an enum of event codes.
///
/// ```ignore
/// #[derive(Code)]
/// #[repr(u16)]
/// enum FsEv {
///     #[severity(Info)] Started,
///     #[severity(Err)] OpenFailed,
/// }
/// ```
///
/// - `#[repr(u16)]`: the value of a variant is the event's code. Values not written count up
///   from 0, as for any enum, and Rust refuses two variants with the same value, so codes are
///   unique. A variant may be given a value (`Started = 0x01`) to pin its code. Without one, a
///   code is the variant's position, so reordering or inserting variants renumbers the codes that
///   follow.
/// - `#[severity(Level)]` on every variant. The level is `Info`, `Warn` or `Err`.
/// - Variants have no fields: the event's details go in its text, not in the code.
///
/// What it generates:
/// - `impl event::Code`: `code`, `severity`, `index` and `from_index`. Each is one `match` with
///   an arm per variant (`code` casts the variant with `as u16`).
/// - A check, made when the code is compiled, that the enum has no more variants than
///   `throttle::EV_MAX_CODES`, the number of counters in a `Throttle`.
///
/// The generated code names the core-api crate as `::vee_core_api`, so the crate using the
/// derive must depend on it.
#[proc_macro_derive(Code, attributes(severity))]
pub fn derive_code(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    match expand_code(&input) {
        Ok(tokens) => tokens.into(),
        // A `syn::Error` becomes a `compile_error!` that points at the code that caused it.
        Err(error) => error.into_compile_error().into(),
    }
}

/// One event code (one variant of the enum), after reading and checking its source.
struct Variant<'a> {
    name: &'a syn::Ident,
    /// `Info`, `Warn` or `Err`.
    severity: syn::Ident,
}

fn expand_code(input: &syn::DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let syn::Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "Code can only be derived for an enum",
        ));
    };
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "Code does not support generics",
        ));
    }
    check_repr_u16(input)?;

    let variants = data
        .variants
        .iter()
        .map(parse_variant)
        .collect::<syn::Result<Vec<_>>>()?;

    // One piece of generated code per variant, for each method. They are pasted into the `impl`
    // below with `#(...)*`.
    let code_arms = variants.iter().map(|variant| {
        let name = variant.name;
        quote::quote! { Self::#name => Self::#name as u16, }
    });
    let severity_arms = variants.iter().map(|variant| {
        let (name, severity) = (variant.name, &variant.severity);
        quote::quote! { Self::#name => ::vee_core_api::event::Severity::#severity, }
    });

    // A variant's position, in the order written. Not the code, which can be any value.
    let index_arms = variants.iter().enumerate().map(|(position, variant)| {
        let name = variant.name;
        quote::quote! { Self::#name => #position, }
    });
    let from_index_arms = variants.iter().enumerate().map(|(position, variant)| {
        let name = variant.name;
        quote::quote! { #position => ::core::option::Option::Some(Self::#name), }
    });

    let name = &input.ident;
    let variant_count = variants.len();
    let too_many_message = format!(
        "{name} has more events than throttle::EV_MAX_CODES: raise VEE_EV_MAX_CODES, or split it"
    );
    Ok(quote::quote! {
        // Every source's `Throttle` has `EV_MAX_CODES` counters, one for each event by `index`.
        const _: () = assert!(
            #variant_count <= ::vee_core_api::throttle::EV_MAX_CODES,
            #too_many_message
        );

        impl ::vee_core_api::event::Code for #name {
            fn code(&self) -> u16 {
                match self { #(#code_arms)* }
            }

            fn severity(&self) -> ::vee_core_api::event::Severity {
                match self { #(#severity_arms)* }
            }

            fn index(&self) -> usize {
                match self { #(#index_arms)* }
            }

            fn from_index(index: usize) -> ::core::option::Option<Self> {
                match index {
                    #(#from_index_arms)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    })
}

/// The codes are `u16`, so the enum must say it is one.
fn check_repr_u16(input: &syn::DeriveInput) -> syn::Result<()> {
    let has_repr_u16 = input.attrs.iter().any(|attr| {
        attr.path().is_ident("repr")
            && attr
                .parse_args::<syn::Ident>()
                .is_ok_and(|repr| repr == "u16")
    });
    if has_repr_u16 {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            &input.ident,
            "add `#[repr(u16)]`: the value of each variant is its event code",
        ))
    }
}

fn parse_variant(variant: &syn::Variant) -> syn::Result<Variant<'_>> {
    if !matches!(variant.fields, syn::Fields::Unit) {
        return Err(syn::Error::new_spanned(
            &variant.fields,
            "a code has no fields: put the details in the event's text",
        ));
    }

    let mut severity_attrs = variant
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("severity"));
    let attr = severity_attrs.next().ok_or_else(|| {
        syn::Error::new_spanned(
            &variant.ident,
            "add `#[severity(Info)]` (the severity is Info, Warn or Err)",
        )
    })?;
    if let Some(extra) = severity_attrs.next() {
        return Err(syn::Error::new_spanned(
            extra,
            "a code has only one `#[severity(..)]`",
        ));
    }
    let severity: syn::Ident = attr.parse_args()?;
    if !["Info", "Warn", "Err"].iter().any(|name| severity == name) {
        return Err(syn::Error::new_spanned(
            &severity,
            "the severity must be Info, Warn or Err",
        ));
    }

    Ok(Variant {
        name: &variant.ident,
        severity,
    })
}
