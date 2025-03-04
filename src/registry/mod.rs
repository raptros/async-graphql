mod cache_control;
mod export_sdl;
mod stringify_exec_doc;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::{self, Display, Formatter},
};

pub use cache_control::CacheControl;
pub use export_sdl::SDLExportOptions;
use indexmap::{map::IndexMap, set::IndexSet};

pub use crate::model::__DirectiveLocation;
use crate::{
    parser::types::{BaseType as ParsedBaseType, Field, Type as ParsedType, VariableDefinition},
    schema::IntrospectionMode,
    Any, Context, InputType, OutputType, Positioned, ServerResult, SubscriptionType, Value,
    VisitorContext,
};

fn strip_brackets(type_name: &str) -> Option<&str> {
    type_name
        .strip_prefix('[')
        .map(|rest| &rest[..rest.len() - 1])
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum MetaTypeName<'a> {
    List(&'a str),
    NonNull(&'a str),
    Named(&'a str),
}

impl<'a> Display for MetaTypeName<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            MetaTypeName::Named(name) => write!(f, "{}", name),
            MetaTypeName::NonNull(name) => write!(f, "{}!", name),
            MetaTypeName::List(name) => write!(f, "[{}]", name),
        }
    }
}

impl<'a> MetaTypeName<'a> {
    #[inline]
    pub fn create(type_name: &str) -> MetaTypeName {
        if let Some(type_name) = type_name.strip_suffix('!') {
            MetaTypeName::NonNull(type_name)
        } else if let Some(type_name) = strip_brackets(type_name) {
            MetaTypeName::List(type_name)
        } else {
            MetaTypeName::Named(type_name)
        }
    }

    #[inline]
    pub fn concrete_typename(type_name: &str) -> &str {
        match MetaTypeName::create(type_name) {
            MetaTypeName::List(type_name) => Self::concrete_typename(type_name),
            MetaTypeName::NonNull(type_name) => Self::concrete_typename(type_name),
            MetaTypeName::Named(type_name) => type_name,
        }
    }

    #[inline]
    pub fn is_non_null(&self) -> bool {
        matches!(self, MetaTypeName::NonNull(_))
    }

    #[inline]
    #[must_use]
    pub fn unwrap_non_null(&self) -> Self {
        match self {
            MetaTypeName::NonNull(ty) => MetaTypeName::create(ty),
            _ => *self,
        }
    }

    #[inline]
    pub fn is_subtype(&self, sub: &MetaTypeName<'_>) -> bool {
        match (self, sub) {
            (MetaTypeName::NonNull(super_type), MetaTypeName::NonNull(sub_type))
            | (MetaTypeName::Named(super_type), MetaTypeName::NonNull(sub_type)) => {
                MetaTypeName::create(super_type).is_subtype(&MetaTypeName::create(sub_type))
            }
            (MetaTypeName::Named(super_type), MetaTypeName::Named(sub_type)) => {
                super_type == sub_type
            }
            (MetaTypeName::List(super_type), MetaTypeName::List(sub_type)) => {
                MetaTypeName::create(super_type).is_subtype(&MetaTypeName::create(sub_type))
            }
            _ => false,
        }
    }

    #[inline]
    pub fn is_list(&self) -> bool {
        match self {
            MetaTypeName::List(_) => true,
            MetaTypeName::NonNull(ty) => MetaTypeName::create(ty).is_list(),
            MetaTypeName::Named(name) => name.ends_with(']'),
        }
    }
}

#[derive(Clone)]
pub struct MetaInputValue {
    pub name: &'static str,
    pub description: Option<&'static str>,
    pub ty: String,
    pub default_value: Option<String>,
    pub visible: Option<MetaVisibleFn>,
    pub inaccessible: bool,
    pub tags: &'static [&'static str],
    pub is_secret: bool,
}

