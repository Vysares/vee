//! `#[derive(Message)]`: see the example on `crate::derive_message`.

/// One field, after reading and checking its source: its name, its kind's name in
/// `message::Kind` (such as `"F32"`), and where it falls in the wire layout.
struct MessageField<'a> {
    name: &'a syn::Ident,
    kind: &'static str,
    start: usize,
    end: usize,
}

pub fn expand(input: &syn::DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let syn::Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "Message can only be derived for a struct",
        ));
    };
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "Message does not support generics",
        ));
    }
    let syn::Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "Message needs named fields",
        ));
    };

    let mut offset = 0usize;
    let mut parsed = Vec::new();
    for field in &fields.named {
        // `Fields::Named` guarantees every field has a name.
        let Some(name) = field.ident.as_ref() else {
            continue;
        };
        let (kind, width) = field_kind(&field.ty)?;
        let end = offset.checked_add(width).ok_or_else(|| {
            syn::Error::new_spanned(field, "message is too large: its wire layout overflows")
        })?;
        parsed.push(MessageField {
            name,
            kind,
            start: offset,
            end,
        });
        offset = end;
    }
    let wire_len = offset;

    // `Vec`s, not lazy iterators: each is interpolated twice below (once for the inherent impl,
    // once for `impl Message`), and `quote!` needs a `Vec` (iterated by reference) to do that,
    // not an iterator (which a second use would find already consumed).
    let layout_entries: Vec<_> = parsed
        .iter()
        .map(|field| {
            let name = field.name;
            let start = field.start;
            let kind = syn::Ident::new(field.kind, name.span());
            quote::quote! {
                ::vee_core_api::message::Field {
                    name: stringify!(#name),
                    offset: #start,
                    kind: ::vee_core_api::message::Kind::#kind,
                }
            }
        })
        .collect();

    let encode_stmts: Vec<_> = parsed
        .iter()
        .map(|field| {
            let name = field.name;
            let (start, end) = (field.start, field.end);
            if field.kind == "Bool" {
                quote::quote! { out[#start] = u8::from(self.#name); }
            } else {
                quote::quote! { out[#start..#end].copy_from_slice(&self.#name.to_be_bytes()); }
            }
        })
        .collect();

    let struct_name = &input.ident;
    Ok(quote::quote! {
        impl #struct_name {
            /// The size of this message's wire encoding: see `encode`.
            pub const WIRE_LEN: usize = #wire_len;

            /// This message type's field layout: name, wire offset and exact kind, one entry per
            /// field, in declaration order. Shared by every topic of this message type.
            pub const LAYOUT: &'static [::vee_core_api::message::Field] = &[ #(#layout_entries,)* ];

            /// Writes this value's wire bytes: big endian, declaration order, `Self::WIRE_LEN`
            /// bytes in all.
            pub fn encode(&self) -> [u8; #wire_len] {
                let mut out = [0u8; #wire_len];
                #(#encode_stmts)*
                out
            }
        }

        // For generic code that only knows `T: message::Message` (the dyn view's
        // `impl<T: Message> Topic for StateTopic<T>`): the same values and bytes as the
        // inherent items above, through the trait instead of this struct's own name.
        impl ::vee_core_api::message::Message for #struct_name {
            const WIRE_LEN: usize = #wire_len;
            const LAYOUT: &'static [::vee_core_api::message::Field] = &[ #(#layout_entries,)* ];

            fn encode_into(&self, out: &mut [u8]) -> ::core::option::Option<usize> {
                if out.len() < #wire_len {
                    return ::core::option::Option::None;
                }
                out[..#wire_len].copy_from_slice(&self.encode());
                ::core::option::Option::Some(#wire_len)
            }
        }
    })
}

/// The `message::Kind` variant name and byte width for a field's Rust type, or an error if the
/// type is not one `Message` supports yet.
fn field_kind(ty: &syn::Type) -> syn::Result<(&'static str, usize)> {
    let not_supported = || {
        syn::Error::new_spanned(
            ty,
            "Message fields must be bool or a fixed-width number (u8..u64, i8..i64, f32, f64): \
             nested structs, arrays and text are not supported yet",
        )
    };
    let syn::Type::Path(path) = ty else {
        return Err(not_supported());
    };
    let ident = path.path.get_ident().ok_or_else(not_supported)?;
    match ident.to_string().as_str() {
        "bool" => Ok(("Bool", 1)),
        "u8" => Ok(("U8", 1)),
        "i8" => Ok(("I8", 1)),
        "u16" => Ok(("U16", 2)),
        "i16" => Ok(("I16", 2)),
        "u32" => Ok(("U32", 4)),
        "i32" => Ok(("I32", 4)),
        "f32" => Ok(("F32", 4)),
        "u64" => Ok(("U64", 8)),
        "i64" => Ok(("I64", 8)),
        "f64" => Ok(("F64", 8)),
        _ => Err(not_supported()),
    }
}
