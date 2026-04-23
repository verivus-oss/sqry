use super::mod_cycle_a::mod_cycle_a_entry;

pub(crate) fn mod_cycle_b_entry() {
    mod_cycle_a_entry();
}
