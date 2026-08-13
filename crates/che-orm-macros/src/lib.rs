use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Expr, Fields, Lit, Path, Type, parse_macro_input};

#[proc_macro_derive(Model, attributes(model, field))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_model(input)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

#[proc_macro_derive(Choice)]
pub fn derive_choice(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_choice(input)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

fn expand_choice(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let enum_name = input.ident;
    let Data::Enum(data) = input.data else {
        return Err(syn::Error::new_spanned(
            enum_name,
            "Choice requires an enum",
        ));
    };

    let mut variants = Vec::new();
    let mut values = Vec::new();
    for variant in data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                "Choice variants must be unit variants",
            ));
        }
        let value = snake_case(&variant.ident.to_string());
        if values.iter().any(|existing| existing == &value) {
            return Err(syn::Error::new_spanned(
                variant.ident,
                "Choice variants must have unique snake_case values",
            ));
        }
        variants.push((variant.ident, value.clone()));
        values.push(value);
    }

    let value_literals = values.iter().map(|value| quote!(#value));
    let as_str_arms = variants
        .iter()
        .map(|(variant, value)| quote!(Self::#variant => #value));
    let from_str_arms = variants
        .iter()
        .map(|(variant, value)| quote!(#value => Ok(Self::#variant)));
    let choice_query_value_impl = quote! {
        impl ::che_orm::QueryValue<#enum_name> for #enum_name {
            fn into_query_value(self) -> ::che_orm::DatabaseValue {
                ::che_orm::DatabaseValue::String(
                    <#enum_name as ::che_orm::Choice>::as_str(&self).to_string()
                )
            }
        }
    };
    let sqlite_choice_impl = if cfg!(feature = "sqlite") {
        quote! {
            impl ::che_orm::ProjectionValue for #enum_name {
                fn from_projection_row(
                    row: &::che_orm::__private::sqlx::sqlite::SqliteRow,
                    field: &str,
                ) -> ::che_orm::Result<Self> {
                    let value: ::std::string::String =
                        ::che_orm::__private::sqlx::Row::try_get(row, field)?;
                    <#enum_name as ::che_orm::Choice>::from_str(&value)
                        .map_err(::che_orm::Error::ProjectionDecode)
                }
            }

            impl ::che_orm::OptionalProjectionValue for #enum_name {
                fn from_optional_projection_row(
                    row: &::che_orm::__private::sqlx::sqlite::SqliteRow,
                    field: &str,
                ) -> ::che_orm::Result<::std::option::Option<Self>> {
                    let value: ::std::option::Option<::std::string::String> =
                        ::che_orm::__private::sqlx::Row::try_get(row, field)?;
                    value
                        .map(|value| <#enum_name as ::che_orm::Choice>::from_str(&value)
                            .map_err(::che_orm::Error::ProjectionDecode))
                        .transpose()
                }
            }
        }
    } else {
        quote! {}
    };

    Ok(quote! {
        impl ::che_orm::Choice for #enum_name {
            fn as_str(&self) -> &'static str {
                match self {
                    #(#as_str_arms,)*
                }
            }

            fn from_str(value: &str) -> ::std::result::Result<Self, ::std::string::String> {
                match value {
                    #(#from_str_arms,)*
                    _ => Err(format!("invalid {} choice: {value}", stringify!(#enum_name))),
                }
            }

            fn values() -> &'static [&'static str] {
                &[#(#value_literals),*]
            }
        }

        #choice_query_value_impl
        #sqlite_choice_impl
    })
}

fn expand_model(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let model_name = input.ident;
    let table_name =
        model_table_name(&input.attrs)?.unwrap_or_else(|| snake_case(&model_name.to_string()));
    validate_identifier(&table_name, "table")?;
    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    model_name,
                    "Model requires named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                model_name,
                "Model can only be derived for structs",
            ));
        }
    };

    let fields_name = format_ident!("{}Fields", model_name);
    let relations_name = format_ident!("{}Relations", model_name);

    let mut infos = Vec::new();
    let mut row_fields = Vec::new();
    let mut postgres_row_fields = Vec::new();
    let mut id_ty = None;
    let mut id_ident = None;
    let mut primary_key_count = 0;
    let mut save_values = Vec::new();
    let mut value_arms = Vec::new();
    let mut field_constants = Vec::new();
    let mut relation_constants = Vec::new();
    let mut relation_names = Vec::new();

    for field in fields {
        let ident = field.ident.expect("named field");
        let ty = field.ty;
        let attrs = field_attrs(&field.attrs)?;
        let rust_name = ident.to_string();
        let db_name = attrs.rename.unwrap_or_else(|| rust_name.clone());
        validate_identifier(&db_name, "column")?;
        let field_constant = format_ident!("{}", rust_name.to_ascii_uppercase());
        let query_ty = query_field_type(&ty);
        let choice_type = choice_type(&ty);
        let field_type = if is_file_path(&ty) {
            quote!(::che_orm::FieldType::FilePath)
        } else if choice_type.is_some() {
            quote!(::che_orm::FieldType::Choice)
        } else {
            field_type(&ty)?
        };
        let primary_key = attrs.primary_key;
        let auto = attrs.auto || primary_key && is_i64(&ty);
        let nullable = is_option(&ty);
        let unique = attrs.unique;
        let index = attrs.index;
        let max_length = attrs.max_length;
        let default = attrs.default;
        let foreign_key = attrs.foreign_key;
        let on_delete = attrs.on_delete;

        if foreign_key.is_some() {
            if !is_i64_or_option_i64(&ty) {
                return Err(syn::Error::new_spanned(
                    &ty,
                    "foreign key fields must be i64 or Option<i64>",
                ));
            }
            if matches!(on_delete.as_deref(), Some("SetNull")) && !is_option_i64(&ty) {
                return Err(syn::Error::new_spanned(
                    &ty,
                    "SET NULL foreign keys must be Option<i64>",
                ));
            }
            if matches!(on_delete.as_deref(), Some("SetDefault")) && default.is_none() {
                return Err(syn::Error::new_spanned(
                    &ident,
                    "SET DEFAULT foreign keys require a default",
                ));
            }
        } else if on_delete.is_some() {
            return Err(syn::Error::new_spanned(
                &ident,
                "on_delete requires foreign_key",
            ));
        }

        if let Some(foreign_model) = foreign_key.clone() {
            let forward_name = rust_name
                .strip_suffix("_id")
                .unwrap_or(&rust_name)
                .to_ascii_uppercase();
            if relation_names.contains(&forward_name) {
                return Err(syn::Error::new_spanned(
                    &ident,
                    format!("duplicate relation descriptor name: {forward_name}"),
                ));
            }
            relation_names.push(forward_name.clone());
            let forward_name = format_ident!("{forward_name}");
            relation_constants.push(quote! {
                pub const #forward_name: ::che_orm::BelongsTo<#model_name, #foreign_model> =
                    ::che_orm::BelongsTo::new(#db_name);
            });
        }
        let auto_now_add = attrs.auto_now_add;
        let auto_now = attrs.auto_now;

        field_constants.push(quote! {
            pub const #field_constant: ::che_orm::ModelField<#model_name, #query_ty> =
                unsafe { ::che_orm::ModelField::new(#db_name) };
        });

        if auto_now_add && auto_now {
            return Err(syn::Error::new_spanned(
                &ident,
                "field cannot use both auto_now_add and auto_now",
            ));
        }
        if (auto_now_add || auto_now) && !is_naive_datetime(&ty) {
            return Err(syn::Error::new_spanned(
                &ty,
                "auto_now_add and auto_now require chrono::NaiveDateTime",
            ));
        }
        if primary_key && (auto_now_add || auto_now) {
            return Err(syn::Error::new_spanned(
                &ident,
                "primary key fields cannot use auto_now_add or auto_now",
            ));
        }

        if primary_key {
            primary_key_count += 1;
            if primary_key_count > 1 {
                return Err(syn::Error::new_spanned(
                    &ident,
                    "Model must have exactly one primary_key field",
                ));
            }
            id_ty = Some(ty.clone());
            id_ident = Some(ident.clone());
        }

        let max_length_tokens = match max_length {
            Some(value) => quote!(Some(#value)),
            None => quote!(None),
        };
        let default_tokens = match default {
            Some(value) => quote!(Some(#value)),
            None => quote!(None),
        };
        let foreign_key_tokens = match foreign_key {
            Some(model) => {
                let on_delete = action_tokens(on_delete.as_deref(), "NoAction");
                quote!(Some(::che_orm::ForeignKeyInfo {
                    table: <#model as ::che_orm::Model>::table_name(),
                    on_delete: #on_delete,
                }))
            }
            None => quote!(None),
        };
        let choices_tokens = match choice_type {
            Some(choice_type) => quote!(Some(<#choice_type as ::che_orm::Choice>::values())),
            None => quote!(None),
        };

        infos.push(quote! {
            ::che_orm::FieldInfo {
                rust_name: #rust_name,
                db_name: #db_name,
                ty: #field_type,
                primary_key: #primary_key,
                nullable: #nullable,
                auto: #auto,
                unique: #unique,
                index: #index,
                max_length: #max_length_tokens,
                default: #default_tokens,
                auto_now_add: #auto_now_add,
                auto_now: #auto_now,
                foreign_key: #foreign_key_tokens,
                choices: #choices_tokens,
            }
        });

        row_fields.push(row_field_quote(&ident, &ty, &db_name));
        postgres_row_fields.push(postgres_row_field_quote(&ident, &ty, &db_name));

        if !primary_key && !auto_now_add && !auto_now {
            save_values.push(database_value_ref_quote(&ident, &ty, &db_name));
        }

        value_arms.push(model_value_arm(&ident, &db_name, &ty));
        if db_name != rust_name {
            value_arms.push(model_value_arm(&ident, &rust_name, &ty));
        }
    }

    let id_ty = id_ty.ok_or_else(|| {
        syn::Error::new_spanned(&model_name, "Model requires #[field(primary_key)]")
    })?;
    let id_ident = id_ident.ok_or_else(|| {
        syn::Error::new_spanned(&model_name, "Model requires #[field(primary_key)]")
    })?;
    if primary_key_count != 1
        || id_ident != syn::Ident::new("id", id_ident.span())
        || !is_i64(&id_ty)
    {
        return Err(syn::Error::new_spanned(
            &id_ident,
            "the primary key must be an immutable id: i64 field",
        ));
    }

    let sqlite_impl = if cfg!(feature = "sqlite") {
        quote! {
            impl ::che_orm::SqliteModel for #model_name {
                fn from_row(row: &::che_orm::__private::sqlx::sqlite::SqliteRow) -> ::che_orm::__private::sqlx::Result<Self> { Ok(Self { #(#row_fields,)* }) }
                fn id(&self) -> Self::Id { self.#id_ident.clone() }
                fn save_values(&self) -> ::std::vec::Vec<(&'static str, ::che_orm::SqliteValue)> { let mut values = ::std::vec::Vec::new(); #(#save_values)* values }
            }
        }
    } else {
        quote! {}
    };
    let postgres_impl = if cfg!(feature = "postgres") {
        quote! {
            impl ::che_orm::PostgresModel for #model_name {
                fn from_postgres_row(row: &::che_orm::__private::sqlx::postgres::PgRow) -> ::che_orm::__private::sqlx::Result<Self> { Ok(Self { #(#postgres_row_fields,)* }) }
                fn id(&self) -> Self::Id { self.#id_ident.clone() }
                fn save_values(&self) -> ::std::vec::Vec<(&'static str, ::che_orm::DatabaseValue)> { let mut values = ::std::vec::Vec::new(); #(#save_values)* values }
            }
        }
    } else {
        quote! {}
    };

    Ok(quote! {
        pub struct #fields_name;

        impl #fields_name {
            #(#field_constants)*
        }

        pub struct #relations_name;

        impl #relations_name {
            #(#relation_constants)*
        }

        impl ::che_orm::Model for #model_name {
            type Id = #id_ty;
            fn table_name() -> &'static str {
                #table_name
            }

            fn fields() -> &'static [::che_orm::FieldInfo] {
                static FIELDS: ::std::sync::OnceLock<::std::vec::Vec<::che_orm::FieldInfo>> = ::std::sync::OnceLock::new();
                FIELDS.get_or_init(|| ::std::vec![#(#infos),*]).as_slice()
            }

            fn get_value(&self, field: &str) -> ::std::option::Option<::che_orm::__private::serde_json::Value> {
                match field {
                    #(#value_arms,)*
                    _ => ::std::option::Option::None,
                }
            }
        }

        #sqlite_impl
        #postgres_impl

    })
}

fn query_field_type(ty: &Type) -> Type {
    let Type::Path(path) = ty else {
        return ty.clone();
    };
    let Some(segment) = path.path.segments.last() else {
        return ty.clone();
    };
    if segment.ident != "Option" {
        return ty.clone();
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return ty.clone();
    };
    let Some(syn::GenericArgument::Type(inner)) = arguments.args.first() else {
        return ty.clone();
    };
    inner.clone()
}

#[derive(Default)]
struct FieldAttrs {
    primary_key: bool,
    auto: bool,
    auto_now_add: bool,
    auto_now: bool,
    unique: bool,
    index: bool,
    max_length: Option<u32>,
    default: Option<String>,
    rename: Option<String>,
    foreign_key: Option<Path>,
    on_delete: Option<String>,
}

fn model_table_name(attrs: &[syn::Attribute]) -> syn::Result<Option<String>> {
    let mut table_name = None;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("model")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(lit) = lit {
                    table_name = Some(lit.value());
                    Ok(())
                } else {
                    Err(meta.error("table must be a string"))
                }
            } else {
                Err(meta.error("unsupported model attribute"))
            }
        })?;
    }
    Ok(table_name)
}

fn validate_identifier(value: &str, kind: &str) -> syn::Result<()> {
    let valid = value
        .chars()
        .enumerate()
        .all(|(index, character)| match index {
            0 => character.is_ascii_alphabetic() || character == '_',
            _ => character.is_ascii_alphanumeric() || character == '_',
        });
    if valid && !value.is_empty() {
        Ok(())
    } else {
        Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("{kind} identifier must use ASCII letters, digits, and underscores"),
        ))
    }
}

fn field_attrs(attrs: &[syn::Attribute]) -> syn::Result<FieldAttrs> {
    let mut result = FieldAttrs::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("field")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("primary_key") {
                result.primary_key = true;
                Ok(())
            } else if meta.path.is_ident("auto") {
                result.auto = true;
                Ok(())
            } else if meta.path.is_ident("auto_now_add") {
                result.auto_now_add = true;
                Ok(())
            } else if meta.path.is_ident("auto_now") {
                result.auto_now = true;
                Ok(())
            } else if meta.path.is_ident("unique") {
                result.unique = true;
                Ok(())
            } else if meta.path.is_ident("index") {
                result.index = true;
                Ok(())
            } else if meta.path.is_ident("max_length") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Int(lit) = lit {
                    result.max_length = Some(lit.base10_parse()?);
                    Ok(())
                } else {
                    Err(meta.error("max_length must be an integer"))
                }
            } else if meta.path.is_ident("default") {
                let value = meta.value()?;
                let expr: Expr = value.parse()?;
                result.default = Some(quote!(#expr).to_string());
                Ok(())
            } else if meta.path.is_ident("rename") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(lit) = lit {
                    result.rename = Some(lit.value());
                    Ok(())
                } else {
                    Err(meta.error("rename must be a string"))
                }
            } else if meta.path.is_ident("foreign_key") {
                let value = meta.value()?;
                result.foreign_key = Some(value.parse()?);
                Ok(())
            } else if meta.path.is_ident("on_delete") {
                let value: syn::Path = meta.value()?.parse()?;
                let action = value
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                    .ok_or_else(|| meta.error("action must be an identifier"))?;
                if !matches!(
                    action.as_str(),
                    "NoAction" | "Restrict" | "Cascade" | "SetNull" | "SetDefault"
                ) {
                    return Err(meta.error("unknown foreign key action"));
                }
                result.on_delete = Some(action);
                Ok(())
            } else {
                Err(meta.error("unsupported field attribute"))
            }
        })?;
    }
    Ok(result)
}

fn action_tokens(action: Option<&str>, default: &str) -> proc_macro2::TokenStream {
    let action = syn::Ident::new(action.unwrap_or(default), proc_macro2::Span::call_site());
    quote!(::che_orm::ForeignKeyAction::#action)
}

fn field_type(ty: &Type) -> syn::Result<proc_macro2::TokenStream> {
    let ty = option_inner(ty).unwrap_or(ty);
    if is_file_path(ty) {
        Ok(quote!(::che_orm::FieldType::FilePath))
    } else if is_type(ty, "i64") || is_type(ty, "i32") || is_type(ty, "u32") {
        Ok(quote!(::che_orm::FieldType::Integer))
    } else if is_type(ty, "String") {
        Ok(quote!(::che_orm::FieldType::Text))
    } else if is_type(ty, "bool") {
        Ok(quote!(::che_orm::FieldType::Boolean))
    } else if is_type(ty, "f64") || is_type(ty, "f32") {
        Ok(quote!(::che_orm::FieldType::Real))
    } else if is_naive_datetime(ty) {
        Ok(quote!(::che_orm::FieldType::DateTime))
    } else if is_json_value(ty) {
        Ok(quote!(::che_orm::FieldType::Json))
    } else {
        Err(syn::Error::new_spanned(ty, "unsupported field type"))
    }
}

fn choice_type(ty: &Type) -> Option<&Type> {
    let base = option_inner(ty).unwrap_or(ty);
    let Type::Path(path) = base else { return None };
    let name = path.path.segments.last()?.ident.to_string();
    if matches!(
        name.as_str(),
        "i64"
            | "i32"
            | "u32"
            | "String"
            | "bool"
            | "f64"
            | "f32"
            | "NaiveDateTime"
            | "Value"
            | "FilePath"
    ) {
        None
    } else {
        Some(base)
    }
}

fn is_file_path(ty: &Type) -> bool {
    let base = option_inner(ty).unwrap_or(ty);
    let Type::Path(path) = base else { return false };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "FilePath")
}

fn postgres_row_field_quote(
    ident: &syn::Ident,
    ty: &Type,
    db_name: &str,
) -> proc_macro2::TokenStream {
    if choice_type(ty).is_some() {
        return row_field_quote(ident, ty, db_name);
    }
    if is_json_value(ty) {
        return quote! {
            #ident: ::che_orm::__private::sqlx::Row::try_get::<::che_orm::__private::sqlx::types::Json<::che_orm::__private::serde_json::Value>, _>(row, #db_name)?.0
        };
    }
    if option_inner(ty).is_some_and(is_json_value) {
        return quote! {
            #ident: ::che_orm::__private::sqlx::Row::try_get::<::std::option::Option<::che_orm::__private::sqlx::types::Json<::che_orm::__private::serde_json::Value>>, _>(row, #db_name)?.map(|value| value.0)
        };
    }
    if is_file_path(ty) {
        if option_inner(ty).is_some() {
            return quote! {
                #ident: ::che_orm::__private::sqlx::Row::try_get::<::std::option::Option<::std::string::String>, _>(row, #db_name)?
                    .map(::che_orm::FilePath::new)
                    .transpose()
                    .map_err(|error| ::che_orm::__private::sqlx::Error::Decode(::std::boxed::Box::new(error)))?
            };
        }
        return quote! {
            #ident: ::che_orm::FilePath::new(::che_orm::__private::sqlx::Row::try_get::<::std::string::String, _>(row, #db_name)?)
                .map_err(|error| ::che_orm::__private::sqlx::Error::Decode(::std::boxed::Box::new(error)))?
        };
    }
    let base = option_inner(ty).unwrap_or(ty);
    if is_type(base, "u32") {
        if option_inner(ty).is_some() {
            return quote! {
                #ident: ::che_orm::__private::sqlx::Row::try_get::<::std::option::Option<i64>, _>(row, #db_name)?
                    .map(|value| u32::try_from(value).map_err(|error| ::che_orm::__private::sqlx::Error::Decode(::std::boxed::Box::new(error))))
                    .transpose()?
            };
        }
        return quote! {
            #ident: u32::try_from(::che_orm::__private::sqlx::Row::try_get::<i64, _>(row, #db_name)?)
                .map_err(|error| ::che_orm::__private::sqlx::Error::Decode(::std::boxed::Box::new(error)))?
        };
    }
    quote! { #ident: ::che_orm::__private::sqlx::Row::try_get(row, #db_name)? }
}

fn row_field_quote(ident: &syn::Ident, ty: &Type, db_name: &str) -> proc_macro2::TokenStream {
    if let Some(choice) = choice_type(ty) {
        if option_inner(ty).is_some() {
            quote! {
                #ident: {
                    let value: ::std::option::Option<::std::string::String> =
                        ::che_orm::__private::sqlx::Row::try_get(row, #db_name)?;
                    match value {
                        ::std::option::Option::Some(value) => ::std::option::Option::Some(
                            <#choice as ::che_orm::Choice>::from_str(&value)
                                .map_err(|error| ::che_orm::__private::sqlx::Error::Decode(
                                    ::std::boxed::Box::new(::std::io::Error::new(
                                        ::std::io::ErrorKind::InvalidData, error,
                                    ))
                                ))?
                        ),
                        ::std::option::Option::None => ::std::option::Option::None,
                    }
                }
            }
        } else {
            quote! {
                #ident: {
                    let value: ::std::string::String =
                        ::che_orm::__private::sqlx::Row::try_get(row, #db_name)?;
                    <#choice as ::che_orm::Choice>::from_str(&value)
                        .map_err(|error| ::che_orm::__private::sqlx::Error::Decode(
                            ::std::boxed::Box::new(::std::io::Error::new(
                                ::std::io::ErrorKind::InvalidData, error,
                            ))
                        ))?
                }
            }
        }
    } else if is_json_value(ty) {
        quote! {
            #ident: {
                let value: ::std::string::String = ::che_orm::__private::sqlx::Row::try_get(row, #db_name)?;
                ::che_orm::__private::serde_json::from_str(&value)
                    .map_err(|error| ::che_orm::__private::sqlx::Error::Decode(::std::boxed::Box::new(error)))?
            }
        }
    } else if option_inner(ty).is_some_and(is_json_value) {
        quote! {
            #ident: {
                let value: ::std::option::Option<::std::string::String> = ::che_orm::__private::sqlx::Row::try_get(row, #db_name)?;
                match value {
                    ::std::option::Option::Some(value) => ::std::option::Option::Some(
                        ::che_orm::__private::serde_json::from_str(&value)
                            .map_err(|error| ::che_orm::__private::sqlx::Error::Decode(::std::boxed::Box::new(error)))?
                    ),
                    ::std::option::Option::None => ::std::option::Option::None,
                }
            }
        }
    } else {
        quote! {
            #ident: ::che_orm::__private::sqlx::Row::try_get(row, #db_name)?
        }
    }
}

fn database_value_ref_quote(
    ident: &syn::Ident,
    ty: &Type,
    db_name: &str,
) -> proc_macro2::TokenStream {
    if let Some(choice) = choice_type(ty) {
        if option_inner(ty).is_some() {
            quote! {
                values.push((#db_name, match self.#ident.clone() {
                    ::std::option::Option::Some(value) =>
                        ::che_orm::DatabaseValue::String(
                            <#choice as ::che_orm::Choice>::as_str(&value).to_string()
                        ),
                    ::std::option::Option::None => ::che_orm::DatabaseValue::Null,
                }));
            }
        } else {
            quote! {
                values.push((#db_name, ::che_orm::DatabaseValue::String(
                    <#choice as ::che_orm::Choice>::as_str(&self.#ident).to_string()
                )));
            }
        }
    } else if is_option(ty) {
        quote! {
            values.push((#db_name, match self.#ident.clone() {
                ::std::option::Option::Some(value) => ::che_orm::DatabaseValue::from(value),
                ::std::option::Option::None => ::che_orm::DatabaseValue::Null,
            }));
        }
    } else {
        quote! {
            values.push((#db_name, ::che_orm::DatabaseValue::from(self.#ident.clone())));
        }
    }
}

fn model_value_arm(ident: &syn::Ident, name: &str, ty: &Type) -> proc_macro2::TokenStream {
    if is_file_path(ty) {
        if option_inner(ty).is_some() {
            quote! {
                #name => match &self.#ident {
                    ::std::option::Option::Some(value) => ::std::option::Option::Some(
                        ::che_orm::__private::serde_json::Value::String(value.as_str().to_string())
                    ),
                    ::std::option::Option::None => ::std::option::Option::Some(
                        ::che_orm::__private::serde_json::Value::Null
                    ),
                }
            }
        } else {
            quote! {
                #name => ::std::option::Option::Some(
                    ::che_orm::__private::serde_json::Value::String(self.#ident.as_str().to_string())
                )
            }
        }
    } else if let Some(choice) = choice_type(ty) {
        if option_inner(ty).is_some() {
            quote! {
                #name => match &self.#ident {
                    ::std::option::Option::Some(value) => ::std::option::Option::Some(
                        ::che_orm::__private::serde_json::Value::String(
                            <#choice as ::che_orm::Choice>::as_str(value).to_string()
                        )
                    ),
                    ::std::option::Option::None => ::std::option::Option::Some(
                        ::che_orm::__private::serde_json::Value::Null
                    ),
                }
            }
        } else {
            quote! {
                #name => ::std::option::Option::Some(
                    ::che_orm::__private::serde_json::Value::String(
                        <#choice as ::che_orm::Choice>::as_str(&self.#ident).to_string()
                    )
                )
            }
        }
    } else if is_json_value(ty) {
        quote! {
            #name => ::std::option::Option::Some(self.#ident.clone())
        }
    } else if option_inner(ty).is_some_and(is_json_value) {
        quote! {
            #name => match &self.#ident {
                ::std::option::Option::Some(value) => ::std::option::Option::Some(value.clone()),
                ::std::option::Option::None => ::std::option::Option::Some(
                    ::che_orm::__private::serde_json::Value::Null
                ),
            }
        }
    } else if is_naive_datetime(ty) {
        quote! {
            #name => ::std::option::Option::Some(
                ::che_orm::__private::serde_json::Value::String(self.#ident.to_string())
            )
        }
    } else if option_inner(ty).is_some_and(is_naive_datetime) {
        quote! {
            #name => match &self.#ident {
                ::std::option::Option::Some(value) => {
                    ::std::option::Option::Some(
                        ::che_orm::__private::serde_json::Value::String(value.to_string())
                    )
                }
                ::std::option::Option::None => ::std::option::Option::Some(
                    ::che_orm::__private::serde_json::Value::Null
                ),
            }
        }
    } else if is_option(ty) {
        quote! {
            #name => match &self.#ident {
                ::std::option::Option::Some(value) => {
                    ::std::option::Option::Some(
                        ::che_orm::__private::serde_json::Value::from(value.clone())
                    )
                }
                ::std::option::Option::None => ::std::option::Option::Some(
                    ::che_orm::__private::serde_json::Value::Null
                ),
            }
        }
    } else {
        quote! {
            #name => ::std::option::Option::Some(
                ::che_orm::__private::serde_json::Value::from(self.#ident.clone())
            )
        }
    }
}