type ComputeComplexityFn = fn(
    &VisitorContext<'_>,
    &[Positioned<VariableDefinition>],
    &Field,
    usize,
) -> ServerResult<usize>;

#[derive(Clone)]
pub enum ComplexityType {
    Const(usize),
    Fn(ComputeComplexityFn),
}

#[derive(Debug, Clone)]
pub enum Deprecation {
    NoDeprecated,
    Deprecated { reason: Option<&'static str> },
}

impl Default for Deprecation {
    fn default() -> Self {
        Deprecation::NoDeprecated
    }
}

impl Deprecation {
    #[inline]
    pub fn is_deprecated(&self) -> bool {
        matches!(self, Deprecation::Deprecated { .. })
    }

    #[inline]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Deprecation::NoDeprecated => None,
            Deprecation::Deprecated { reason } => reason.as_deref(),
        }
    }
}

#[derive(Clone)]
pub struct MetaField {
    pub name: String,
    pub description: Option<&'static str>,
    pub args: IndexMap<String, MetaInputValue>,
    pub ty: String,
    pub deprecation: Deprecation,
    pub cache_control: CacheControl,
    pub external: bool,
    pub requires: Option<&'static str>,
    pub provides: Option<&'static str>,
    pub visible: Option<MetaVisibleFn>,
    pub shareable: bool,
    pub inaccessible: bool,
    pub tags: &'static [&'static str],
    pub override_from: Option<&'static str>,
    pub compute_complexity: Option<ComplexityType>,
}

#[derive(Clone)]
pub struct MetaEnumValue {
    pub name: &'static str,
    pub description: Option<&'static str>,
    pub deprecation: Deprecation,
    pub visible: Option<MetaVisibleFn>,
    pub inaccessible: bool,
    pub tags: &'static [&'static str],
}

type MetaVisibleFn = fn(&Context<'_>) -> bool;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MetaTypeId {
    Scalar,
    Object,
    Interface,
    Union,
    Enum,
    InputObject,
}

impl Display for MetaTypeId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MetaTypeId::Scalar => "Scalar",
            MetaTypeId::Object => "Object",
            MetaTypeId::Interface => "Interface",
            MetaTypeId::Union => "Union",
            MetaTypeId::Enum => "Enum",
            MetaTypeId::InputObject => "InputObject",
        })
    }
}

#[derive(Clone)]
pub enum MetaType {
    Scalar {
        name: String,
        description: Option<&'static str>,
        is_valid: fn(value: &Value) -> bool,
        visible: Option<MetaVisibleFn>,
        inaccessible: bool,
        tags: &'static [&'static str],
        specified_by_url: Option<&'static str>,
    },
    Object {
        name: String,
        description: Option<&'static str>,
        fields: IndexMap<String, MetaField>,
        cache_control: CacheControl,
        extends: bool,
        shareable: bool,
        keys: Option<Vec<String>>,
        visible: Option<MetaVisibleFn>,
        inaccessible: bool,
        tags: &'static [&'static str],
        is_subscription: bool,
        rust_typename: &'static str,
    },
    Interface {
        name: String,
        description: Option<&'static str>,
        fields: IndexMap<String, MetaField>,
        possible_types: IndexSet<String>,
        extends: bool,
        keys: Option<Vec<String>>,
        visible: Option<MetaVisibleFn>,
        inaccessible: bool,
        tags: &'static [&'static str],
        rust_typename: &'static str,
    },
    Union {
        name: String,
        description: Option<&'static str>,
        possible_types: IndexSet<String>,
        visible: Option<MetaVisibleFn>,
        inaccessible: bool,
        tags: &'static [&'static str],
        rust_typename: &'static str,
    },
    Enum {
        name: String,
        description: Option<&'static str>,
        enum_values: IndexMap<&'static str, MetaEnumValue>,
        visible: Option<MetaVisibleFn>,
        inaccessible: bool,
        tags: &'static [&'static str],
        rust_typename: &'static str,
    },
    InputObject {
        name: String,
        description: Option<&'static str>,
        input_fields: IndexMap<String, MetaInputValue>,
        visible: Option<MetaVisibleFn>,
        inaccessible: bool,
        tags: &'static [&'static str],
        rust_typename: &'static str,
        oneof: bool,
    },
}

