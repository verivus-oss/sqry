//! T2.4 channel send / receive / close pairing emission.
//!
//! Covers the channel-pairing acceptance criteria from
//! `docs/development/go-channels-and-generic-instantiation/01_SPEC.md`:
//!
//! * AC-1 — named-local channel: Send + Receive `ChannelPeer` edges to a single
//!   `q::main::ch` `Channel` node.
//! * AC-2a — single-parameter pass-through (single file): producer / consumer
//!   body ops pair onto the make-rooted `q::main::ch`.
//! * AC-2b — multi-file pass-through is Phase 2: absence of the pass-through
//!   edges (the make-site node still exists once a peer references it).
//! * AC-3 — struct-field channel via the method receiver selector.
//! * AC-4 — factory-returned channel: no `Channel` node, no `ChannelPeer` edge
//!   (zero-false-positive fence).
//! * AC-5 — `close(ch)` emits a `ChannelPeer{Close}` alongside Send / Receive.
//! * AC-6 — `select` arms emit Receive / Send onto distinct channels.

use std::collections::HashMap;
use std::path::Path;

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StringId;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::{ChannelBufferKind, ChannelPeerDirection};
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_go::relations::GoGraphBuilder;
use tree_sitter::Parser;

fn parse_go_file(content: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("set Go language");
    parser.parse(content.as_bytes(), None).expect("parse Go")
}

fn build_test_graph(source: &str, filename: &str) -> StagingGraph {
    let tree = parse_go_file(source);
    let mut staging = StagingGraph::new();
    let builder = GoGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), Path::new(filename), &mut staging)
        .expect("build_graph should succeed");
    staging
}

fn string_lookup(staging: &StagingGraph) -> HashMap<StringId, String> {
    let mut map = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::InternString { local_id, value } = op {
            map.insert(*local_id, value.clone());
        }
    }
    map
}

/// Node index -> (display name, kind).
fn node_lookup(staging: &StagingGraph) -> HashMap<u32, (String, NodeKind)> {
    let strings = string_lookup(staging);
    let mut map = HashMap::new();
    for op in staging.operations() {
        if let StagingOp::AddNode { entry, expected_id } = op
            && let Some(id) = expected_id
        {
            let name = entry
                .qualified_name
                .and_then(|q| strings.get(&q).cloned())
                .or_else(|| strings.get(&entry.name).cloned())
                .unwrap_or_default();
            map.insert(id.index(), (name, entry.kind));
        }
    }
    map
}

/// Qualified names of every staged `Channel` node.
fn channel_nodes(staging: &StagingGraph) -> Vec<String> {
    node_lookup(staging)
        .into_values()
        .filter(|(_, kind)| *kind == NodeKind::Channel)
        .map(|(name, _)| name)
        .collect()
}

struct ChannelPeer {
    target_channel: String,
    direction: ChannelPeerDirection,
    buffer_kind: ChannelBufferKind,
}

fn channel_peers(staging: &StagingGraph) -> Vec<ChannelPeer> {
    let nodes = node_lookup(staging);
    let mut out = Vec::new();
    for op in staging.operations() {
        if let StagingOp::AddEdge {
            target,
            kind:
                EdgeKind::ChannelPeer {
                    direction,
                    buffer_kind,
                },
            ..
        } = op
        {
            out.push(ChannelPeer {
                target_channel: nodes
                    .get(&target.index())
                    .map(|(n, _)| n.clone())
                    .unwrap_or_default(),
                direction: *direction,
                buffer_kind: *buffer_kind,
            });
        }
    }
    out
}

fn directions_to(peers: &[ChannelPeer], channel: &str) -> Vec<ChannelPeerDirection> {
    peers
        .iter()
        .filter(|p| p.target_channel == channel)
        .map(|p| p.direction)
        .collect()
}

#[test]
fn ac1_named_channel_send_recv_pair() {
    let src = r#"
package q

func main() {
    ch := make(chan int, 4)
    ch <- 1
    x := <-ch
    _ = x
}
"#;
    let staging = build_test_graph(src, "q.go");
    let channels = channel_nodes(&staging);
    assert_eq!(
        channels,
        vec!["q::main::ch".to_string()],
        "exactly one Channel node q::main::ch (got {channels:?})"
    );

    let peers = channel_peers(&staging);
    let dirs = directions_to(&peers, "q::main::ch");
    assert!(
        dirs.contains(&ChannelPeerDirection::Send),
        "expected a Send ChannelPeer to q::main::ch (got {dirs:?})"
    );
    assert!(
        dirs.contains(&ChannelPeerDirection::Receive),
        "expected a Receive ChannelPeer to q::main::ch (got {dirs:?})"
    );
    // make(chan int, 4) is buffered with capacity 4.
    assert!(
        peers
            .iter()
            .all(|p| p.buffer_kind == ChannelBufferKind::Buffered),
        "the buffer classifier must be Buffered for make(chan int, 4)"
    );
}

#[test]
fn ac2a_single_param_pass_through_single_file() {
    let src = r#"
package q

func producer(out chan<- int) { out <- 1 }

func consumer(in <-chan int) { <-in }

func main() {
    ch := make(chan int)
    go producer(ch)
    consumer(ch)
}
"#;
    let staging = build_test_graph(src, "q.go");
    let channels = channel_nodes(&staging);
    assert_eq!(
        channels,
        vec!["q::main::ch".to_string()],
        "single canonical Channel node q::main::ch (got {channels:?})"
    );

    let peers = channel_peers(&staging);
    let dirs = directions_to(&peers, "q::main::ch");
    assert!(
        dirs.contains(&ChannelPeerDirection::Send),
        "producer's send-site must pair onto q::main::ch (got {dirs:?})"
    );
    assert!(
        dirs.contains(&ChannelPeerDirection::Receive),
        "consumer's receive-site must pair onto q::main::ch (got {dirs:?})"
    );
}

