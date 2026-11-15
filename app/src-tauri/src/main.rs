// Pas de fenêtre de console en version finale.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Linux : X11 quand il est là (sous Wayland, une appli ne peut pas placer sa fenêtre en haut de l'écran
    // ni tenir un raccourci global), et sans le rendu DMA-BUF de WebKitGTK, qui donne une fenêtre vide avec
    // certains pilotes (NVIDIA). Avant tout le reste : un seul fil tourne encore.
    #[cfg(target_os = "linux")]
    for (variable, valeur) in [("GDK_BACKEND", "x11"), ("WEBKIT_DISABLE_DMABUF_RENDERER", "1")] {
        if std::env::var_os(variable).is_none() {
            // SAFETY : au tout début de `main`, aucun autre fil ne lit l'environnement.
            unsafe { std::env::set_var(variable, valeur) };
        }
    }
    ellipse_lib::run()
}