impl MetaType {
    #[inline]
    pub fn type_id(&self) -> MetaTypeId {
        match self {
            MetaType::Scalar { .. } => MetaTypeId::Scalar,
            MetaType::Object { .. } => MetaTypeId::Object,
            MetaType::Interface { .. } => MetaTypeId::Interface,
            MetaType::Union { .. } => MetaTypeId::Union,
            MetaType::Enum { .. } => MetaTypeId::Enum,
            MetaType::InputObject { .. } => MetaTypeId::InputObject,
        }
    }

    #[inline]
    pub fn field_by_name(&self, name: &str) -> Option<&MetaField> {
        self.fields().and_then(|fields| fields.get(name))
    }

    #[inline]
    pub fn fields(&self) -> Option<&IndexMap<String, MetaField>> {
        match self {
            MetaType::Object { fields, .. } => Some(&fields),
            MetaType::Interface { fields, .. } => Some(&fields),
            _ => None,
        }
    }

    #[inline]
    pub fn is_visible(&self, ctx: &Context<'_>) -> bool {
        let visible = match self {
            MetaType::Scalar { visible, .. } => visible,
            MetaType::Object { visible, .. } => visible,
            MetaType::Interface { visible, .. } => visible,
            MetaType::Union { visible, .. } => visible,
            MetaType::Enum { visible, .. } => visible,
            MetaType::InputObject { visible, .. } => visible,
        };
        is_visible(ctx, visible)
    }

    #[inline]
    pub fn name(&self) -> &str {
        match self {
            MetaType::Scalar { name, .. } => &name,
            MetaType::Object { name, .. } => name,
            MetaType::Interface { name, .. } => name,
            MetaType::Union { name, .. } => name,
            MetaType::Enum { name, .. } => name,
            MetaType::InputObject { name, .. } => name,
        }
    }

    #[inline]
    pub fn is_composite(&self) -> bool {
        matches!(
            self,
            MetaType::Object { .. } | MetaType::Interface { .. } | MetaType::Union { .. }
        )
    }

    #[inline]
    pub fn is_abstract(&self) -> bool {
        matches!(self, MetaType::Interface { .. } | MetaType::Union { .. })
    }

    #[inline]
    pub fn is_leaf(&self) -> bool {
        matches!(self, MetaType::Enum { .. } | MetaType::Scalar { .. })
    }

    #[inline]
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            MetaType::Enum { .. } | MetaType::Scalar { .. } | MetaType::InputObject { .. }
        )
    }

    #[inline]
    pub fn is_possible_type(&self, type_name: &str) -> bool {
        match self {
            MetaType::Interface { possible_types, .. } => possible_types.contains(type_name),
            MetaType::Union { possible_types, .. } => possible_types.contains(type_name),
            MetaType::Object { name, .. } => name == type_name,
            _ => false,
        }
    }

    #[inline]
    pub fn possible_types(&self) -> Option<&IndexSet<String>> {
        match self {
            MetaType::Interface { possible_types, .. } => Some(possible_types),
            MetaType::Union { possible_types, .. } => Some(possible_types),
            _ => None,
        }
    }

    pub fn type_overlap(&self, ty: &MetaType) -> bool {
        if std::ptr::eq(self, ty) {
            return true;
        }

        match (self.is_abstract(), ty.is_abstract()) {
            (true, true) => self
                .possible_types()
                .iter()
                .copied()
                .flatten()
                .any(|type_name| ty.is_possible_type(type_name)),
            (true, false) => self.is_possible_type(ty.name()),
            (false, true) => ty.is_possible_type(self.name()),
            (false, false) => false,
        }
    }

    pub fn rust_typename(&self) -> Option<&'static str> {
        match self {
            MetaType::Scalar { .. } => None,
            MetaType::Object { rust_typename, .. } => Some(rust_typename),
            MetaType::Interface { rust_typename, .. } => Some(rust_typename),
            MetaType::Union { rust_typename, .. } => Some(rust_typename),
            MetaType::Enum { rust_typename, .. } => Some(rust_typename),
            MetaType::InputObject { rust_typename, .. } => Some(rust_typename),
        }
    }
}

