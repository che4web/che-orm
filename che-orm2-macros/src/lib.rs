use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use quote::quote;
use std::collections::HashSet;
use syn::{
    Data, DeriveInput, Error, Fields, Ident, LitStr, Path, Token, parse_macro_input,
    spanned::Spanned,
};

fn orm_path() -> syn::Result<proc_macro2::TokenStream> {
    match crate_name("che-orm2") {
        Ok(FoundCrate::Itself) => Ok(quote!(crate)),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, proc_macro2::Span::call_site());
            Ok(quote!(::#ident))
        }
        Err(error) => Err(Error::new(
            proc_macro2::Span::call_site(),
            format!("could not resolve che-orm2 dependency: {error}"),
        )),
    }
}

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
    let orm = orm_path()?;
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
    let mut input_fields = Vec::new();
    let mut serializer_fields = Vec::new();
    for field in fields {
        let name = field.ident.expect("named fields always have identifiers");
        let json_name = name.to_string();
        let mut read_only = false;
        let mut write_only = false;
        serialize_fields.push(quote! {
            #orm::serde::ser::SerializeStruct::serialize_field(
                &mut state,
                #json_name,
                &self.#name,
            )?;
        });
        let mut nested_relation = None;
        let mut relation_path = None;
        for attribute in &field.attrs {
            if !attribute.path().is_ident("serializer") {
                continue;
            }
            attribute.parse_nested_meta(|meta| {
                    if meta.path.is_ident("read_only") {
                        read_only = true;
                        Ok(())
                    } else if meta.path.is_ident("write_only") {
                        write_only = true;
                        Ok(())
                    } else if meta.path.is_ident("many") || meta.path.is_ident("one") {
                        let many = meta.path.is_ident("many");
                        let model: Path = meta.value()?.parse()?;
                        if nested_relation.is_some() {
                            return Err(meta.error("a serializer field can have only one relation"));
                        }
                        nested_relation = Some((many, model));
                        Ok(())
                    } else if meta.path.is_ident("relation") {
                        relation_path = Some(meta.value()?.parse::<Path>()?);
                        Ok(())
                    } else {
                        Err(meta.error(
                            "unsupported serializer field attribute; expected read_only, many = Model, one = Model or relation = Model::RELATION",
                        ))
                    }
                })?;
        }
        if write_only {
            serialize_fields.pop();
        }
        serializer_fields.push(quote! {
            #orm::SerializerField {
                name: #json_name,
                read_only: #read_only,
                write_only: #write_only,
            }
        });
        if let Some((many, related_model)) = nested_relation {
            let relation_path = relation_path.ok_or_else(|| {
                Error::new_spanned(
                    &name,
                    "nested serializer fields require `relation = RelationMarker`",
                )
            })?;
            let marker = relation_marker(&relation_path)?;
            let optional = !many && is_option_type(&field.ty);
            let serializer_type = nested_serializer_type(&field.ty, many, optional)?;
            nested.push((name, many, related_model, marker, optional, serializer_type));
        } else {
            if !read_only {
                let constant = Ident::new(&name.to_string().to_uppercase(), name.span());
                input_fields.push((name.clone(), field.ty.clone(), constant));
            }
            assignments.push(quote! { #name: model.#name });
        }
    }
    let serializer_fields = quote! { &[#(#serializer_fields),*] };
    let has_nested = !nested.is_empty();
    let field_count = serialize_fields.len();
    let conversion = if nested.len() == 1 {
        let (name, many, _related_model, marker, optional, serializer_type) =
            nested.into_iter().next().unwrap();
        let child_input = quote! { <#serializer_type as #orm::ModelSerializer>::Input };
        let wrapper = if many {
            quote! { #orm::WithMany<#model, #child_input, #marker> }
        } else if optional {
            quote! { #orm::WithOptionalOne<#model, #child_input, #marker> }
        } else {
            quote! { #orm::WithOne<#model, #child_input, #marker> }
        };
        let loaded_wrapper = quote! {
            #orm::Loaded<#model, (#orm::LoadedMany<#child_input, #marker>,)>
        };
        let many_input = if many {
            loaded_wrapper.clone()
        } else {
            wrapper.clone()
        };
        let nested_assignment = if many {
            quote! { #name: value.related.into_iter().map(::core::convert::Into::into).collect() }
        } else if optional {
            quote! { #name: value.related.map(::core::convert::Into::into) }
        } else {
            quote! { #name: ::core::convert::Into::into(value.related) }
        };
        let loaded_conversion = if many {
            quote! {
                impl ::core::convert::From<#loaded_wrapper> for #serializer {
                    fn from(value: #loaded_wrapper) -> Self {
                        let model = value.model;
                        let (relation,) = value.relations;
                        Self {
                            #(#assignments,)*
                            #name: relation.related
                                .into_iter()
                                .map(::core::convert::Into::into)
                                .collect()
                        }
                    }
                }

                impl #orm::ModelSerializer for #serializer {
                    type Model = #model;
                    type Input = #loaded_wrapper;

                    fn from_input(value: Self::Input) -> Self {
                        <Self as ::core::convert::From<#loaded_wrapper>>::from(value)
                    }

                    fn fields() -> &'static [#orm::SerializerField] {
                        #serializer_fields
                    }
                }
            }
        } else {
            quote! {}
        };
        quote! {
            impl ::core::convert::From<#wrapper> for #serializer {
                fn from(value: #wrapper) -> Self {
                    let model = value.model;
                    Self { #(#assignments,)* #nested_assignment }
                }
            }

            #loaded_conversion

            impl #serializer {
                pub fn many<I>(values: I) -> ::std::vec::Vec<Self>
                where
                    I: ::core::iter::IntoIterator<Item = #many_input>,
                {
                    values.into_iter().map(::core::convert::Into::into).collect()
                }
            }
        }
    } else if nested.len() > 1 {
        let mut wrappers = Vec::new();
        let mut relation_vars = Vec::new();
        let mut nested_assignments = Vec::new();
        for (index, (name, many, _related_model, marker, optional, serializer_type)) in
            nested.into_iter().enumerate()
        {
            if !many || optional {
                return Err(Error::new_spanned(
                    name,
                    "multiple nested fields currently support only `many` relations",
                ));
            }
            let relation_var = Ident::new(
                &format!("__relation{index}"),
                proc_macro2::Span::call_site(),
            );
            relation_vars.push(relation_var.clone());
            let child_input = quote! { <#serializer_type as #orm::ModelSerializer>::Input };
            wrappers.push(quote! { #orm::LoadedMany<#child_input, #marker> });
            nested_assignments.push(quote! {
                #name: #relation_var.related
                    .into_iter()
                    .map(::core::convert::Into::into)
                    .collect()
            });
        }
        let loaded_type = quote! {
            #orm::Loaded<#model, (#(#wrappers),*,)>
        };
        let destructure = quote! { let (#(#relation_vars),*,) = value.relations; };
        quote! {
            impl ::core::convert::From<#loaded_type> for #serializer {
                fn from(value: #loaded_type) -> Self {
                    let model = value.model;
                    #destructure
                    Self { #(#assignments,)* #(#nested_assignments),* }
                }
            }

            impl #orm::ModelSerializer for #serializer {
                type Model = #model;
                type Input = #loaded_type;

                fn from_input(value: Self::Input) -> Self {
                    <Self as ::core::convert::From<#loaded_type>>::from(value)
                }

                fn fields() -> &'static [#orm::SerializerField] {
                    #serializer_fields
                }
            }

            impl #serializer {
                pub fn many<I>(values: I) -> ::std::vec::Vec<Self>
                where
                    I: ::core::iter::IntoIterator<Item = #loaded_type>,
                {
                    values.into_iter().map(::core::convert::Into::into).collect()
                }
            }
        }
    } else {
        quote! {
        impl ::core::convert::From<#model> for #serializer {
                fn from(model: #model) -> Self {
                    <Self as #orm::ModelSerializer>::from_input(model)
                }
            }

            impl #serializer {
                pub fn from_model(model: #model) -> Self {
                    <Self as #orm::ModelSerializer>::from_input(model)
                }

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
            impl #orm::ModelSerializer for #serializer {
                type Model = #model;
                type Input = #model;

                fn from_input(model: Self::Input) -> Self {
                    Self { #(#assignments),* }
                }

                fn fields() -> &'static [#orm::SerializerField] {
                    #serializer_fields
                }
            }
        }
    };

    let create_name = Ident::new(&format!("{}CreateInput", serializer), serializer.span());
    let update_name = Ident::new(&format!("{}UpdateInput", serializer), serializer.span());
    let patch_name = Ident::new(&format!("{}PatchInput", serializer), serializer.span());
    let input_names = input_fields
        .iter()
        .map(|(name, _, _)| name.clone())
        .collect::<Vec<_>>();
    let input_types = input_fields
        .iter()
        .map(|(_, ty, _)| ty.clone())
        .collect::<Vec<_>>();
    let create_sets = input_fields.iter().map(|(name, _, constant)| {
        quote! {
            let builder = builder.set(<#model>::#constant, input.#name);
        }
    });
    let update_sets = input_fields.iter().map(|(name, _, constant)| {
        quote! {
            let builder = builder.set(<#model>::#constant, input.#name);
        }
    });
    let patch_sets = input_fields.iter().map(|(name, _, constant)| {
        quote! {
            let builder = match input.#name {
                #orm::PatchField::Missing => builder,
                #orm::PatchField::Value(value) => builder.set(<#model>::#constant, value),
            };
        }
    });
    let patch_missing_checks = input_fields
        .iter()
        .map(|(name, _, _)| quote! { input.#name.is_missing() })
        .collect::<Vec<_>>();
    let patch_empty = if patch_missing_checks.is_empty() {
        quote! { true }
    } else {
        quote! { #(#patch_missing_checks)&&* }
    };

    Ok(quote! {
        #[derive(Debug, #orm::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct #create_name {
            #(pub #input_names: #input_types,)*
        }

        #[derive(Debug, #orm::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct #update_name {
            #(pub #input_names: #input_types,)*
        }

        #[derive(Debug, #orm::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct #patch_name {
            #(#[serde(default)] pub #input_names: #orm::PatchField<#input_types>,)*
        }

        #[cfg(feature = "sqlite")]
        impl #serializer {
            pub async fn create(
                database: &#orm::Database,
                input: #create_name,
            ) -> ::core::result::Result<#model, #orm::OrmError>
            where
                #model: Send + 'static,
            {
                let builder = database.create::<#model>();
                #(#create_sets)*
                builder.execute().await
            }

            pub async fn update(
                database: &#orm::Database,
                id: i64,
                input: #update_name,
            ) -> ::core::result::Result<Option<#model>, #orm::OrmError>
            where
                #model: Send + 'static,
            {
                let builder = database.update::<#model>(id);
                #(#update_sets)*
                builder.execute().await
            }

            pub async fn patch(
                database: &#orm::Database,
                id: i64,
                input: #patch_name,
            ) -> ::core::result::Result<Option<#model>, #orm::OrmError>
            where
                #model: Send + 'static,
            {
                if #patch_empty {
                    return Err(#orm::OrmError::QueryBuild(#orm::QueryBuildError::EmptyUpdate));
                }
                let builder = database.update::<#model>(id);
                #(#patch_sets)*
                builder.execute().await
            }
        }

        #[cfg(feature = "sqlite")]
        impl #orm::ModelWriteSerializer for #serializer {
            type Model = #model;
            type CreateInput = #create_name;
            type UpdateInput = #update_name;
            type PatchInput = #patch_name;

            fn create<'a>(database: &'a #orm::Database, input: Self::CreateInput)
                -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ::core::result::Result<Self::Model, #orm::OrmError>> + Send + 'a>>
            {
                Box::pin(Self::create(database, input))
            }

            fn update<'a>(database: &'a #orm::Database, id: i64, input: Self::UpdateInput)
                -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ::core::result::Result<Option<Self::Model>, #orm::OrmError>> + Send + 'a>>
            {
                Box::pin(Self::update(database, id, input))
            }

            fn patch<'a>(database: &'a #orm::Database, id: i64, input: Self::PatchInput)
                -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ::core::result::Result<Option<Self::Model>, #orm::OrmError>> + Send + 'a>>
            {
                Box::pin(Self::patch(database, id, input))
            }
        }

        impl #orm::serde::Serialize for #serializer {
            fn serialize<__Serializer>(
                &self,
                serializer: __Serializer,
            ) -> ::core::result::Result<__Serializer::Ok, __Serializer::Error>
            where
                __Serializer: #orm::serde::Serializer,
            {
                let mut state = #orm::serde::Serializer::serialize_struct(
                    serializer,
                    stringify!(#serializer),
                    #field_count,
                )?;
                #(#serialize_fields)*
                #orm::serde::ser::SerializeStruct::end(state)
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
                if model.is_some() {
                    return Err(meta.error("serializer model is declared more than once"));
                }
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

fn relation_marker(path: &Path) -> syn::Result<Ident> {
    let mut segments = path.segments.iter();
    let model = segments
        .next()
        .ok_or_else(|| Error::new_spanned(path, "relation must use `RelationMarker`"))?;
    if segments.clone().next().is_none() {
        return Ok(model.ident.clone());
    }
    let relation = segments
        .next()
        .ok_or_else(|| Error::new_spanned(path, "relation must use `RelationMarker`"))?;
    if segments.next().is_some() {
        return Err(Error::new_spanned(
            path,
            "relation must use `RelationMarker`",
        ));
    }
    Ok(Ident::new(
        &format!("{}{}Relation", model.ident, relation.ident),
        path.span(),
    ))
}

fn is_option_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Option")
}

fn nested_serializer_type(ty: &syn::Type, many: bool, optional: bool) -> syn::Result<Path> {
    let inner = if many {
        let syn::Type::Path(path) = ty else {
            return Err(Error::new_spanned(
                ty,
                "many serializer field must be Vec<Serializer>",
            ));
        };
        let segment = path.path.segments.last().ok_or_else(|| {
            Error::new_spanned(ty, "many serializer field must be Vec<Serializer>")
        })?;
        if segment.ident != "Vec" {
            return Err(Error::new_spanned(
                ty,
                "many serializer field must be Vec<Serializer>",
            ));
        }
        let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
            return Err(Error::new_spanned(
                ty,
                "many serializer field must be Vec<Serializer>",
            ));
        };
        arguments.args.first().cloned()
    } else if optional {
        let syn::Type::Path(path) = ty else {
            return Err(Error::new_spanned(
                ty,
                "optional relation must be Option<Serializer>",
            ));
        };
        let segment = path.path.segments.last().ok_or_else(|| {
            Error::new_spanned(ty, "optional relation must be Option<Serializer>")
        })?;
        let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
            return Err(Error::new_spanned(
                ty,
                "optional relation must be Option<Serializer>",
            ));
        };
        arguments.args.first().cloned()
    } else {
        match ty {
            syn::Type::Path(_) => Some(syn::GenericArgument::Type(ty.clone())),
            _ => None,
        }
    };
    match inner {
        Some(syn::GenericArgument::Type(syn::Type::Path(path))) => Ok(path.path),
        _ => Err(Error::new_spanned(
            ty,
            "nested serializer field must contain a serializer type",
        )),
    }
}

fn pascal_case(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_ascii_uppercase().to_string() + chars.as_str()
}

fn derive_model_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let orm = orm_path()?;
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
    let mut clone_fields = Vec::with_capacity(fields.len());
    let mut relation_constants = Vec::new();
    let mut relation_markers = Vec::new();
    let mut primary_key_seen = false;
    let mut primary_key_constant = None;
    let mut primary_key_field = None;
    let mut generated_constants = HashSet::new();

    for (index, field) in fields.into_iter().enumerate() {
        let field_name = field.ident.expect("named fields always have identifiers");
        let column = field_name.to_string();
        let constant = Ident::new(&column.to_uppercase(), field_name.span());
        let field_type = field.ty;
        let field_attributes = parse_field_attributes(&field.attrs)?;

        if !generated_constants.insert(constant.to_string()) {
            return Err(Error::new_spanned(
                &field_name,
                format!("generated constant {constant} conflicts with another model field"),
            ));
        }

        clone_fields.push(quote! {
            #field_name: self.#field_name.clone(),
        });

        if field_attributes.foreign_key.is_some()
            && !is_i64(&field_type)
            && !is_option_i64(&field_type)
        {
            return Err(Error::new_spanned(
                &field_type,
                "foreign_key currently requires an i64 field",
            ));
        }

        if field_attributes.foreign_key.is_some()
            && field_attributes
                .on_delete
                .as_ref()
                .is_some_and(|action| action.value().eq_ignore_ascii_case("set null"))
            && !is_option_i64(&field_type)
        {
            return Err(Error::new_spanned(
                &field_type,
                "on_delete = \"set null\" requires an Option<i64> foreign_key field",
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
            pub const #constant: #orm::ModelField<Self, #field_type> =
                #orm::ModelField::new(#table, #column);
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
                column.references = Some(#orm::ForeignKey::new(
                    #target,
                    #on_delete,
                ));
            }
        });
        let foreign_key = field_attributes.foreign_key.as_ref().map(|target| {
            let on_delete = field_attributes
                .on_delete
                .clone()
                .map(|value| quote! { Some(#value) })
                .unwrap_or_else(|| quote! { None });
            quote! {
                column.references = Some(#orm::ForeignKey::new(
                    format!(
                        "{}({})",
                        <#target as #orm::Model>::table_name(),
                        <#target as #orm::Model>::primary_key().column().name,
                    ),
                    #on_delete,
                ));
            }
        });
        if let Some(target) = &field_attributes.foreign_key {
            let relation_name = column.strip_suffix("_id").unwrap_or(&column).to_uppercase();
            let alias = column
                .strip_suffix("_id")
                .unwrap_or(&column)
                .to_ascii_lowercase();
            let relation_constant = Ident::new(&relation_name, field_name.span());
            let marker_name = Ident::new(
                &format!(
                    "{}{}Relation",
                    model,
                    pascal_case(column.strip_suffix("_id").unwrap_or(&column))
                ),
                field_name.span(),
            );
            if !generated_constants.insert(relation_name.clone()) {
                return Err(Error::new_spanned(
                    &field_name,
                    format!(
                        "generated relation constant {relation_name} conflicts with a model field"
                    ),
                ));
            }
            let reverse_name = format!("{}_set", model.to_string().to_lowercase());
            relation_constants.push(quote! {
                pub const #relation_constant:
                    #orm::BelongsTo<Self, #target, #marker_name, #field_type> =
                    #orm::BelongsTo::new(
                        #table,
                        #column,
                        |model: &Self| model.#field_name,
                        #alias,
                        #reverse_name,
                    );
            });
            relation_markers.push(quote! { pub struct #marker_name; });
        }

        schema_columns.push(quote! {
            let mut column = #orm::ColumnSchema::new(
                #column,
                <#field_type as #orm::ColumnTypeOf>::column_type(),
                <#field_type as #orm::ColumnTypeOf>::nullable(),
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
            #field_name: row.get(#index + offset)?,
        });

        if !field_attributes.primary_key && !auto_now_add && !auto_now {
            insert_values.push(quote! {
                #orm::InsertValue {
                    column: #orm::ColumnRef::new(#table, #column),
                    value: #orm::QueryValue::<#field_type>::into_query_value(
                        self.#field_name.clone(),
                    ),
                }
            });
        }

        if auto_now {
            managed_update_values.push(quote! {
                #orm::Assignment {
                    column: #orm::ColumnRef::new(#table, #column),
                    value: #orm::QueryValue::<#field_type>::into_query_value(
                        #orm::time::OffsetDateTime::now_utc(),
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
        #(#relation_markers)*

        impl #orm::Model for #model {
            fn table_name() -> &'static str {
                #table
            }

            fn columns() -> &'static [&'static str] {
                &[#(#columns),*]
            }

            fn primary_key() -> #orm::ModelField<Self, i64> {
                Self::#primary_key_constant
            }

            fn primary_key_value(&self) -> i64 {
                self.#primary_key_field
            }

            fn schema() -> #orm::TableSchema {
                let mut columns = Vec::new();
                #(#schema_columns)*
                #orm::TableSchema {
                    name: #table,
                    columns,
                    unique_constraints: vec![#(#unique_constraints),*],
                    indexes: vec![#(#indexes),*],
                }
            }

            fn from_row(row: &#orm::rusqlite::Row<'_>) -> #orm::rusqlite::Result<Self> {
                Self::from_row_at(row, 0)
            }

            fn from_row_at(
                row: &#orm::rusqlite::Row<'_>,
                offset: usize,
            ) -> #orm::rusqlite::Result<Self> {
                Ok(Self {
                    #(#row_fields)*
                })
            }

            fn insert_values(&self) -> ::std::vec::Vec<#orm::InsertValue> {
                vec![#(#insert_values),*]
            }

            fn managed_update_values() -> ::std::vec::Vec<#orm::Assignment> {
                vec![#(#managed_update_values),*]
            }
        }

        impl ::core::clone::Clone for #model {
            fn clone(&self) -> Self {
                Self {
                    #(#clone_fields)*
                }
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

fn is_option_i64(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    if segment.ident != "Option" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return false;
    };
    let Some(syn::GenericArgument::Type(inner)) = arguments.args.first() else {
        return false;
    };
    is_i64(inner)
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
