//! Les applications installées, cherchées par leur nom et toujours montrées avant les fichiers : « chrome »
//! trouve Google Chrome. Relues de temps en temps, sur un fil à part.
//! - Windows : celles du menu Démarrer (dossier « Applications » de l'interpréteur de commandes : programmes
//!   classiques et applis du Microsoft Store), lancées par `explorer.exe shell:AppsFolder\<id>`.
//! - Linux : les fichiers `.desktop` (ceux du menu des applications), lancés par `gio launch`.
//! - macOS : les `.app` des dossiers Applications, lancées par `open`.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use ellipse_moteur::noms::{normaliser, score_avec};

/// La liste est relue à ce rythme (une appli installée apparaît sans relancer Ellipse).
const RELECTURE: Duration = Duration::from_secs(10 * 60);
/// En dessous, une appli ne sort pas. Une faute de frappe (« calculatrise ») donne 0,675.
const SCORE_MIN: f32 = 0.65;
/// Une ou deux lettres : seulement au début d'un mot (sinon « e » trouverait presque toutes les applis).
const COURTE: usize = 2;
const SCORE_COURTE_MIN: f32 = 0.95;
/// Les applis bien moins ressemblantes que la meilleure ne sortent pas.
const ECART_MAX: f32 = 0.2;

#[derive(Debug, Clone)]
pub struct Appli {
    pub nom: String,
    /// Le nom normalisé, et ses initiales (« visual studio code » → « vsc »).
    cle: String,
    initiales: String,
    /// Ce qui la lance et donne son icône : `shell:AppsFolder\<identifiant>` (Windows), le fichier
    /// `.desktop` (Linux) ou le dossier `.app` (macOS).
    pub chemin: String,
    /// Linux : son icône en PNG, si le thème en a une.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    icone: Option<PathBuf>,
}

static APPLIS: RwLock<Vec<Appli>> = RwLock::new(Vec::new());

/// Lit la liste tout de suite, puis la relit de temps en temps.
pub fn demarrer() {
    let lance = std::thread::Builder::new().name("ellipse-applications".into()).spawn(|| loop {
        let liste = lire();
        log::info!("{} applications", liste.len());
        *APPLIS.write().unwrap_or_else(|e| e.into_inner()) = liste;
        std::thread::sleep(RELECTURE);
    });
    if let Err(e) = lance {
        log::warn!("liste des applications impossible : {e}");
    }
}

/// Les `n` applications qui ressemblent le plus à `texte`, la meilleure d'abord.
pub fn chercher(texte: &str, n: usize) -> Vec<(f32, Appli)> {
    let recherche = normaliser(texte);
    if recherche.is_empty() {
        return Vec::new();
    }
    let applis = APPLIS.read().unwrap_or_else(|e| e.into_inner());
    let mut trouvees: Vec<(f32, Appli)> = applis
        .iter()
        .filter_map(|a| {
            let mut score = score_avec(&recherche, &a.cle, true);
            // « vsc » pour Visual Studio Code.
            if recherche.len() >= 2 && !recherche.contains(' ') && a.initiales.starts_with(&recherche) {
                score = score.max(0.9);
            }
            let min = if recherche.chars().count() <= COURTE { SCORE_COURTE_MIN } else { SCORE_MIN };
            (score >= min).then(|| (score, a.clone()))
        })
        .collect();
    // À score égal, le nom le plus court (« Chrome » avant « Chrome Remote Desktop »).
    trouvees.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.nom.len().cmp(&b.1.nom.len())));
    // Une faute de frappe n'est tolérée que si rien ne correspond vraiment : « chrome » ne sort pas Razer Chroma.
    if let Some(&(meilleur, _)) = trouvees.first() {
        trouvees.retain(|(s, _)| *s >= meilleur - ECART_MAX);
    }
    trouvees.truncate(n);
    trouvees
}

