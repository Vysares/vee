//! `#[derive(Command)]`: see the example on `crate::derive_command`.

/// One field, after reading and checking its source: its name, its exact wire type, and where it
/// falls in the wire layout.
struct CommandField<'a> {
    name: &'a syn::Ident,
    ty: &'a syn::Type,
    /// `true` for `bool`, which has no `to_be_bytes`/`from_be_bytes` of its own.
    is_bool: bool,
    start: usize,
    end: usize,
}

pub fn expand(input: &syn::DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let syn::Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "Command can only be derived for a struct",
        ));
    };
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "Command does not support generics",
        ));
    }
    let syn::Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "Command needs named fields",
        ));
    };

    let has_validate = has_validate_attr(input)?;

    let mut offset = 0usize;
    let mut parsed = Vec::new();
    for field in &fields.named {
        // `Fields::Named` guarantees every field has a name.
        let Some(name) = field.ident.as_ref() else {
            continue;
        };
        let (is_bool, width) = field_width(&field.ty)?;
        let end = offset.checked_add(width).ok_or_else(|| {
            syn::Error::new_spanned(field, "command is too large: its wire layout overflows")
        })?;
        parsed.push(CommandField {
            name,
            ty: &field.ty,
            is_bool,
            start: offset,
            end,
        });
        offset = end;
    }
    let wire_len = offset;

    let encode_stmts = parsed.iter().map(|field| {
        let name = field.name;
        let (start, end) = (field.start, field.end);
        if field.is_bool {
            quote::quote! { out[#start] = u8::from(self.#name); }
        } else {
            quote::quote! { out[#start..#end].copy_from_slice(&self.#name.to_be_bytes()); }
        }
    });

    let decode_stmts = parsed.iter().map(|field| {
        let name = field.name;
        let start = field.start;
        if field.is_bool {
            quote::quote! {
                let #name = (*bytes.get(#start).ok_or(::vee_core_api::cmd::CmdErr::BadArgs)?) != 0;
            }
        } else {
            let ty = field.ty;
            quote::quote! {
                let #name = <#ty>::from_be_bytes(
                    ::vee_core_api::cmd::chunk(bytes, #start)
                        .ok_or(::vee_core_api::cmd::CmdErr::BadArgs)?
                );
            }
        }
    });
    let field_names = parsed.iter().map(|field| field.name);

    let validate_default = if has_validate {
        quote::quote! {}
    } else {
        quote::quote! {
            /// Always succeeds. Generated because the struct has no `#[validate]`: see
            /// `docs/design.md`'s "hook for validate".
            pub fn validate(&self) -> ::core::result::Result<(), ::vee_core_api::cmd::CmdErr> {
                ::core::result::Result::Ok(())
            }
        }
    };

    let struct_name = &input.ident;
    let too_large_message = format!(
        "{struct_name} is more bytes than cmd::CMD_ARGS_MAX_BYTES: raise it, or shrink the command"
    );
    Ok(quote::quote! {
        impl #struct_name {
            /// The size of this command's wire encoding: see `encode`.
            pub const WIRE_LEN: usize = #wire_len;

            /// Writes this value's wire bytes: big endian, declaration order, `Self::WIRE_LEN`
            /// bytes in all. To send it, turn this into `cmd::Args` the same way any other bytes
            /// are: `cmd::Args::new(&value.encode())`.
            pub fn encode(&self) -> [u8; #wire_len] {
                let mut out = [0u8; #wire_len];
                #(#encode_stmts)*
                out
            }

            /// Reads a value back out of its wire bytes, or `BadArgs` if `bytes` is shorter than
            /// `Self::WIRE_LEN`.
            pub fn decode(bytes: &[u8]) -> ::core::result::Result<Self, ::vee_core_api::cmd::CmdErr> {
                #(#decode_stmts)*
                ::core::result::Result::Ok(Self { #(#field_names,)* })
            }

            #validate_default
        }

        // `encode`'s output must always fit in `cmd::Args`, which a target's command queue stores
        // a decoded command's args in (`cmd::CMD_ARGS_MAX_BYTES`). Checked here, at compile time,
        // so a command too big to ever fit is a build error, not something discovered in flight.
        const _: () = assert!(
            #wire_len <= ::vee_core_api::cmd::CMD_ARGS_MAX_BYTES,
            #too_large_message
        );
    })
}

/// Whether `input`'s struct has `#[validate]` (no arguments): if so, the struct provides its own
/// `validate` by hand, and the derive generates none.
fn has_validate_attr(input: &syn::DeriveInput) -> syn::Result<bool> {
    let mut validate_attrs = input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("validate"));
    let Some(attr) = validate_attrs.next() else {
        return Ok(false);
    };
    if let Some(extra) = validate_attrs.next() {
        return Err(syn::Error::new_spanned(
            extra,
            "a command has only one `#[validate]`",
        ));
    }
    if !matches!(attr.meta, syn::Meta::Path(_)) {
        return Err(syn::Error::new_spanned(
            attr,
            "`#[validate]` takes no arguments",
        ));
    }
    Ok(true)
}

/// Whether a field's Rust type is `bool`, and its wire width, or an error if the type is not one
/// `Command` supports yet.
fn field_width(ty: &syn::Type) -> syn::Result<(bool, usize)> {
    let not_supported = || {
        syn::Error::new_spanned(
            ty,
            "Command fields must be bool or a fixed-width number (u8..u64, i8..i64, f32, f64): \
             nested structs, arrays and text are not supported yet",
        )
    };
    let syn::Type::Path(path) = ty else {
        return Err(not_supported());
    };
    let ident = path.path.get_ident().ok_or_else(not_supported)?;
    match ident.to_string().as_str() {
        "bool" => Ok((true, 1)),
        "u8" | "i8" => Ok((false, 1)),
        "u16" | "i16" => Ok((false, 2)),
        "u32" | "i32" | "f32" => Ok((false, 4)),
        "u64" | "i64" | "f64" => Ok((false, 8)),
        _ => Err(not_supported()),
    }
}
