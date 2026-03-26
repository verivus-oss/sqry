use sqry_core::search::classifier::{QueryClassifier, QueryType};

/// Ensure CLI predicate classification treats `returns:` as a metadata predicate
/// and keeps it on the semantic path instead of relation-only handling.
#[test]
fn predicate_classification_returns() {
    assert_eq!(
        QueryClassifier::classify("returns:Optional<User>"),
        QueryType::Semantic,
        "returns: predicates should be classified as semantic/metadata queries"
    );

    // Simple relation predicate sanity check (still semantic because it requires relation data)
    assert_eq!(
        QueryClassifier::classify("callers:findById"),
        QueryType::Semantic,
        "relation predicates must continue to be classified as semantic"
    );
}
