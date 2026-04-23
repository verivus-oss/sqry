const _UTF16_PREFIX: &str = "é"; fn utf16_cycle_start() { utf16_cycle_end(); }
const _UTF16_UNUSED_PREFIX: &str = "é"; fn utf16_unused_marker() {}

fn utf16_cycle_end() {
    utf16_cycle_start();
}