/// Une entrée du menu Démarrer qui n'est pas vraiment une appli : désinstaller, lire la notice…
fn annexe(cle: &str) -> bool {
    const MOTS: [&str; 9] =
        ["uninstall", "desinstall", "readme", "read me", "lisez moi", "release notes", "documentation", "website", "site web"];
    MOTS.iter().any(|m| cle.contains(m))
}

fn nouvelle(nom: String, chemin: String) -> Option<Appli> {
    nouvelle_avec_icone(nom, chemin, None)
}

fn nouvelle_avec_icone(nom: String, chemin: String, icone: Option<PathBuf>) -> Option<Appli> {
    let cle = normaliser(&nom);
    if cle.is_empty() || annexe(&cle) {
        return None;
    }
    let initiales = cle.split(' ').filter_map(|m| m.chars().next()).collect();
    Some(Appli { nom, cle, initiales, chemin, icone })
}

#[cfg(windows)]
fn lire() -> Vec<Appli> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT, SHGetKnownFolderItem,
        SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    };

    // SAFETY : COM est initialisé pour ce fil ; chaque texte rendu par l'interpréteur est copié puis libéré.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let texte = |item: &IShellItem, sorte: SIGDN| -> Option<String> {
            let p = item.GetDisplayName(sorte).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        };
        let Ok(dossier) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else {
            return Vec::new();
        };
        let Ok(elements) = dossier.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            return Vec::new();
        };
        let mut applis: Vec<Appli> = Vec::new();
        loop {
            let mut un = [None];
            let mut lus = 0;
            if elements.Next(&mut un, Some(&mut lus)).is_err() || lus == 0 {
                break;
            }
            let Some(item) = un[0].take() else { break };
            let (Some(nom), Some(id)) = (texte(&item, SIGDN_NORMALDISPLAY), texte(&item, SIGDN_PARENTRELATIVEPARSING)) else {
                continue;
            };
            // Un raccourci vers une page web ou un document (.url, .pdf…) n'est pas une appli.
            let id_min = id.to_lowercase();
            if id_min.starts_with("http") || [".url", ".pdf", ".txt", ".chm", ".html", ".htm"].iter().any(|e| id_min.ends_with(e)) {
                continue;
            }
            if let Some(a) = nouvelle(nom, format!(r"shell:AppsFolder\{id}")) {
                // Deux entrées du même nom (installée pour tous et pour soi) : une seule.
                if !applis.iter().any(|b| b.cle == a.cle) {
                    applis.push(a);
                }
            }
        }
        applis
    }
}

/// Ce chemin désigne-t-il une application (plutôt qu'un fichier) ?
pub fn est_appli(chemin: &Path) -> bool {
    let c = chemin.to_string_lossy();
    if cfg!(windows) {
        c.starts_with(r"shell:AppsFolder\")
    } else if cfg!(target_os = "macos") {
        c.ends_with(".app")
    } else {
        c.ends_with(".desktop")
    }
}

/// Lance une application.
pub fn lancer(chemin: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    ellipse_moteur::processus::commande("explorer.exe").arg(chemin).spawn()?;
    #[cfg(target_os = "macos")]
    ellipse_moteur::processus::commande("open").arg(chemin).spawn()?;
    #[cfg(target_os = "linux")]
    {
        // `gio` vient avec GLib (présent partout où il y a GTK) ; sinon `gtk-launch`, qui prend l'identifiant.
        let gio = ellipse_moteur::processus::commande("gio").arg("launch").arg(chemin).spawn();
        if gio.is_err() {
            let id = chemin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ellipse_moteur::processus::commande("gtk-launch").arg(id).spawn()?;
        }
    }
    Ok(())
}

/// L'icône d'une application en PNG, pour sa miniature (Linux : celle du thème ; macOS : convertie depuis
/// son .icns, une fois, dans le dossier temporaire). Rien pour un fichier.
#[cfg(not(windows))]
pub fn icone(chemin: &Path) -> Option<PathBuf> {
    if !est_appli(chemin) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let c = chemin.to_string_lossy();
        APPLIS.read().unwrap_or_else(|e| e.into_inner()).iter().find(|a| a.chemin == c)?.icone.clone()
    }
    #[cfg(target_os = "macos")]
    {
        icone_mac(chemin)
    }
}

