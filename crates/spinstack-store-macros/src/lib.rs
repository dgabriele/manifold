extern crate proc_macro;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, Type};

#[proc_macro_derive(Store, attributes(primary_key, indexed))]
pub fn derive_store(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;
    let table_name = struct_name.to_string().to_lowercase();

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => panic!("Store derive only supports structs with named fields"),
        },
        _ => panic!("Store derive only supports structs"),
    };

    let num_fields = fields.len();

    // Collect field info
    let mut field_infos: Vec<FieldInfo> = Vec::new();
    let mut primary_key_index: Option<usize> = None;

    for (i, field) in fields.iter().enumerate() {
        let name = field.ident.as_ref().unwrap();
        let ty = &field.ty;
        let type_str = type_to_string(ty);

        let is_primary_key = field.attrs.iter().any(|a| a.path().is_ident("primary_key"));
        let is_indexed = field.attrs.iter().any(|a| a.path().is_ident("indexed"));

        if is_primary_key {
            if primary_key_index.is_some() {
                panic!("Only one #[primary_key] field is allowed");
            }
            primary_key_index = Some(i);
        }

        field_infos.push(FieldInfo {
            name: name.clone(),
            type_str,
            is_primary_key,
            is_indexed,
            index: i,
        });
    }

    let primary_key_index =
        primary_key_index.expect("Store derive requires exactly one #[primary_key] field");

    // Generate field constant accessors
    let field_constants: Vec<_> = field_infos
        .iter()
        .map(|fi| {
            let const_name = format_ident!("{}", fi.name.to_string().to_uppercase());
            let idx = fi.index as u16;
            let field_ty = rust_type_tokens(&fi.type_str);
            quote! {
                pub const #const_name: spinstack_store::Field<#field_ty> =
                    spinstack_store::Field::new(#idx);
            }
        })
        .collect();

    // Generate ColumnDef entries for schema
    let column_defs: Vec<_> = field_infos
        .iter()
        .map(|fi| {
            let name_str = fi.name.to_string();
            let col_type = column_type_tokens(&fi.type_str);
            let is_pk = fi.is_primary_key;
            let is_idx = fi.is_indexed;
            quote! {
                spinstack_store::ColumnDef {
                    name: #name_str,
                    column_type: #col_type,
                    is_primary_key: #is_pk,
                    is_indexed: #is_idx,
                }
            }
        })
        .collect();

    // Generate encode calls for to_store_bytes
    let encode_calls: Vec<_> = field_infos
        .iter()
        .map(|fi| {
            let name = &fi.name;
            let encode_fn = format_ident!("encode_{}", fi.type_str.to_lowercase());
            if fi.type_str == "String" {
                quote! { spinstack_store::serialize::#encode_fn(&mut buf, &self.#name); }
            } else {
                quote! { spinstack_store::serialize::#encode_fn(&mut buf, self.#name); }
            }
        })
        .collect();

    // Generate decode calls for from_store_bytes
    let decode_calls: Vec<_> = field_infos
        .iter()
        .map(|fi| {
            let name = &fi.name;
            let decode_fn = format_ident!("decode_{}", fi.type_str.to_lowercase());
            quote! {
                let (#name, __n) = spinstack_store::serialize::#decode_fn(&bytes[__offset..])?;
                __offset += __n;
            }
        })
        .collect();

    let field_names: Vec<_> = field_infos.iter().map(|fi| &fi.name).collect();

    // Generate primary_key_bytes
    let pk_info = &field_infos[primary_key_index];
    let pk_name = &pk_info.name;
    let pk_encode_fn = format_ident!("encode_{}", pk_info.type_str.to_lowercase());
    let pk_encode = if pk_info.type_str == "String" {
        quote! { spinstack_store::serialize::#pk_encode_fn(&mut buf, &self.#pk_name); }
    } else {
        quote! { spinstack_store::serialize::#pk_encode_fn(&mut buf, self.#pk_name); }
    };

    let expanded = quote! {
        impl #struct_name {
            #(#field_constants)*
        }

        impl spinstack_store::StoreRecord for #struct_name {
            const TABLE_NAME: &'static str = #table_name;

            fn schema() -> spinstack_store::TableSchema {
                static COLUMNS: [spinstack_store::ColumnDef; #num_fields] = [
                    #(#column_defs),*
                ];
                spinstack_store::TableSchema {
                    table_name: #table_name,
                    columns: &COLUMNS,
                }
            }

            fn to_store_bytes(&self) -> Vec<u8> {
                let mut buf = Vec::new();
                #(#encode_calls)*
                buf
            }

            fn from_store_bytes(bytes: &[u8]) -> spinstack_store::Result<Self> {
                let mut __offset = 0usize;
                #(#decode_calls)*
                Ok(Self { #(#field_names),* })
            }

            fn primary_key_bytes(&self) -> Vec<u8> {
                let mut buf = Vec::new();
                #pk_encode
                buf
            }
        }
    };

    TokenStream::from(expanded)
}

struct FieldInfo {
    name: syn::Ident,
    type_str: String,
    is_primary_key: bool,
    is_indexed: bool,
    index: usize,
}

fn type_to_string(ty: &Type) -> String {
    let s = quote!(#ty).to_string().replace(' ', "");
    match s.as_str() {
        "u64" => "u64".to_string(),
        "i64" => "i64".to_string(),
        "f64" => "f64".to_string(),
        "String" => "String".to_string(),
        "bool" => "bool".to_string(),
        other => panic!("Unsupported field type: {other}. Supported: u64, i64, f64, String, bool"),
    }
}

fn rust_type_tokens(type_str: &str) -> proc_macro2::TokenStream {
    match type_str {
        "u64" => quote!(u64),
        "i64" => quote!(i64),
        "f64" => quote!(f64),
        "String" => quote!(String),
        "bool" => quote!(bool),
        _ => unreachable!(),
    }
}

fn column_type_tokens(type_str: &str) -> proc_macro2::TokenStream {
    match type_str {
        "u64" => quote!(spinstack_store::ColumnType::U64),
        "i64" => quote!(spinstack_store::ColumnType::I64),
        "f64" => quote!(spinstack_store::ColumnType::F64),
        "String" => quote!(spinstack_store::ColumnType::String),
        "bool" => quote!(spinstack_store::ColumnType::Bool),
        _ => unreachable!(),
    }
}