pub struct MetaDirective {
    pub name: &'static str,
    pub description: Option<&'static str>,
    pub locations: Vec<__DirectiveLocation>,
    pub args: IndexMap<String, MetaInputValue>,
    pub is_repeatable: bool,
    pub visible: Option<MetaVisibleFn>,
}

#[derive(Default)]
pub struct Registry {
    pub types: BTreeMap<String, MetaType>,
    pub directives: HashMap<String, MetaDirective>,
    pub implements: HashMap<String, HashSet<String>>,
    pub query_type: String,
    pub mutation_type: Option<String>,
    pub subscription_type: Option<String>,
    pub introspection_mode: IntrospectionMode,
    pub enable_federation: bool,
    pub enable_apollo_link: bool,
    pub federation_subscription: bool,
    pub ignore_name_conflicts: HashSet<String>,
}

impl Registry {
    pub fn create_input_type<T, F>(&mut self, type_id: MetaTypeId, mut f: F) -> String
    where
        T: InputType + ?Sized,
        F: FnMut(&mut Registry) -> MetaType,
    {
        self.create_type(
            &mut f,
            &*T::type_name(),
            std::any::type_name::<T>(),
            type_id,
        );
        T::qualified_type_name()
    }

    pub fn create_output_type<T, F>(&mut self, type_id: MetaTypeId, mut f: F) -> String
    where
        T: OutputType + ?Sized,
        F: FnMut(&mut Registry) -> MetaType,
    {
        self.create_type(
            &mut f,
            &*T::type_name(),
            std::any::type_name::<T>(),
            type_id,
        );
        T::qualified_type_name()
    }

    pub fn create_subscription_type<T, F>(&mut self, mut f: F) -> String
    where
        T: SubscriptionType + ?Sized,
        F: FnMut(&mut Registry) -> MetaType,
    {
        self.create_type(
            &mut f,
            &*T::type_name(),
            std::any::type_name::<T>(),
            MetaTypeId::Object,
        );
        T::qualified_type_name()
    }

    fn create_type<F: FnMut(&mut Registry) -> MetaType>(
        &mut self,
        f: &mut F,
        name: &str,
        rust_typename: &str,
        type_id: MetaTypeId,
    ) {
        match self.types.get(name) {
            Some(ty) => {
                if let Some(prev_typename) = ty.rust_typename() {
                    if prev_typename == "__fake_type__" {
                        return;
                    }

                    if rust_typename != prev_typename && !self.ignore_name_conflicts.contains(name)
                    {
                        panic!(
                            "`{}` and `{}` have the same GraphQL name `{}`",
                            prev_typename, rust_typename, name,
                        );
                    }

                    if ty.type_id() != type_id {
                        panic!(
                            "Register `{}` as `{}`, but it is already registered as `{}`",
                            name,
                            type_id,
                            ty.type_id()
                        );
                    }
                }
            }
            None => {
                // Inserting a fake type before calling the function allows recursive types to
                // exist.
                self.types.insert(
                    name.to_string(),
                    MetaType::Object {
                        name: "".to_string(),
                        description: None,
                        fields: Default::default(),
                        cache_control: Default::default(),
                        extends: false,
                        shareable: false,
                        inaccessible: false,
                        tags: Default::default(),
                        keys: None,
                        visible: None,
                        is_subscription: false,
                        rust_typename: "__fake_type__",
                    },
                );
                let ty = f(self);
                *self.types.get_mut(name).unwrap() = ty;
            }
        }
    }

