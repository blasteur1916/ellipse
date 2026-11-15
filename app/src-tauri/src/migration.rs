//! Ellipse s'appelait Furet (versions 0.9). Au premier lancement, ce qu'avait laissé Furet passe à Ellipse :
//! ses réglages et son index sont déplacés (rien à retélécharger ni à relire), Furet ne démarre plus avec
//! Windows, et sa copie installée est désinstallée sans bruit. Avant tout le reste : les journaux et les
//! réglages d'Ellipse vont dans ces dossiers.

#[cfg(windows)]
pub fn depuis_furet() {
    use std::path::PathBuf;

    // Les données d'une appli Tauri : %APPDATA%\<identifiant> (réglages) et %LOCALAPPDATA%\<identifiant>
    // (journaux, outils et modèles, index).
    for variable in ["APPDATA", "LOCALAPPDATA"] {
        let Some(base) = std::env::var_os(variable).map(PathBuf::from) else { continue };
        let (ancien, neuf) = (base.join("app.furet"), base.join("app.ellipse"));
        if ancien.is_dir() && !neuf.exists() {
            // Furet encore ouvert (fichiers occupés) : ce sera pour le prochain lancement.
            if let Err(e) = std::fs::rename(&ancien, &neuf) {
                eprintln!("données de Furet pas encore déplacées ({}) : {e}", ancien.display());
            }
        }
    }

    // Furet ne démarre plus avec Windows (Ellipse s'y inscrit à sa place, selon son réglage).
    let cle = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let _ = ellipse_moteur::processus::commande("reg").args(["delete", cle, "/v", "Furet", "/f"]).output();

    // La copie installée de Furet : désinstallée en silence (ses données sont déjà passées à Ellipse).
    if let Some(base) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let desinstallateur = base.join("Furet").join("uninstall.exe");
        if desinstallateur.is_file() {
            let _ = ellipse_moteur::processus::commande(&desinstallateur).arg("/S").spawn();
        }
    }
}

/// Furet n'a existé que sous Windows.
#[cfg(not(windows))]
pub fn depuis_furet() {}
