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
        return Err(syn::Error::new_spanned(enum_name, "Choice requires an enum"));
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
    let as_str_arms = variants.iter().map(|(variant, value)| quote!(Self::#variant => #value));
    let from_str_arms = variants.iter().map(|(variant, value)| {
        quote!(#value => Ok(Self::#variant))
    });

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
    })
}

fn expand_model(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let model_name = input.ident;
    let table_name =
        model_table_name(&input.attrs)?.unwrap_or_else(|| snake_case(&model_name.to_string()));
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

    let update_name = format_ident!("{}Update", model_name);
    let fields_name = format_ident!("{}Fields", model_name);

    let mut infos = Vec::new();
    let mut row_fields = Vec::new();
    let mut update_fields = Vec::new();
    let mut update_values = Vec::new();
    let mut id_ty = None;
    let mut id_ident = None;
    let mut save_values = Vec::new();
    let mut value_arms = Vec::new();
    let mut field_constants = Vec::new();

    for field in fields {
        let ident = field.ident.expect("named field");
        let ty = field.ty;
        let attrs = field_attrs(&field.attrs)?;
        let rust_name = ident.to_string();
        let db_name = attrs.rename.unwrap_or_else(|| rust_name.clone());
        let field_constant = format_ident!("{}", rust_name.to_ascii_uppercase());
        let choice_type = choice_type(&ty);
        let field_type = if choice_type.is_some() {
            quote!(::che_orm::FieldType::Choice)
        } else {
            field_type(&ty)?
        };
        let primary_key = attrs.primary_key;
        let auto = attrs.auto || primary_key && is_i64(&ty);
        let nullable = is_option(&ty);
        let unique = attrs.unique;
        let max_length = attrs.max_length;
        let default = attrs.default;
        let foreign_key = attrs.foreign_key;
        let auto_now_add = attrs.auto_now_add;
        let auto_now = attrs.auto_now;

        field_constants.push(quote! {
            pub const #field_constant: ::che_orm::ModelField<#model_name> =
                ::che_orm::ModelField::new(#db_name);
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
            Some(model) => quote!(Some(::che_orm::ForeignKeyInfo {
                table: <#model as ::che_orm::Model>::table_name(),
                column: "id",
            })),
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
                max_length: #max_length_tokens,
                default: #default_tokens,
                auto_now_add: #auto_now_add,
                auto_now: #auto_now,
                foreign_key: #foreign_key_tokens,
                choices: #choices_tokens,
            }
        });

        row_fields.push(row_field_quote(&ident, &ty, &db_name));

        if !primary_key && !auto_now_add && !auto_now {
            update_fields.push(quote! { pub #ident: ::std::option::Option<#ty> });
            update_values.push(sqlite_value_update_quote(&ident, &ty, &db_name));
            save_values.push(sqlite_value_ref_quote(&ident, &ty, &db_name));
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

    Ok(quote! {
        #[derive(Debug, Clone, Default)]
        pub struct #update_name {
            #(#update_fields,)*
        }

        pub struct #fields_name;

        impl #fields_name {
            #(#field_constants)*
        }

        impl ::che_orm::Model for #model_name {
            type Id = #id_ty;
            type Update = #update_name;

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

        impl #model_name {
            pub async fn save(&self, db: &::che_orm::SqliteBackend) -> ::che_orm::Result<Self> {
                <Self as ::che_orm::Model>::objects(db).save(self).await
            }
        }

        impl ::che_orm::SqliteModel for #model_name {
            fn from_row(row: &::che_orm::__private::sqlx::sqlite::SqliteRow) -> ::che_orm::__private::sqlx::Result<Self> {
                Ok(Self {
                    #(#row_fields,)*
                })
            }

            fn id(&self) -> Self::Id {
                self.#id_ident.clone()
            }

            fn update_values(data: Self::Update) -> ::std::vec::Vec<(&'static str, ::che_orm::SqliteValue)> {
                let mut values = ::std::vec::Vec::new();
                #(#update_values)*
                values
            }

            fn save_values(&self) -> ::std::vec::Vec<(&'static str, ::che_orm::SqliteValue)> {
                let mut values = ::std::vec::Vec::new();
                #(#save_values)*
                values
            }
        }

    })
}