    pub fn create_fake_output_type<T: OutputType>(&mut self) -> MetaType {
        T::create_type_info(self);
        self.types
            .get(&*T::type_name())
            .cloned()
            .expect("You definitely encountered a bug!")
    }

    pub fn create_fake_input_type<T: InputType>(&mut self) -> MetaType {
        T::create_type_info(self);
        self.types
            .get(&*T::type_name())
            .cloned()
            .expect("You definitely encountered a bug!")
    }

    pub fn create_fake_subscription_type<T: SubscriptionType>(&mut self) -> MetaType {
        T::create_type_info(self);
        self.types
            .get(&*T::type_name())
            .cloned()
            .expect("You definitely encountered a bug!")
    }

    pub fn add_directive(&mut self, directive: MetaDirective) {
        self.directives
            .insert(directive.name.to_string(), directive);
    }

    pub fn add_implements(&mut self, ty: &str, interface: &str) {
        self.implements
            .entry(ty.to_string())
            .and_modify(|interfaces| {
                interfaces.insert(interface.to_string());
            })
            .or_insert({
                let mut interfaces = HashSet::new();
                interfaces.insert(interface.to_string());
                interfaces
            });
    }

    pub fn add_keys(&mut self, ty: &str, keys: &str) {
        let all_keys = match self.types.get_mut(ty) {
            Some(MetaType::Object { keys: all_keys, .. }) => all_keys,
            Some(MetaType::Interface { keys: all_keys, .. }) => all_keys,
            _ => return,
        };
        if let Some(all_keys) = all_keys {
            all_keys.push(keys.to_string());
        } else {
            *all_keys = Some(vec![keys.to_string()]);
        }
    }

    pub fn concrete_type_by_name(&self, type_name: &str) -> Option<&MetaType> {
        self.types.get(MetaTypeName::concrete_typename(type_name))
    }

    pub fn concrete_type_by_parsed_type(&self, query_type: &ParsedType) -> Option<&MetaType> {
        match &query_type.base {
            ParsedBaseType::Named(name) => self.types.get(name.as_str()),
            ParsedBaseType::List(ty) => self.concrete_type_by_parsed_type(ty),
        }
    }

    pub(crate) fn has_entities(&self) -> bool {
        self.types.values().any(|ty| match ty {
            MetaType::Object {
                keys: Some(keys), ..
            }
            | MetaType::Interface {
                keys: Some(keys), ..
            } => !keys.is_empty(),
            _ => false,
        })
    }

