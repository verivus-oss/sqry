mod cycle_ab;
mod cycle_ba;
mod mod_cycle_a;
mod mod_cycle_b;
mod reachability;
mod self_loop;
mod unused_bulk;
mod utf16_ident;

fn main() {
    reachability::drive_imports();
    let _ = reachability::drive_references();
    let _ = reachability::drive_type_of();
}
