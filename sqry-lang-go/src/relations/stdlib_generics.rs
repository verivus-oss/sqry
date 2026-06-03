//! Static catalog of Go stdlib generic signatures (T2.5, Phase 4b).
//!
//! The inferred-instantiation path (`02_DESIGN.md` §4.3) resolves a generic
//! callee's type-parameter list and parameter-type patterns from one of two
//! sources: the in-file generic-function declarations (collected at parse
//! time, see `graph_builder::collect_local_generic_sigs`) or, when the callee
//! lives outside the workspace (`slices.SortFunc`, `cmp.Compare`, ...), this
//! hand-maintained catalog.
//!
//! The design sketched the catalog as a `phf::phf_map!`; we use a plain `match`
//! so the Go plugin gains no new dependency. The data is identical: each entry
//! records the type-parameter names in declaration order and a `ParamPattern`
//! per call-argument position describing how to peel that argument's static
//! type back to a type-parameter binding.
//!
//! Maintenance: every Go-stdlib version bump that adds a generic function in
//! `slices` / `maps` / `cmp` (or a future `iter`, `clear`, ...) that the
//! inference subset must cover needs an additive arm here. Out-of-workspace
//! non-stdlib generics are not cataloged (Phase 2 brings them in by parsing the
//! third-party package's index).

/// How a call-site argument's static type maps onto type-parameter bindings.
///
/// Owned at runtime so local (in-file) generic signatures and the static
/// stdlib catalog share one representation. The variants cover exactly the
/// peelable subset of `02_DESIGN.md` §4.3 (`peel_static_type`); anything
/// richer resolves to `Other` and leaves the slot `<unknown>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParamPattern {
    /// The argument's whole static type binds this type parameter, e.g. the
    /// `x S` parameter of `slices.SortFunc[S ~[]E, E any]` binds `S`.
    Whole(String),
    /// The argument's static type is `[]E`; peel one slice layer to bind `E`.
    SliceElem(String),
    /// The argument is a function value (`func literal` / typed func); bind
    /// type parameters from its parameter and result positions. A `None` slot
    /// means "this position carries no type parameter, ignore it".
    FuncSig {
        params: Vec<Option<String>>,
        results: Vec<Option<String>>,
    },
    /// Not peelable by the Phase 1 subset (interface, complex composite, ...).
    Other,
}

/// A generic callee's signature in the form the inference pass consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericSig {
    /// Type-parameter names in declaration order (`["S", "E"]`).
    pub type_params: Vec<String>,
    /// One pattern per declared parameter position, in declaration order.
    pub params: Vec<ParamPattern>,
}

impl GenericSig {
    fn whole(tp: &str) -> ParamPattern {
        ParamPattern::Whole(tp.to_string())
    }
}

/// Helper: build a `FuncSig` pattern from parallel name slices.
fn func_sig(params: &[Option<&str>], results: &[Option<&str>]) -> ParamPattern {
    ParamPattern::FuncSig {
        params: params.iter().map(|o| o.map(str::to_string)).collect(),
        results: results.iter().map(|o| o.map(str::to_string)).collect(),
    }
}

fn tps(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

/// Look up a cataloged stdlib generic signature by canonical qualified name
/// (`"slices.SortFunc"`). Returns `None` for uncataloged callees.
pub(crate) fn lookup(qualified: &str) -> Option<GenericSig> {
    let sig = match qualified {
        // slices (Go 1.21)
        "slices.SortFunc" | "slices.SortStableFunc" => GenericSig {
            type_params: tps(&["S", "E"]),
            params: vec![
                GenericSig::whole("S"),
                func_sig(&[Some("E"), Some("E")], &[None]),
            ],
        },
        "slices.IndexFunc" | "slices.ContainsFunc" | "slices.DeleteFunc" => GenericSig {
            type_params: tps(&["S", "E"]),
            params: vec![GenericSig::whole("S"), func_sig(&[Some("E")], &[None])],
        },
        "slices.Index" | "slices.Contains" => GenericSig {
            type_params: tps(&["S", "E"]),
            params: vec![GenericSig::whole("S"), GenericSig::whole("E")],
        },
        "slices.Sort" | "slices.Min" | "slices.Max" | "slices.Clone" | "slices.Compact"
        | "slices.IsSorted" | "slices.Reverse" => GenericSig {
            type_params: tps(&["S", "E"]),
            params: vec![GenericSig::whole("S")],
        },
        "slices.Equal" => GenericSig {
            type_params: tps(&["S", "E"]),
            params: vec![GenericSig::whole("S"), GenericSig::whole("S")],
        },
        "slices.BinarySearch" => GenericSig {
            type_params: tps(&["S", "E"]),
            params: vec![GenericSig::whole("S"), GenericSig::whole("E")],
        },
        // maps (Go 1.21)
        "maps.Keys" | "maps.Values" | "maps.Clone" => GenericSig {
            type_params: tps(&["M", "K", "V"]),
            params: vec![GenericSig::whole("M")],
        },
        // cmp (Go 1.21)
        "cmp.Compare" | "cmp.Less" => GenericSig {
            type_params: tps(&["T"]),
            params: vec![GenericSig::whole("T"), GenericSig::whole("T")],
        },
        "cmp.Or" => GenericSig {
            type_params: tps(&["T"]),
            params: vec![GenericSig::whole("T")],
        },
        _ => return None,
    };
    Some(sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sortfunc_has_two_params() {
        let sig = lookup("slices.SortFunc").expect("cataloged");
        assert_eq!(sig.type_params, vec!["S".to_string(), "E".to_string()]);
        assert_eq!(sig.params.len(), 2);
        assert_eq!(sig.params[0], ParamPattern::Whole("S".to_string()));
        match &sig.params[1] {
            ParamPattern::FuncSig { params, results } => {
                assert_eq!(params, &vec![Some("E".to_string()), Some("E".to_string())]);
                assert_eq!(results, &vec![None]);
            }
            other => panic!("expected FuncSig, got {other:?}"),
        }
    }

    #[test]
    fn uncataloged_returns_none() {
        assert!(lookup("fmt.Println").is_none());
        assert!(lookup("q.LocalGeneric").is_none());
    }

    #[test]
    fn cmp_compare_two_t() {
        let sig = lookup("cmp.Compare").expect("cataloged");
        assert_eq!(sig.type_params, vec!["T".to_string()]);
        assert_eq!(sig.params.len(), 2);
    }
}
