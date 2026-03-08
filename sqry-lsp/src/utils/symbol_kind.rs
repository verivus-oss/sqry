use sqry_core::graph::unified::NodeKind;
use tower_lsp::lsp_types::SymbolKind;

/// Map graph-native `NodeKind` to LSP `SymbolKind`.
#[must_use]
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
        NodeKind::Component
        | NodeKind::StyleRule
        | NodeKind::StyleAtRule
        | NodeKind::Lifetime
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
}
