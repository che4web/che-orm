use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, Ident, LitStr, Token, parse_macro_input};

#[proc_macro_derive(Model, attributes(orm))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    match derive_model_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
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
    let mut primary_key_seen = false;

    for (index, field) in fields.into_iter().enumerate() {
        let field_name = field.ident.expect("named fields always have identifiers");
        let column = field_name.to_string();
        let constant = Ident::new(&column.to_uppercase(), field_name.span());
        let field_type = field.ty;
        let field_attributes = parse_field_attributes(&field.attrs)?;

        if field_attributes.primary_key {
            if primary_key_seen {
                return Err(Error::new_spanned(
                    &field_name,
                    "a model can have only one primary_key field",
                ));
            }
            primary_key_seen = true;
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
                    target: #target,
                    on_delete: #on_delete,
                });
            }
        });

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