/// Linux : les fichiers `.desktop` des dossiers de données (XDG), le premier trouvé gagnant (ceux de
/// l'utilisateur passent avant ceux du système). Ni ceux qui se cachent (NoDisplay, Hidden), ni ce qui
/// n'est pas une application (liens, dossiers).
#[cfg(target_os = "linux")]
fn lire() -> Vec<Appli> {
    let maison = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let perso = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| maison.join(".local/share"));
    let systeme = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let mut donnees: Vec<PathBuf> = vec![perso];
    donnees.extend(systeme.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    // Flatpak et Snap, s'ils ne sont pas déjà dans la liste.
    for d in ["/var/lib/flatpak/exports/share", "/var/lib/snapd/desktop"] {
        donnees.push(PathBuf::from(d));
    }
    donnees.push(maison.join(".local/share/flatpak/exports/share"));
    let mut vus = std::collections::HashSet::new();
    let mut applis: Vec<Appli> = Vec::new();
    for d in &donnees {
        let Ok(contenu) = std::fs::read_dir(d.join("applications")) else { continue };
        for e in contenu.flatten() {
            let chemin = e.path();
            if chemin.extension().is_none_or(|x| x != "desktop") || !vus.insert(e.file_name()) {
                continue;
            }
            let Some((nom, icone)) = lire_desktop(&chemin) else { continue };
            let icone = icone.and_then(|i| trouver_icone(&i, &donnees));
            if let Some(a) = nouvelle_avec_icone(nom, chemin.to_string_lossy().into_owned(), icone)
                && !applis.iter().any(|b| b.cle == a.cle)
            {
                applis.push(a);
            }
        }
    }
    applis
}

/// (nom, icône) d'un fichier `.desktop`, ou rien si ce n'est pas une application à montrer. Le nom dans la
/// langue de l'appli s'il existe (`Name[fr]`).
#[cfg(target_os = "linux")]
fn lire_desktop(chemin: &Path) -> Option<(String, Option<String>)> {
    let texte = std::fs::read_to_string(chemin).ok()?;
    let langue = if ellipse_moteur::langue::francais() { "fr" } else { "en" };
    let (mut dedans, mut nom, mut nom_local, mut icone) = (false, None, None, None);
    for ligne in texte.lines() {
        let ligne = ligne.trim();
        if ligne.starts_with('[') {
            dedans = ligne == "[Desktop Entry]";
            continue;
        }
        let Some((cle, valeur)) = ligne.split_once('=') else { continue };
        if !dedans {
            continue;
        }
        match cle.trim() {
            "Type" if valeur.trim() != "Application" => return None,
            "NoDisplay" | "Hidden" if valeur.trim() == "true" => return None,
            "Name" => nom = Some(valeur.trim().to_owned()),
            "Icon" => icone = Some(valeur.trim().to_owned()),
            c if c == format!("Name[{langue}]") => nom_local = Some(valeur.trim().to_owned()),
            _ => {}
        }
    }
    Some((nom_local.or(nom)?, icone))
}

