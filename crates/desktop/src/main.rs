// Entry point only — see src/lib.rs for everything else. Kept separate so
// integration tests can link against the library and reuse its logic
// (overlay geometry, settings) without pulling in a second copy.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    remote_assist_lib::run();
}
