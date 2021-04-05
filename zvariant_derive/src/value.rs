use proc_macro2::{Span, TokenStream};
use quote::{quote, ToTokens};
use syn::{
    self, Attribute, Data, DataEnum, DeriveInput, Expr, Fields, Generics, Ident, Lifetime,
    LifetimeDef,
};

pub enum ValueType {
    Value,
    OwnedValue,
}

pub fn expand_derive(ast: DeriveInput, value_type: ValueType) -> TokenStream {
    match ast.data {
        Data::Struct(ds) => match ds.fields {
            Fields::Named(_) | Fields::Unnamed(_) => {
                impl_struct(value_type, ast.ident, ast.generics, ds.fields)
            }
            Fields::Unit => panic!("Unit structures not supported"),
        },
        Data::Enum(data) => impl_enum(value_type, ast.ident, ast.generics, ast.attrs, data),
        _ => panic!("Only structures and enums supported at the moment"),
    }
}

fn impl_struct(
    value_type: ValueType,
    name: Ident,
    generics: Generics,
    fields: Fields,
) -> TokenStream {
    let statc_lifetime = LifetimeDef::new(Lifetime::new("'static", Span::call_site()));
    let (value_type, value_lifetime) = match value_type {
        ValueType::Value => {
            let mut lifetimes = generics.lifetimes();
            let value_lifetime = lifetimes
                .next()
                .cloned()
                .unwrap_or_else(|| statc_lifetime.clone());
            if lifetimes.next().is_some() {
                panic!("Type with more than 1 lifetime not supported");
            }

            (quote! { zvariant::Value<#value_lifetime> }, value_lifetime)
        }
        ValueType::OwnedValue => (quote! { zvariant::OwnedValue }, statc_lifetime),
    };

    let type_params = generics.type_params().cloned().collect::<Vec<_>>();
    let (from_value_where_clause, into_value_where_clause) = if !type_params.is_empty() {
        (
            Some(quote! {
                where
                #(
                    #type_params: std::convert::TryFrom<zvariant::Value<#value_lifetime>> + zvariant::Type
                ),*
            }),
            Some(quote! {
                where
                #(
                    #type_params: Into<zvariant::Value<#value_lifetime>> + zvariant::Type
                ),*
            }),
        )
    } else {
        (None, None)
    };
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let field_types: Vec<_> = fields
        .iter()
        .map(|field| field.ty.to_token_stream())
        .collect();
    match fields {
        Fields::Named(_) => {
            let field_names: Vec<_> = fields
                .iter()
                .map(|field| field.ident.to_token_stream())
                .collect();
            quote! {
                impl #impl_generics std::convert::TryFrom<#value_type> for #name #ty_generics
                    #from_value_where_clause
                {
                    type Error = zvariant::Error;

                    #[inline]
                    fn try_from(value: #value_type) -> zvariant::Result<Self> {
                        let mut fields = zvariant::Structure::try_from(value)?.into_fields();

                        Ok(Self {
                            #(
                                #field_names:
                                    fields
                                    .remove(0)
                                    .downcast()
                                    .ok_or_else(|| zvariant::Error::IncorrectType)?
                             ),*
                        })
                    }
                }

                impl #impl_generics From<#name #ty_generics> for #value_type
                    #into_value_where_clause
                {
                    #[inline]
                    fn from(s: #name #ty_generics) -> Self {
                        zvariant::StructureBuilder::new()
                        #(
                            .add_field(s.#field_names)
                        )*
                        .build()
                        .into()
                    }
                }
            }
        }
        Fields::Unnamed(_) if field_types.len() == 1 => {
            // Newtype struct.
            quote! {
                impl #impl_generics std::convert::TryFrom<#value_type> for #name #ty_generics
                    #from_value_where_clause
                {
                    type Error = zvariant::Error;

                    #[inline]
                    fn try_from(value: #value_type) -> zvariant::Result<Self> {
                        std::convert::TryInto::try_into(value).map(Self)
                    }
                }

                impl #impl_generics From<#name #ty_generics> for #value_type
                    #into_value_where_clause
                {
                    #[inline]
                    fn from(s: #name) -> Self {
                        s.0.into()
                    }
                }
            }
        }
        Fields::Unnamed(_) => panic!("impl_struct must not be called for tuples"),
        Fields::Unit => panic!("impl_struct must not be called for unit structures"),
    }
}

fn impl_enum(
    value_type: ValueType,
    name: Ident,
    _generics: Generics,
    attrs: Vec<Attribute>,
    data: DataEnum,
) -> TokenStream {
    let repr: TokenStream = match attrs.iter().find(|attr| attr.path.is_ident("repr")) {
        Some(repr_attr) => repr_attr
            .parse_args()
            .expect("Failed to parse `#[repr(...)]` attribute"),
        None => quote! { u32 },
    };

    let mut variant_names = vec![];
    let mut variant_values = vec![];
    for variant in data.variants {
        // Ensure all variants of the enum are unit type
        match variant.fields {
            Fields::Unit => {
                variant_names.push(variant.ident);
                let value = match variant
                    .discriminant
                    .expect("expected `Name = Value` variants")
                    .1
                {
                    Expr::Lit(lit_exp) => lit_exp.lit,
                    _ => panic!("expected `Name = Value` variants"),
                };
                variant_values.push(value);
            }
            _ => panic!("`{}` must be a unit variant", variant.ident.to_string()),
        }
    }

    let value_type = match value_type {
        ValueType::Value => quote! { zvariant::Value<'_> },
        ValueType::OwnedValue => quote! { zvariant::OwnedValue },
    };

    quote! {
        impl std::convert::TryFrom<#value_type> for #name {
            type Error = zvariant::Error;

            #[inline]
            fn try_from(value: #value_type) -> zvariant::Result<Self> {
                let v: #repr = std::convert::TryInto::try_into(value)?;

                Ok(match v {
                    #(
                        #variant_values => #name::#variant_names
                     ),*,
                    _ => return Err(zvariant::Error::IncorrectType),
                })
            }
        }

        impl std::convert::From<#name> for #value_type {
            #[inline]
            fn from(e: #name) -> Self {
                let u: #repr = match e {
                    #(
                        #name::#variant_names => #variant_values
                     ),*
                };

                zvariant::Value::from(u).into()
             }
        }
    }
}