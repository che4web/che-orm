use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, Ident, LitStr, Path, Token, parse_macro_input};

#[proc_macro_derive(Model, attributes(orm))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    match derive_model_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

#[proc_macro_derive(ModelSerializer, attributes(serializer))]
pub fn derive_model_serializer(input: TokenStream) -> TokenStream {
    match derive_model_serializer_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

fn derive_model_serializer_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let model = parse_serializer_model(&input.attrs)?;
    let serializer = input.ident.clone();
    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return Err(Error::new_spanned(
                    serializer,
                    "ModelSerializer can only be derived for structs with named fields",
                ));
            }
        },
        _ => {
            return Err(Error::new_spanned(
                serializer,
                "ModelSerializer can only be derived for structs",
            ));
        }
    };
    let mut assignments = Vec::new();
    let mut nested = Vec::new();
    let mut serialize_fields = Vec::new();
    for field in fields {
        let name = field.ident.expect("named fields always have identifiers");
        let json_name = name.to_string();
        serialize_fields.push(quote! {
            ::che_orm2::serde::ser::SerializeStruct::serialize_field(
                &mut state,
                #json_name,
                &self.#name,
            )?;
        });
        let mut nested_relation = None;
        for attribute in &field.attrs {
            if !attribute.path().is_ident("serializer") {
                continue;
            }
            attribute.parse_nested_meta(|meta| {
                    if meta.path.is_ident("read_only") {
                        Ok(())
                    } else if meta.path.is_ident("many") || meta.path.is_ident("one") {
                        let many = meta.path.is_ident("many");
                        let model: Path = meta.value()?.parse()?;
                        if nested_relation.is_some() {
                            return Err(meta.error("a serializer field can have only one relation"));
                        }
                        nested_relation = Some((many, model));
                        Ok(())
                    } else {
                        Err(meta.error(
                            "unsupported serializer field attribute; expected read_only, many = Model or one = Model",
                        ))
                    }
                })?;
        }
        if let Some((many, related_model)) = nested_relation {
            nested.push((name, many, related_model));
        } else {
            assignments.push(quote! { #name: model.#name });
        }
    }
    if nested.len() > 1 {
        return Err(Error::new(
            proc_macro2::Span::call_site(),
            "a ModelSerializer currently supports one nested relation field",
        ));
    }

    let has_nested = !nested.is_empty();
    let field_count = serialize_fields.len();
    let conversion = if let Some((name, many, related_model)) = nested.into_iter().next() {
        let wrapper = if many {
            quote! { ::che_orm2::WithMany<#model, #related_model> }
        } else {
            quote! { ::che_orm2::WithOne<#model, #related_model> }
        };
        let nested_assignment = if many {
            quote! { #name: value.related.into_iter().map(::core::convert::Into::into).collect() }
        } else {
            quote! { #name: ::core::convert::Into::into(value.related) }
        };
        quote! {
            impl ::core::convert::From<#wrapper> for #serializer {
                fn from(value: #wrapper) -> Self {
                    let model = value.model;
                    Self { #(#assignments,)* #nested_assignment }
                }
            }

            impl #serializer {
                pub fn many<I>(values: I) -> ::std::vec::Vec<Self>
                where
                    I: ::core::iter::IntoIterator<Item = #wrapper>,
                {
                    values.into_iter().map(::core::convert::Into::into).collect()
                }
            }
        }
    } else {
        quote! {
        impl ::core::convert::From<#model> for #serializer {
                fn from(model: #model) -> Self {
                    <Self as ::che_orm2::ModelSerializer>::from_model(model)
                }
            }

            impl #serializer {
                pub fn many<I>(values: I) -> ::std::vec::Vec<Self>
                where
                    I: ::core::iter::IntoIterator<Item = #model>,
                {
                    values.into_iter().map(::core::convert::Into::into).collect()
                }
            }
        }
    };

    let trait_impl = if has_nested {
        quote! {}
    } else {
        quote! {
            impl ::che_orm2::ModelSerializer for #serializer {
                type Model = #model;

                fn from_model(model: Self::Model) -> Self {
                    Self { #(#assignments),* }
                }
            }
        }
    };

    Ok(quote! {
        impl ::che_orm2::serde::Serialize for #serializer {
            fn serialize<__Serializer>(
                &self,
                serializer: __Serializer,
            ) -> ::core::result::Result<__Serializer::Ok, __Serializer::Error>
            where
                __Serializer: ::che_orm2::serde::Serializer,
            {
                let mut state = ::che_orm2::serde::Serializer::serialize_struct(
                    serializer,
                    stringify!(#serializer),
                    #field_count,
                )?;
                #(#serialize_fields)*
                ::che_orm2::serde::ser::SerializeStruct::end(state)
            }
        }

        #trait_impl
        #conversion
    })
}

fn parse_serializer_model(attributes: &[syn::Attribute]) -> syn::Result<Path> {
    let mut model = None;
    for attribute in attributes {
        if !attribute.path().is_ident("serializer") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("model") {
                model = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `model = Model`"))
            }
        })?;
    }
    model.ok_or_else(|| {
        Error::new(
            proc_macro2::Span::call_site(),
            "serializer requires `#[serializer(model = Model)]`",
        )
    })
}

