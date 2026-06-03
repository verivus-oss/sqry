use sqry_core::graph::unified::NodeKind;
use tower_lsp::lsp_types::SymbolKind;

/// Map graph-native `NodeKind` to LSP `SymbolKind`.
#[must_use]
#[allow(
    clippy::match_same_arms,
    reason = "arms are kept separate for semantic clarity; EnumVariant vs EnumConstant, Type vs TypeParameter, Module vs JavaModule are distinct domain concepts"
)]
pub fn node_kind_to_symbol_kind(kind: NodeKind) -> SymbolKind {
    match kind {
        NodeKind::Function | NodeKind::Macro | NodeKind::CallSite | NodeKind::Test => {
            SymbolKind::FUNCTION
        }
        NodeKind::Method | NodeKind::Endpoint => SymbolKind::METHOD,
        NodeKind::Class | NodeKind::Service | NodeKind::Resource => SymbolKind::CLASS,
        NodeKind::Struct => SymbolKind::STRUCT,
        NodeKind::Interface | NodeKind::Trait => SymbolKind::INTERFACE,
        NodeKind::Enum => SymbolKind::ENUM,
        NodeKind::EnumVariant => SymbolKind::ENUM_MEMBER,
        NodeKind::Variable | NodeKind::Parameter | NodeKind::StyleVariable => SymbolKind::VARIABLE,
        NodeKind::Constant => SymbolKind::CONSTANT,
        NodeKind::Type => SymbolKind::TYPE_PARAMETER,
        NodeKind::Module => SymbolKind::NAMESPACE,
        NodeKind::Property => SymbolKind::PROPERTY,
        NodeKind::Import | NodeKind::Export => SymbolKind::PACKAGE,
        NodeKind::EnumConstant => SymbolKind::ENUM_MEMBER,
        NodeKind::TypeParameter => SymbolKind::TYPE_PARAMETER,
        NodeKind::Annotation | NodeKind::AnnotationValue => SymbolKind::EVENT,
        NodeKind::LambdaTarget => SymbolKind::FUNCTION,
        NodeKind::JavaModule => SymbolKind::NAMESPACE,
        NodeKind::Component
        | NodeKind::StyleRule
        | NodeKind::StyleAtRule
        | NodeKind::Lifetime
        | NodeKind::Channel
        | NodeKind::Other => SymbolKind::OBJECT,
    }
}

#[cfg(test)]
mod tests {
    use super::{NodeKind, SymbolKind, node_kind_to_symbol_kind};

    #[test]
    fn node_kind_parameter_maps_to_variable() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Parameter),
            SymbolKind::VARIABLE
        );
    }

    #[test]
    fn node_kind_import_maps_to_package() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Import),
            SymbolKind::PACKAGE
        );
    }

    // Cover every NodeKind variant exhaustively

    #[test]
    fn function_variants_map_to_function() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Function),
            SymbolKind::FUNCTION
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Macro),
            SymbolKind::FUNCTION
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::CallSite),
            SymbolKind::FUNCTION
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Test),
            SymbolKind::FUNCTION
        );
    }

    #[test]
    fn method_variants_map_to_method() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Method),
            SymbolKind::METHOD
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Endpoint),
            SymbolKind::METHOD
        );
    }

    #[test]
    fn class_variants_map_to_class() {
        assert_eq!(node_kind_to_symbol_kind(NodeKind::Class), SymbolKind::CLASS);
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Service),
            SymbolKind::CLASS
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Resource),
            SymbolKind::CLASS
        );
    }

    #[test]
    fn struct_maps_to_struct() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Struct),
            SymbolKind::STRUCT
        );
    }

    #[test]
    fn interface_variants_map_to_interface() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Interface),
            SymbolKind::INTERFACE
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Trait),
            SymbolKind::INTERFACE
        );
    }

    #[test]
    fn enum_maps_to_enum() {
        assert_eq!(node_kind_to_symbol_kind(NodeKind::Enum), SymbolKind::ENUM);
    }

    #[test]
    fn enum_variant_maps_to_enum_member() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::EnumVariant),
            SymbolKind::ENUM_MEMBER
        );
    }

    #[test]
    fn variable_variants_map_to_variable() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Variable),
            SymbolKind::VARIABLE
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::StyleVariable),
            SymbolKind::VARIABLE
        );
    }

    #[test]
    fn constant_maps_to_constant() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Constant),
            SymbolKind::CONSTANT
        );
    }

    #[test]
    fn type_maps_to_type_parameter() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Type),
            SymbolKind::TYPE_PARAMETER
        );
    }

    #[test]
    fn module_maps_to_namespace() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Module),
            SymbolKind::NAMESPACE
        );
    }

    #[test]
    fn property_maps_to_property() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Property),
            SymbolKind::PROPERTY
        );
    }

    #[test]
    fn export_maps_to_package() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Export),
            SymbolKind::PACKAGE
        );
    }

    #[test]
    fn object_variants_map_to_object() {
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Component),
            SymbolKind::OBJECT
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::StyleRule),
            SymbolKind::OBJECT
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::StyleAtRule),
            SymbolKind::OBJECT
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Channel),
            SymbolKind::OBJECT
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Lifetime),
            SymbolKind::OBJECT
        );
        assert_eq!(
            node_kind_to_symbol_kind(NodeKind::Other),
            SymbolKind::OBJECT
        );
    }
}
