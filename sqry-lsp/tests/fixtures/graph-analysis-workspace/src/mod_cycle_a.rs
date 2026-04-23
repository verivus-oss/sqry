use super::mod_cycle_b::mod_cycle_b_entry;

pub(crate) fn mod_cycle_a_entry() {
    mod_cycle_b_entry();
}
