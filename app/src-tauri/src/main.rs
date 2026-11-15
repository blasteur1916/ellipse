// Pas de fenêtre de console en version finale.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ellipse_lib::run()
}