/// L'image PNG d'une icône nommée : chemin complet, ou cherchée dans le thème « hicolor » (le thème de
/// secours que toutes les applis remplissent) et dans `pixmaps`. Les icônes SVG seules ne sont pas prises.
#[cfg(target_os = "linux")]
fn trouver_icone(nom: &str, donnees: &[PathBuf]) -> Option<PathBuf> {
    let p = Path::new(nom);
    if p.is_absolute() {
        return (p.extension().is_some_and(|x| x == "png") && p.is_file()).then(|| p.to_path_buf());
    }
    let maison = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut bases: Vec<PathBuf> = vec![maison.join(".local/share/icons"), maison.join(".icons")];
    bases.extend(donnees.iter().map(|d| d.join("icons")));
    for taille in ["128x128", "96x96", "256x256", "64x64", "48x48"] {
        for b in &bases {
            let f = b.join("hicolor").join(taille).join("apps").join(format!("{nom}.png"));
            if f.is_file() {
                return Some(f);
            }
        }
    }
    let f = PathBuf::from("/usr/share/pixmaps").join(format!("{nom}.png"));
    f.is_file().then_some(f)
}

/// macOS : les `.app` des dossiers Applications (du système, de tous, de l'utilisateur).
#[cfg(target_os = "macos")]
fn lire() -> Vec<Appli> {
    let maison = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let dossiers = [
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
        maison.join("Applications"),
    ];
    let mut applis: Vec<Appli> = Vec::new();
    for d in dossiers {
        let Ok(contenu) = std::fs::read_dir(&d) else { continue };
        for e in contenu.flatten() {
            let chemin = e.path();
            if chemin.extension().is_none_or(|x| x != "app") {
                continue;
            }
            let nom = chemin.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(a) = nouvelle(nom, chemin.to_string_lossy().into_owned())
                && !applis.iter().any(|b| b.cle == a.cle)
            {
                applis.push(a);
            }
        }
    }
    applis
}

/// macOS : l'icône d'une `.app` (son .icns, nommé dans Info.plist), convertie une fois en PNG par `sips`.
#[cfg(target_os = "macos")]
fn icone_mac(app: &Path) -> Option<PathBuf> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    app.hash(&mut h);
    let png = std::env::temp_dir().join("ellipse-icones").join(format!("{:016x}.png", h.finish()));
    if png.is_file() {
        return Some(png);
    }
    let infos = app.join("Contents").join("Info");
    let sortie = ellipse_moteur::processus::commande("defaults").arg("read").arg(&infos).arg("CFBundleIconFile").output().ok()?;
    let mut nom = String::from_utf8_lossy(&sortie.stdout).trim().to_owned();
    if nom.is_empty() {
        return None;
    }
    if !nom.ends_with(".icns") {
        nom.push_str(".icns");
    }
    let icns = app.join("Contents").join("Resources").join(nom);
    std::fs::create_dir_all(png.parent()?).ok()?;
    let fait = ellipse_moteur::processus::commande("sips")
        .args(["-s", "format", "png", "-Z", "128"])
        .arg(&icns)
        .arg("--out")
        .arg(&png)
        .output()
        .ok()?;
    (fait.status.success() && png.is_file()).then_some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_trouve_google_chrome() {
        *APPLIS.write().unwrap() = ["Google Chrome", "Chrome Remote Desktop", "Visual Studio Code", "Uninstall Chrome", "Calculatrice"]
            .into_iter()
            .filter_map(|n| nouvelle(n.into(), format!(r"shell:AppsFolder\{n}")))
            .collect();
        let noms = |t: &str| chercher(t, 3).into_iter().map(|(_, a)| a.nom).collect::<Vec<_>>();
        APPLIS.write().unwrap().extend(nouvelle("Razer Chroma".into(), "x".into()));
        assert_eq!(noms("chrome"), ["Chrome Remote Desktop", "Google Chrome"]);
        assert_eq!(noms("google"), ["Google Chrome"]);
        assert_eq!(noms("vsc"), ["Visual Studio Code"]);
        assert_eq!(noms("calculatrise"), ["Calculatrice"]);
        assert!(noms("zzz").is_empty());
        // Une lettre au milieu d'un mot ne suffit pas ; au début d'un mot, si.
        assert!(noms("o").is_empty());
        assert_eq!(noms("ca"), ["Calculatrice"]);
    }
}