    /// Each type annotated with @key should be added to the _Entity union.
    /// If no types are annotated with the key directive, then the _Entity union
    /// and Query._entities field should be removed from the schema.
    ///
    /// [Reference](https://www.apollographql.com/docs/federation/federation-spec/#resolve-requests-for-entities).
    fn create_entity_type_and_root_field(&mut self) {
        let possible_types: IndexSet<String> = self
            .types
            .values()
            .filter_map(|ty| match ty {
                MetaType::Object {
                    name,
                    keys: Some(keys),
                    ..
                } if !keys.is_empty() => Some(name.clone()),
                MetaType::Interface {
                    name,
                    keys: Some(keys),
                    ..
                } if !keys.is_empty() => Some(name.clone()),
                _ => None,
            })
            .collect();

        if let MetaType::Object { fields, .. } = self.types.get_mut(&self.query_type).unwrap() {
            fields.insert(
                "_service".to_string(),
                MetaField {
                    name: "_service".to_string(),
                    description: None,
                    args: Default::default(),
                    ty: "_Service!".to_string(),
                    deprecation: Default::default(),
                    cache_control: Default::default(),
                    external: false,
                    requires: None,
                    provides: None,
                    shareable: false,
                    inaccessible: false,
                    tags: Default::default(),
                    override_from: None,
                    visible: None,
                    compute_complexity: None,
                },
            );
        }

        if !possible_types.is_empty() {
            self.types.insert(
                "_Entity".to_string(),
                MetaType::Union {
                    name: "_Entity".to_string(),
                    description: None,
                    possible_types,
                    visible: None,
                    inaccessible: false,
                    tags: Default::default(),
                    rust_typename: "async_graphql::federation::Entity",
                },
            );

            if let MetaType::Object { fields, .. } = self.types.get_mut(&self.query_type).unwrap() {
                fields.insert(
                    "_entities".to_string(),
                    MetaField {
                        name: "_entities".to_string(),
                        description: None,
                        args: {
                            let mut args = IndexMap::new();
                            args.insert(
                                "representations".to_string(),
                                MetaInputValue {
                                    name: "representations",
                                    description: None,
                                    ty: "[_Any!]!".to_string(),
                                    default_value: None,
                                    visible: None,
                                    inaccessible: false,
                                    tags: Default::default(),
                                    is_secret: false,
                                },
                            );
                            args
                        },
                        ty: "[_Entity]!".to_string(),
                        deprecation: Default::default(),
                        cache_control: Default::default(),
                        external: false,
                        requires: None,
                        provides: None,
                        shareable: false,
                        visible: None,
                        inaccessible: false,
                        tags: Default::default(),
                        override_from: None,
                        compute_complexity: None,
                    },
                );
            }
        }
    }

    pub(crate) fn create_federation_types(&mut self) {
        <Any as InputType>::create_type_info(self);

        self.types.insert(
            "_Service".to_string(),
            MetaType::Object {
                name: "_Service".to_string(),
                description: None,
                fields: {
                    let mut fields = IndexMap::new();
                    fields.insert(
                        "sdl".to_string(),
                        MetaField {
                            name: "sdl".to_string(),
                            description: None,
                            args: Default::default(),
                            ty: "String".to_string(),
                            deprecation: Default::default(),
                            cache_control: Default::default(),
                            external: false,
                            requires: None,
                            provides: None,
                            shareable: false,
                            visible: None,
                            inaccessible: false,
                            tags: Default::default(),
                            override_from: None,
                            compute_complexity: None,
                        },
                    );
                    fields
                },
                cache_control: Default::default(),
                extends: false,
                shareable: false,
                keys: None,
                visible: None,
                inaccessible: false,
                tags: Default::default(),
                is_subscription: false,
                rust_typename: "async_graphql::federation::Service",
            },
        );

        self.create_entity_type_and_root_field();
    }

    pub fn names(&self) -> Vec<String> {
        let mut names = HashSet::new();

        for d in self.directives.values() {
            names.insert(d.name.to_string());
            names.extend(d.args.values().map(|arg| arg.name.to_string()));
        }

        for ty in self.types.values() {
            match ty {
                MetaType::Scalar { name, .. } | MetaType::Union { name, .. } => {
                    names.insert(name.clone());
                }
                MetaType::Object { name, fields, .. }
                | MetaType::Interface { name, fields, .. } => {
                    names.insert(name.clone());
                    names.extend(
                        fields
                            .values()
                            .map(|field| {
                                std::iter::once(field.name.clone())
                                    .chain(field.args.values().map(|arg| arg.name.to_string()))
                            })
                            .flatten(),
                    );
                }
                MetaType::Enum {
                    name, enum_values, ..
                } => {
                    names.insert(name.clone());
                    names.extend(enum_values.values().map(|value| value.name.to_string()));
                }
                MetaType::InputObject {
                    name, input_fields, ..
                } => {
                    names.insert(name.clone());
                    names.extend(input_fields.values().map(|field| field.name.to_string()));
                }
            }
        }

        names.into_iter().collect()
    }