#[test]
fn ac2b_multi_file_pass_through_is_phase2_absence() {
    // producer/consumer in separate files: the file-local rule-2 table is empty
    // for their bodies, so no ChannelPeer edge is emitted from them (Phase 2).
    let producer = r#"
package q

func producer(out chan<- int) { out <- 1 }
"#;
    let consumer = r#"
package q

func consumer(in <-chan int) { <-in }
"#;
    let prod_staging = build_test_graph(producer, "producer.go");
    let cons_staging = build_test_graph(consumer, "consumer.go");

    assert!(
        channel_peers(&prod_staging).is_empty(),
        "no ChannelPeer edge from producer in the multi-file case (Phase 2 fence)"
    );
    assert!(
        channel_peers(&cons_staging).is_empty(),
        "no ChannelPeer edge from consumer in the multi-file case (Phase 2 fence)"
    );
    assert!(
        channel_nodes(&prod_staging).is_empty() && channel_nodes(&cons_staging).is_empty(),
        "no Channel node when the make is in another file"
    );
}

#[test]
fn ac3_struct_field_channel() {
    let src = r#"
package q

type Job struct{}

type W struct {
    in chan Job
}

func (w *W) Submit(j Job) { w.in <- j }

func (w *W) Loop() {
    for j := range w.in {
        process(j)
    }
}

func process(j Job) {}
"#;
    let staging = build_test_graph(src, "q.go");
    let channels = channel_nodes(&staging);
    assert_eq!(
        channels,
        vec!["q::W::in".to_string()],
        "one struct-field-rooted Channel node q::W::in (got {channels:?})"
    );

    let peers = channel_peers(&staging);
    let dirs = directions_to(&peers, "q::W::in");
    assert!(
        dirs.contains(&ChannelPeerDirection::Send),
        "Submit's send-site must pair onto q::W::in (got {dirs:?})"
    );
    assert!(
        dirs.contains(&ChannelPeerDirection::Receive),
        "Loop's range must pair onto q::W::in (got {dirs:?})"
    );
}

#[test]
fn ac4_factory_returned_channel_emits_nothing() {
    let src = r#"
package q

func make_ch() chan int { return make(chan int) }

func main() {
    ch := make_ch()
    ch <- 1
}
"#;
    let staging = build_test_graph(src, "q.go");
    assert!(
        channel_nodes(&staging).is_empty(),
        "no Channel node for a factory-returned channel (AC-4 fence)"
    );
    assert!(
        channel_peers(&staging).is_empty(),
        "no ChannelPeer edge for a factory-returned channel (AC-4 fence)"
    );
}

#[test]
fn nested_closure_make_does_not_pair_outer_operation() {
    // Regression (Codex iter-2): the outer `ch` is factory-returned (not a
    // make), while a nested closure declares its own `ch := make(...)`. The
    // byte-ordered declaration scan must NOT attribute the closure's make to
    // the outer `ch <- 1` operation — that is a scope-insensitivity false
    // positive and an AC-4 violation.
    let src = r#"
package q

func getChan() chan int { return make(chan int) }

func main() {
    ch := getChan()
    f := func() {
        ch := make(chan int)
        _ = ch
    }
    f()
    ch <- 1
}
"#;
    let staging = build_test_graph(src, "q.go");
    assert!(
        channel_nodes(&staging).is_empty(),
        "the closure's make must not become a Channel node for the outer op (got {:?})",
        channel_nodes(&staging)
    );
    assert!(
        channel_peers(&staging).is_empty(),
        "the outer `ch <- 1` (factory-returned ch) must emit no ChannelPeer edge"
    );
}

#[test]
fn ac5_close_direction() {
    let src = r#"
package q

func main() {
    ch := make(chan int)
    go func() {
        ch <- 1
        close(ch)
    }()
    for x := range ch {
        _ = x
    }
}
"#;
    let staging = build_test_graph(src, "q.go");
    let channels = channel_nodes(&staging);
    assert_eq!(
        channels,
        vec!["q::main::ch".to_string()],
        "one Channel node q::main::ch (got {channels:?})"
    );
    let dirs = directions_to(&channel_peers(&staging), "q::main::ch");
    for expected in [
        ChannelPeerDirection::Send,
        ChannelPeerDirection::Close,
        ChannelPeerDirection::Receive,
    ] {
        assert!(
            dirs.contains(&expected),
            "expected {expected:?} ChannelPeer to q::main::ch (got {dirs:?})"
        );
    }
}

#[test]
fn ac6_select_pairing() {
    let src = r#"
package q

func main() {
    a := make(chan int)
    b := make(chan int)
    select {
    case x := <-a:
        _ = x
    case b <- 1:
    }
}
"#;
    let staging = build_test_graph(src, "q.go");
    let mut channels = channel_nodes(&staging);
    channels.sort();
    assert_eq!(
        channels,
        vec!["q::main::a".to_string(), "q::main::b".to_string()],
        "two Channel nodes q::main::a and q::main::b (got {channels:?})"
    );

    let peers = channel_peers(&staging);
    assert_eq!(
        directions_to(&peers, "q::main::a"),
        vec![ChannelPeerDirection::Receive],
        "the `<-a` select arm receives on q::main::a"
    );
    assert_eq!(
        directions_to(&peers, "q::main::b"),
        vec![ChannelPeerDirection::Send],
        "the `b <- 1` select arm sends on q::main::b"
    );
}