fn derive_model_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let model_attributes = parse_model_attributes(&input)?;
    let table = model_attributes.table;
    validate_identifier(&table.value(), table.span(), "table")?;
    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return Err(Error::new_spanned(
                    input.ident,
                    "Model can only be derived for structs with named fields",
                ));
            }
        },
        _ => {
            return Err(Error::new_spanned(
                input.ident,
                "Model can only be derived for structs",
            ));
        }
    };

    let model = input.ident;
    let mut columns = Vec::with_capacity(fields.len());
    let mut constants = Vec::with_capacity(fields.len());
    let mut schema_columns = Vec::with_capacity(fields.len());
    let mut row_fields = Vec::with_capacity(fields.len());
    let mut insert_values = Vec::with_capacity(fields.len());
    let mut managed_update_values = Vec::with_capacity(fields.len());
    let mut relation_constants = Vec::new();
    let mut primary_key_seen = false;
    let mut primary_key_constant = None;
    let mut primary_key_field = None;

    for (index, field) in fields.into_iter().enumerate() {
        let field_name = field.ident.expect("named fields always have identifiers");
        let column = field_name.to_string();
        let constant = Ident::new(&column.to_uppercase(), field_name.span());
        let field_type = field.ty;
        let field_attributes = parse_field_attributes(&field.attrs)?;

        if field_attributes.foreign_key.is_some() && !is_i64(&field_type) {
            return Err(Error::new_spanned(
                &field_type,
                "foreign_key currently requires an i64 field",
            ));
        }

        if field_attributes.primary_key {
            if primary_key_seen {
                return Err(Error::new_spanned(
                    &field_name,
                    "a model can have only one primary_key field",
                ));
            }
            if !is_i64(&field_type) {
                return Err(Error::new_spanned(
                    &field_type,
                    "primary_key requires an i64 field",
                ));
            }
            primary_key_seen = true;
            primary_key_constant = Some(constant.clone());
            primary_key_field = Some(field_name.clone());
        }

        if (field_attributes.auto_now_add || field_attributes.auto_now)
            && !is_offset_date_time(&field_type)
        {
            return Err(Error::new_spanned(
                field_type,
                "auto_now_add and auto_now require time::OffsetDateTime",
            ));
        }

        columns.push(LitStr::new(&column, field_name.span()));
        constants.push(quote! {
            pub const #constant: ::che_orm2::ModelField<Self, #field_type> =
                ::che_orm2::ModelField::new(#table, #column);
        });

        let primary_key = field_attributes.primary_key;
        let unique = field_attributes.unique;
        let auto_now_add = field_attributes.auto_now_add;
        let auto_now = field_attributes.auto_now;
        let default = field_attributes
            .default
            .map(|value| quote! { column.default = Some(#value); });
        let check = field_attributes
            .check
            .map(|value| quote! { column.check = Some(#value); });
        let references = field_attributes.references.map(|target| {
            let on_delete = field_attributes
                .on_delete
                .clone()
                .map(|value| quote! { Some(#value) })
                .unwrap_or_else(|| quote! { None });
            quote! {
                column.references = Some(::che_orm2::ForeignKey {
                    target: #target.to_string(),
                    on_delete: #on_delete,
                });
            }
        });
        let foreign_key = field_attributes.foreign_key.as_ref().map(|target| {
            let on_delete = field_attributes
                .on_delete
                .clone()
                .map(|value| quote! { Some(#value) })
                .unwrap_or_else(|| quote! { None });
            quote! {
                column.references = Some(::che_orm2::ForeignKey {
                    target: format!(
                        "{}({})",
                        <#target as ::che_orm2::Model>::table_name(),
                        <#target as ::che_orm2::Model>::primary_key().column().name,
                    ),
                    on_delete: #on_delete,
                });
            }
        });
        if let Some(target) = &field_attributes.foreign_key {
            let relation_name = column.strip_suffix("_id").unwrap_or(&column).to_uppercase();
            let relation_constant = Ident::new(&relation_name, field_name.span());
            let reverse_name = format!("{}_set", model.to_string().to_lowercase());
            relation_constants.push(quote! {
                pub const #relation_constant: ::che_orm2::BelongsTo<Self, #target> =
                    ::che_orm2::BelongsTo::new(
                        #table,
                        #column,
                        |model: &Self| model.#field_name,
                        #reverse_name,
                    );
            });
        }

        schema_columns.push(quote! {
            let mut column = ::che_orm2::ColumnSchema::new(
                #column,
                <#field_type as ::che_orm2::ColumnTypeOf>::column_type(),
                <#field_type as ::che_orm2::ColumnTypeOf>::nullable(),
            );
            column.primary_key = #primary_key;
            column.unique = #unique;
            column.auto_now_add = #auto_now_add;
            column.auto_now = #auto_now;
            #default
            #check
            #references
            #foreign_key
            columns.push(column);
        });

        row_fields.push(quote! {
            #field_name: row.get(#index)?,
        });

        if !field_attributes.primary_key && !auto_now_add && !auto_now {
            insert_values.push(quote! {
                ::che_orm2::InsertValue {
                    column: ::che_orm2::ColumnRef::new(#table, #column),
                    value: ::che_orm2::QueryValue::<#field_type>::into_query_value(
                        self.#field_name.clone(),
                    ),
                }
            });
        }

        if auto_now {
            managed_update_values.push(quote! {
                ::che_orm2::Assignment {
                    column: ::che_orm2::ColumnRef::new(#table, #column),
                    value: ::che_orm2::QueryValue::<#field_type>::into_query_value(
                        ::che_orm2::time::OffsetDateTime::now_utc(),
                    ),
                }
            });
        }
    }

    let primary_key_constant = primary_key_constant.ok_or_else(|| {
        Error::new_spanned(
            &model,
            "Model requires exactly one #[orm(primary_key)] field",
        )
    })?;
    let primary_key_field = primary_key_field.expect("primary key constant has a field");

    let unique_constraints = model_attributes.unique_constraints.iter().map(|columns| {
        quote! { vec![#(#columns),*] }
    });
    let indexes = model_attributes.indexes.iter().map(|columns| {
        quote! { vec![#(#columns),*] }
    });

    Ok(quote! {
        impl ::che_orm2::Model for #model {
            fn table_name() -> &'static str {
                #table
            }

            fn columns() -> &'static [&'static str] {
                &[#(#columns),*]
            }

            fn primary_key() -> ::che_orm2::ModelField<Self, i64> {
                Self::#primary_key_constant
            }

            fn primary_key_value(&self) -> i64 {
                self.#primary_key_field
            }

            fn schema() -> ::che_orm2::TableSchema {
                let mut columns = Vec::new();
                #(#schema_columns)*
                ::che_orm2::TableSchema {
                    name: #table,
                    columns,
                    unique_constraints: vec![#(#unique_constraints),*],
                    indexes: vec![#(#indexes),*],
                }
            }

            fn from_row(row: &::che_orm2::rusqlite::Row<'_>) -> ::che_orm2::rusqlite::Result<Self> {
                Ok(Self {
                    #(#row_fields)*
                })
            }

            fn insert_values(&self) -> ::std::vec::Vec<::che_orm2::InsertValue> {
                vec![#(#insert_values),*]
            }

            fn managed_update_values() -> ::std::vec::Vec<::che_orm2::Assignment> {
                vec![#(#managed_update_values),*]
            }
        }

        impl #model {
            #(#constants)*
            #(#relation_constants)*
        }
    })
}

struct ModelAttributes {
    table: LitStr,
    unique_constraints: Vec<Vec<LitStr>>,
    indexes: Vec<Vec<LitStr>>,
}

fn parse_model_attributes(input: &DeriveInput) -> syn::Result<ModelAttributes> {
    let mut table = None;
    let mut unique_constraints = Vec::new();
    let mut indexes = Vec::new();

    for attribute in &input.attrs {
        if !attribute.path().is_ident("orm") {
            continue;
        }

        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("unique") || meta.path.is_ident("index") {
                let content;
                syn::parenthesized!(content in meta.input);
                let values =
                    content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                if values.is_empty() {
                    return Err(meta.error("constraint requires at least one column"));
                }
                let values = values.into_iter().collect::<Vec<_>>();
                for value in &values {
                    validate_identifier(&value.value(), value.span(), "constraint column")?;
                }
                if meta.path.is_ident("unique") {
                    unique_constraints.push(values);
                } else {
                    indexes.push(values);
                }
                Ok(())
            } else {
                Err(meta
                    .error("unsupported orm attribute; expected table, unique(...) or index(...)"))
            }
        })?;
    }

    let table = table.ok_or_else(|| {
        Error::new_spanned(
            &input.ident,
            "Model requires #[orm(table = \"table_name\")]",
        )
    })?;
    Ok(ModelAttributes {
        table,
        unique_constraints,
        indexes,
    })
}

struct FieldAttributes {
    primary_key: bool,
    unique: bool,
    default: Option<LitStr>,
    check: Option<LitStr>,
    references: Option<LitStr>,
    foreign_key: Option<Path>,
    on_delete: Option<LitStr>,
    auto_now_add: bool,
    auto_now: bool,
}

fn parse_field_attributes(attributes: &[syn::Attribute]) -> syn::Result<FieldAttributes> {
    let mut result = FieldAttributes {
        primary_key: false,
        unique: false,
        default: None,
        check: None,
        references: None,
        foreign_key: None,
        on_delete: None,
        auto_now_add: false,
        auto_now: false,
    };

    for attribute in attributes {
        if !attribute.path().is_ident("orm") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("primary_key") {
                result.primary_key = true;
            } else if meta.path.is_ident("unique") {
                result.unique = true;
            } else if meta.path.is_ident("default") {
                result.default = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("check") {
                result.check = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("references") {
                let value: LitStr = meta.value()?.parse()?;
                validate_reference(&value.value(), value.span())?;
                result.references = Some(value);
            } else if meta.path.is_ident("foreign_key") {
                if result.references.is_some() {
                    return Err(meta.error("foreign_key and references are mutually exclusive"));
                }
                result.foreign_key = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("on_delete") {
                let value: LitStr = meta.value()?.parse()?;
                validate_on_delete(&value.value(), value.span())?;
                result.on_delete = Some(value);
            } else if meta.path.is_ident("auto_now_add") {
                result.auto_now_add = true;
            } else if meta.path.is_ident("auto_now") {
                result.auto_now = true;
            } else {
                return Err(meta.error("unsupported field attribute"));
            }
            Ok(())
        })?;
    }

    if result.auto_now && result.auto_now_add {
        return Err(Error::new_spanned(
            &attributes[0],
            "a field cannot use both auto_now and auto_now_add",
        ));
    }

    if result.references.is_some() && result.foreign_key.is_some() {
        return Err(Error::new_spanned(
            &attributes[0],
            "foreign_key and references are mutually exclusive",
        ));
    }

    Ok(result)
}

fn is_offset_date_time(ty: &syn::Type) -> bool {
    let syn::Type::Path(type_path) = ty else {
        return false;
    };
    type_path
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "OffsetDateTime")
}

fn is_i64(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(path) => path.qself.is_none() && path.path.is_ident("i64"),
        _ => false,
    }
}

fn validate_identifier(identifier: &str, span: proc_macro2::Span, kind: &str) -> syn::Result<()> {
    let mut chars = identifier.chars();
    let valid_start = chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic());
    if valid_start && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(Error::new(
            span,
            format!("invalid {kind} identifier: {identifier}"),
        ))
    }
}

fn validate_reference(reference: &str, span: proc_macro2::Span) -> syn::Result<()> {
    let Some((table, column)) = reference.split_once('(') else {
        return Err(Error::new(
            span,
            "references must use `table(column)` format",
        ));
    };
    let Some(column) = column.strip_suffix(')') else {
        return Err(Error::new(
            span,
            "references must use `table(column)` format",
        ));
    };
    validate_identifier(table, span, "referenced table")?;
    validate_identifier(column, span, "referenced column")
}

fn validate_on_delete(action: &str, span: proc_macro2::Span) -> syn::Result<()> {
    if matches!(
        action.to_ascii_lowercase().as_str(),
        "cascade" | "restrict" | "no action" | "set null" | "set default"
    ) {
        Ok(())
    } else {
        Err(Error::new(
            span,
            format!("unsupported on_delete action: {action}"),
        ))
    }
}
