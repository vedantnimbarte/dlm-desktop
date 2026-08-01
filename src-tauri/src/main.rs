// Release builds on Windows must not open a console behind the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    dlm_desktop_lib::run()
}