    pub fn set_description(&mut self, name: &str, desc: &'static str) {
        match self.types.get_mut(name) {
            Some(MetaType::Scalar { description, .. }) => *description = Some(desc),
            Some(MetaType::Object { description, .. }) => *description = Some(desc),
            Some(MetaType::Interface { description, .. }) => *description = Some(desc),
            Some(MetaType::Union { description, .. }) => *description = Some(desc),
            Some(MetaType::Enum { description, .. }) => *description = Some(desc),
            Some(MetaType::InputObject { description, .. }) => *description = Some(desc),
            None => {}
        }
    }

    pub fn remove_unused_types(&mut self) {
        let mut used_types = BTreeSet::new();
        let mut unused_types = BTreeSet::new();

        fn traverse_field<'a>(
            types: &'a BTreeMap<String, MetaType>,
            used_types: &mut BTreeSet<&'a str>,
            field: &'a MetaField,
        ) {
            traverse_type(
                types,
                used_types,
                MetaTypeName::concrete_typename(&field.ty),
            );
            for arg in field.args.values() {
                traverse_input_value(types, used_types, arg);
            }
        }

        fn traverse_input_value<'a>(
            types: &'a BTreeMap<String, MetaType>,
            used_types: &mut BTreeSet<&'a str>,
            input_value: &'a MetaInputValue,
        ) {
            traverse_type(
                types,
                used_types,
                MetaTypeName::concrete_typename(&input_value.ty),
            );
        }

        fn traverse_type<'a>(
            types: &'a BTreeMap<String, MetaType>,
            used_types: &mut BTreeSet<&'a str>,
            type_name: &'a str,
        ) {
            if used_types.contains(type_name) {
                return;
            }

            if let Some(ty) = types.get(type_name) {
                used_types.insert(type_name);
                match ty {
                    MetaType::Object { fields, .. } => {
                        for field in fields.values() {
                            traverse_field(types, used_types, field);
                        }
                    }
                    MetaType::Interface {
                        fields,
                        possible_types,
                        ..
                    } => {
                        for field in fields.values() {
                            traverse_field(types, used_types, field);
                        }
                        for type_name in possible_types.iter() {
                            traverse_type(types, used_types, type_name);
                        }
                    }
                    MetaType::Union { possible_types, .. } => {
                        for type_name in possible_types.iter() {
                            traverse_type(types, used_types, type_name);
                        }
                    }
                    MetaType::InputObject { input_fields, .. } => {
                        for field in input_fields.values() {
                            traverse_input_value(types, used_types, field);
                        }
                    }
                    _ => {}
                }
            }
        }

        for directive in self.directives.values() {
            for arg in directive.args.values() {
                traverse_input_value(&self.types, &mut used_types, arg);
            }
        }

        for type_name in Some(&self.query_type)
            .into_iter()
            .chain(self.mutation_type.iter())
            .chain(self.subscription_type.iter())
        {
            traverse_type(&self.types, &mut used_types, type_name);
        }

        for ty in self.types.values().filter(|ty| match ty {
            MetaType::Object {
                keys: Some(keys), ..
            }
            | MetaType::Interface {
                keys: Some(keys), ..
            } => !keys.is_empty(),
            _ => false,
        }) {
            traverse_type(&self.types, &mut used_types, ty.name());
        }

        for ty in self.types.values() {
            let name = ty.name();
            if !is_system_type(name) && !used_types.contains(name) {
                unused_types.insert(name.to_string());
            }
        }

        for type_name in unused_types {
            self.types.remove(&type_name);
        }
    }

    pub fn find_visible_types(&self, ctx: &Context<'_>) -> HashSet<&str> {
        let mut visible_types = HashSet::new();

        fn traverse_field<'a>(
            ctx: &Context<'_>,
            types: &'a BTreeMap<String, MetaType>,
            visible_types: &mut HashSet<&'a str>,
            field: &'a MetaField,
        ) {
            if !is_visible(ctx, &field.visible) {
                return;
            }

            traverse_type(
                ctx,
                types,
                visible_types,
                MetaTypeName::concrete_typename(&field.ty),
            );
            for arg in field.args.values() {
                traverse_input_value(ctx, types, visible_types, arg);
            }
        }

        fn traverse_input_value<'a>(
            ctx: &Context<'_>,
            types: &'a BTreeMap<String, MetaType>,
            visible_types: &mut HashSet<&'a str>,
            input_value: &'a MetaInputValue,
        ) {
            if !is_visible(ctx, &input_value.visible) {
                return;
            }

            traverse_type(
                ctx,
                types,
                visible_types,
                MetaTypeName::concrete_typename(&input_value.ty),
            );
        }

        fn traverse_type<'a>(
            ctx: &Context<'_>,
            types: &'a BTreeMap<String, MetaType>,
            visible_types: &mut HashSet<&'a str>,
            type_name: &'a str,
        ) {
            if visible_types.contains(type_name) {
                return;
            }

            if let Some(ty) = types.get(type_name) {
                if !ty.is_visible(ctx) {
                    return;
                }

                visible_types.insert(type_name);
                match ty {
                    MetaType::Object { fields, .. } => {
                        for field in fields.values() {
                            traverse_field(ctx, types, visible_types, field);
                        }
                    }
                    MetaType::Interface {
                        fields,
                        possible_types,
                        ..
                    } => {
                        for field in fields.values() {
                            traverse_field(ctx, types, visible_types, field);
                        }
                        for type_name in possible_types.iter() {
                            traverse_type(ctx, types, visible_types, type_name);
                        }
                    }
                    MetaType::Union { possible_types, .. } => {
                        for type_name in possible_types.iter() {
                            traverse_type(ctx, types, visible_types, type_name);
                        }
                    }
                    MetaType::InputObject { input_fields, .. } => {
                        for field in input_fields.values() {
                            traverse_input_value(ctx, types, visible_types, field);
                        }
                    }
                    _ => {}
                }
            }
        }

        for directive in self.directives.values() {
            if is_visible(ctx, &directive.visible) {
                for arg in directive.args.values() {
                    traverse_input_value(ctx, &self.types, &mut visible_types, arg);
                }
            }
        }

        for type_name in Some(&self.query_type)
            .into_iter()
            .chain(self.mutation_type.iter())
            .chain(self.subscription_type.iter())
        {
            traverse_type(ctx, &self.types, &mut visible_types, type_name);
        }

        for ty in self.types.values().filter(|ty| match ty {
            MetaType::Object {
                keys: Some(keys), ..
            }
            | MetaType::Interface {
                keys: Some(keys), ..
            } => !keys.is_empty(),
            _ => false,
        }) {
            traverse_type(ctx, &self.types, &mut visible_types, ty.name());
        }

        for ty in self.types.values() {
            if let MetaType::Interface { possible_types, .. } = ty {
                if ty.is_visible(ctx) && !visible_types.contains(ty.name()) {
                    for type_name in possible_types.iter() {
                        if visible_types.contains(type_name.as_str()) {
                            traverse_type(ctx, &self.types, &mut visible_types, ty.name());
                            break;
                        }
                    }
                }
            }
        }

        self.types
            .values()
            .filter_map(|ty| {
                let name = ty.name();
                if is_system_type(name) || visible_types.contains(name) {
                    Some(name)
                } else {
                    None
                }
            })
            .collect()
    }
}

pub(crate) fn is_visible(ctx: &Context<'_>, visible: &Option<MetaVisibleFn>) -> bool {
    match visible {
        Some(f) => f(ctx),
        None => true,
    }
}

fn is_system_type(name: &str) -> bool {
    if name.starts_with("__") {
        return true;
    }

    name == "Boolean" || name == "Int" || name == "Float" || name == "String" || name == "ID"
}