fn is_option(ty: &Type) -> bool {
    option_inner(ty).is_some()
}

fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let syn::GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    Some(inner)
}

fn is_i64(ty: &Type) -> bool {
    is_type(ty, "i64")
}

fn is_option_i64(ty: &Type) -> bool {
    option_inner(ty).is_some_and(is_i64)
}

fn is_i64_or_option_i64(ty: &Type) -> bool {
    is_i64(ty) || is_option_i64(ty)
}

fn is_naive_datetime(ty: &Type) -> bool {
    is_type(ty, "NaiveDateTime")
}

fn is_json_value(ty: &Type) -> bool {
    is_type(ty, "Value")
}

fn is_type(ty: &Type, name: &str) -> bool {
    let Type::Path(path) = ty else { return false };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == name)
}

fn snake_case(input: &str) -> String {
    let mut output = String::new();
    for (index, ch) in input.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                output.push('_');
            }
            output.extend(ch.to_lowercase());
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use syn::{DeriveInput, parse_quote};

    use super::expand_model;

    #[test]
    fn rejects_non_integer_foreign_key() {
        let input = parse_quote! {
            struct Post {
                #[field(primary_key)] id: i64,
                #[field(foreign_key = User)] user_id: String,
            }
        };
        assert!(
            expand_model(input)
                .unwrap_err()
                .to_string()
                .contains("foreign key fields must be i64")
        );
    }

    #[test]
    fn rejects_multiple_primary_keys() {
        let input = parse_quote! {
            struct Post {
                #[field(primary_key)] id: i64,
                #[field(primary_key)] other_id: i64,
            }
        };
        assert!(
            expand_model(input)
                .unwrap_err()
                .to_string()
                .contains("exactly one primary_key")
        );
    }

    #[test]
    fn rejects_duplicate_relation_descriptor_names() {
        let input = parse_quote! {
            struct Post {
                #[field(primary_key)] id: i64,
                #[field(foreign_key = User)] owner_id: i64,
                #[field(foreign_key = User)] owner: i64,
            }
        };
        assert!(
            expand_model(input)
                .unwrap_err()
                .to_string()
                .contains("duplicate relation descriptor name")
        );
    }

    #[test]
    fn rejects_unsafe_table_and_column_identifiers() {
        let table: DeriveInput = parse_quote! {
            #[model(table = "users; DROP TABLE users")]
            struct User { #[field(primary_key)] id: i64 }
        };
        assert!(expand_model(table).is_err());

        let column: DeriveInput = parse_quote! {
            struct User {
                #[field(primary_key)] id: i64,
                #[field(rename = "name\"; DROP TABLE users")] name: String,
            }
        };
        assert!(expand_model(column).is_err());
    }
}