#[derive(Default)]
struct FieldAttrs {
    primary_key: bool,
    auto: bool,
    auto_now_add: bool,
    auto_now: bool,
    unique: bool,
    max_length: Option<u32>,
    default: Option<String>,
    rename: Option<String>,
    foreign_key: Option<Path>,
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
            } else {
                Err(meta.error("unsupported field attribute"))
            }
        })?;
    }
    Ok(result)
}

fn field_type(ty: &Type) -> syn::Result<proc_macro2::TokenStream> {
    let ty = option_inner(ty).unwrap_or(ty);
    if is_type(ty, "i64") || is_type(ty, "i32") || is_type(ty, "u32") {
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
        "i64" | "i32" | "u32" | "String" | "bool" | "f64" | "f32" | "NaiveDateTime" | "Value"
    ) {
        None
    } else {
        Some(base)
    }
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

fn sqlite_value_ref_quote(
    ident: &syn::Ident,
    ty: &Type,
    db_name: &str,
) -> proc_macro2::TokenStream {
    if let Some(choice) = choice_type(ty) {
        if option_inner(ty).is_some() {
            quote! {
                values.push((#db_name, match self.#ident.clone() {
                    ::std::option::Option::Some(value) =>
                        ::che_orm::SqliteValue::String(
                            <#choice as ::che_orm::Choice>::as_str(&value).to_string()
                        ),
                    ::std::option::Option::None => ::che_orm::SqliteValue::Null,
                }));
            }
        } else {
            quote! {
                values.push((#db_name, ::che_orm::SqliteValue::String(
                    <#choice as ::che_orm::Choice>::as_str(&self.#ident).to_string()
                )));
            }
        }
    } else if is_option(ty) {
        quote! {
            values.push((#db_name, match self.#ident.clone() {
                ::std::option::Option::Some(value) => ::che_orm::SqliteValue::from(value),
                ::std::option::Option::None => ::che_orm::SqliteValue::Null,
            }));
        }
    } else {
        quote! {
            values.push((#db_name, ::che_orm::SqliteValue::from(self.#ident.clone())));
        }
    }
}

fn sqlite_value_update_quote(
    ident: &syn::Ident,
    ty: &Type,
    db_name: &str,
) -> proc_macro2::TokenStream {
    if let Some(choice) = choice_type(ty) {
        if option_inner(ty).is_some() {
            quote! {
                if let ::std::option::Option::Some(value) = data.#ident {
                    values.push((#db_name, match value {
                        ::std::option::Option::Some(value) =>
                            ::che_orm::SqliteValue::String(
                                <#choice as ::che_orm::Choice>::as_str(&value).to_string()
                            ),
                        ::std::option::Option::None => ::che_orm::SqliteValue::Null,
                    }));
                }
            }
        } else {
            quote! {
                if let ::std::option::Option::Some(value) = data.#ident {
                    values.push((#db_name, ::che_orm::SqliteValue::String(
                        <#choice as ::che_orm::Choice>::as_str(&value).to_string()
                    )));
                }
            }
        }
    } else if is_option(ty) {
        quote! {
            if let ::std::option::Option::Some(value) = data.#ident {
                values.push((#db_name, match value {
                    ::std::option::Option::Some(value) => ::che_orm::SqliteValue::from(value),
                    ::std::option::Option::None => ::che_orm::SqliteValue::Null,
                }));
            }
        }
    } else {
        quote! {
            if let ::std::option::Option::Some(value) = data.#ident {
                values.push((#db_name, ::che_orm::SqliteValue::from(value)));
            }
        }
    }
}

fn model_value_arm(ident: &syn::Ident, name: &str, ty: &Type) -> proc_macro2::TokenStream {
    if let Some(choice) = choice_type(ty) {
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
