//! Les miniatures de Windows, les mêmes que l'Explorateur : l'aperçu des photos, PDF, vidéos et documents,
//! l'icône du type pour le reste. Linux et macOS : faites ici pour les photos, les PDF et les vidéos, l'icône
//! pour les applications ; rien pour le reste (l'interface montre alors le pictogramme du type).
//! Une à la fois, sur un seul fil : certains générateurs d'aperçus échouent s'ils sont appelés de
//! plusieurs fils en même temps. Servies à l'interface par l'adresse `http://ellipse-mini.localhost/<clé>`.

use std::collections::{HashMap, VecDeque};
#[cfg(windows)]
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use tauri::http::{Response, StatusCode, header};
#[cfg(windows)]
use windows::Win32::Foundation::SIZE;
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP,
    ReleaseDC,
};
#[cfg(windows)]
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_INCACHEONLY,
    SIIGBF_RESIZETOFIT,
};
#[cfg(windows)]
use windows::core::HSTRING;

pub const SCHEMA: &str = "ellipse-mini";
/// Côté en pixels : la miniature fait 40 px à l'écran, deux fois plus pour les écrans à haute densité.
const COTE: i32 = 80;
/// Miniatures gardées en mémoire (≈ 10 Ko chacune) : retaper une recherche ne les refait pas.
const EN_MEMOIRE: usize = 400;

/// Côté de la grande miniature (aperçu d'une vidéo, d'un fichier sans meilleur aperçu).
const COTE_GRAND: i32 = 640;

/// L'adresse de la miniature d'un fichier proposé.
pub fn adresse(cle: &str) -> String {
    format!("{}{cle}", crate::apercus::base_adresse(SCHEMA))
}

/// La même, en grand (pas gardée en mémoire ici : la WebView la garde un moment).
pub fn adresse_grande(cle: &str) -> String {
    format!("{}{cle}?grand", crate::apercus::base_adresse(SCHEMA))
}

type Demande = (PathBuf, String, i32, tauri::UriSchemeResponder);

static FIL: OnceLock<Sender<Demande>> = OnceLock::new();
static CACHE: LazyLock<Mutex<(HashMap<String, Arc<Vec<u8>>>, VecDeque<String>)>> = LazyLock::new(Default::default);

/// Répond à une demande `ellipse-mini` ; `chemin` est celui que la recherche a proposé pour cette clé.
pub fn servir(chemin: Option<PathBuf>, cle: String, grand: bool, repondeur: tauri::UriSchemeResponder) {
    let Some(chemin) = chemin else {
        repondeur.respond(reponse(None));
        return;
    };
    let cote = if grand { COTE_GRAND } else { COTE };
    if !grand && let Some(png) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).0.get(&cle).cloned() {
        repondeur.respond(reponse(Some(png.to_vec())));
        return;
    }
    let fil = FIL.get_or_init(|| {
        let (envoi, reception) = mpsc::channel::<Demande>();
        let lance = std::thread::Builder::new().name("ellipse-miniatures".into()).spawn(move || {
            for (chemin, cle, cote, repondeur) in reception {
                let png = miniature(&chemin, cote);
                if let Some(png) = &png
                    && cote == COTE
                {
                    garder(cle, png.clone());
                }
                repondeur.respond(reponse(png));
            }
        });
        if let Err(e) = lance {
            log::error!("le fil des miniatures n'a pas pu démarrer : {e}");
        }
        envoi
    });
    if let Err(mpsc::SendError((_, _, _, repondeur))) = fil.send((chemin, cle, cote, repondeur)) {
        repondeur.respond(reponse(None));
    }
}

fn garder(cle: String, png: Vec<u8>) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let (table, ordre) = &mut *cache;
    if table.insert(cle.clone(), Arc::new(png)).is_none() {
        ordre.push_back(cle);
        while ordre.len() > EN_MEMOIRE {
            if let Some(vieille) = ordre.pop_front() {
                table.remove(&vieille);
            }
        }
    }
}

fn reponse(image: Option<Vec<u8>>) -> Response<Vec<u8>> {
    let r = match image {
        // PNG en général ; JPEG pour une page de PDF ou une image de vidéo (Linux, macOS).
        Some(image) => Response::builder()
            .header(header::CONTENT_TYPE, if image.starts_with(&[0xFF, 0xD8]) { "image/jpeg" } else { "image/png" })
            .header(header::CACHE_CONTROL, "max-age=3600")
            .body(image),
        // Pas gardée par la WebView : elle viendra peut-être plus tard.
        None => Response::builder().status(StatusCode::NOT_FOUND).header(header::CACHE_CONTROL, "no-store").body(Vec::new()),
    };
    r.unwrap_or_else(|_| Response::new(Vec::new()))
}

/// La miniature PNG de `chemin`, au plus `cote` × `cote` pixels.
#[cfg(windows)]
fn miniature(chemin: &Path, cote: i32) -> Option<Vec<u8>> {
    // SAFETY : COM est initialisé pour ce fil (sans effet s'il l'est déjà) ; l'image est créée par
    // l'interpréteur de commandes et libérée ici.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // L'interpréteur ne comprend que les barres obliques inverses.
        let nom = chemin.to_string_lossy().replace('/', "\\");
        let fabrique: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(nom), None).ok()?;
        let taille = SIZE { cx: cote, cy: cote };
        // Un fichier seulement en ligne (OneDrive, téléphone relié) : la miniature déjà connue de Windows, sans
        // quoi le calculer téléchargerait le fichier.
        let mut options = SIIGBF_RESIZETOFIT | SIIGBF_BIGGERSIZEOK;
        if ellipse_moteur::fichiers::en_ligne(chemin) {
            options |= SIIGBF_INCACHEONLY;
        }
        // Certains générateurs d'aperçus échouent au lieu de se rabattre sur l'icône : on la demande alors.
        let image = fabrique
            .GetImage(taille, options)
            .or_else(|_| fabrique.GetImage(taille, SIIGBF_ICONONLY))
            .ok()?;
        let pixels = lire(image);
        let _ = DeleteObject(image.into());
        let (largeur, hauteur, rgba) = pixels?;
        png(largeur, hauteur, &rgba)
    }
}

/// Les pixels en RGBA (alpha non prémultiplié), ligne du haut d'abord.
#[cfg(windows)]
unsafe fn lire(image: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut info = BITMAP::default();
        let lu = GetObjectW(image.into(), size_of::<BITMAP>() as i32, Some(&mut info as *mut BITMAP as *mut c_void));
        if lu == 0 || info.bmWidth <= 0 || info.bmHeight == 0 {
            return None;
        }
        let (l, h) = (info.bmWidth, info.bmHeight.abs());
        let mut entete = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: l,
                // Négatif : lignes de haut en bas.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; l as usize * h as usize * 4];
        let dc = GetDC(None);
        let lignes = GetDIBits(dc, image, 0, h as u32, Some(bgra.as_mut_ptr().cast()), &mut entete, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lignes == 0 {
            return None;
        }
        // BGRA à alpha prémultiplié → RGBA. Sans alpha (tout à 0), l'image est opaque.
        let sans_alpha = bgra.chunks_exact(4).all(|p| p[3] == 0);
        for p in bgra.chunks_exact_mut(4) {
            p.swap(0, 2);
            if sans_alpha {
                p[3] = 255;
            } else if p[3] > 0 && p[3] < 255 {
                let a = u32::from(p[3]);
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        Some((l as u32, h as u32, bgra))
    }
}

fn png(largeur: u32, hauteur: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut sortie = Vec::new();
    {
        let mut encodeur = png::Encoder::new(&mut sortie, largeur, hauteur);
        encodeur.set_color(png::ColorType::Rgba);
        encodeur.set_depth(png::BitDepth::Eight);
        encodeur.set_compression(png::Compression::Fast);
        let mut ecrivain = encodeur.write_header().ok()?;
        ecrivain.write_image_data(rgba).ok()?;
    }
    Some(sortie)
}

/// Linux et macOS : l'icône d'une application, une photo réduite, la première page d'un PDF ou une image
/// d'une vidéo (à 1 s) ; rien pour les autres types.
#[cfg(not(windows))]
fn miniature(chemin: &Path, cote: i32) -> Option<Vec<u8>> {
    use ellipse_moteur::fichiers::{self, Genre};
    if let Some(icone) = crate::applications::icone(chemin) {
        return std::fs::read(icone).ok();
    }
    let cote = cote as u32;
    match Genre::de(chemin)? {
        Genre::Photo => {
            let images = ellipse_moteur::photo::Images::nouveau().ok()?;
            let source = images.ouvrir(chemin).ok()?;
            let p = images.pixels(&source, cote, true).ok()?;
            // BGRA → RGBA.
            let rgba: Vec<u8> = p.octets.chunks_exact(4).flat_map(|b| [b[2], b[1], b[0], b[3]]).collect();
            png(p.largeur, p.hauteur, &rgba)
        }
        Genre::Pdf => {
            let (racine, _) = crate::recherche::racine_et_base();
            let pdfium = fichiers::charger_pdfium(&racine).ok()?;
            fichiers::rendre_page(&pdfium, chemin, 1, cote).ok().map(|(jpeg, _)| jpeg)
        }
        Genre::Video => {
            let sortie = ellipse_moteur::processus::commande(ellipse_moteur::processus::ffmpeg("ffmpeg"))
                .args(["-v", "error", "-ss", "1", "-i"])
                .arg(chemin)
                .args(["-frames:v", "1", "-vf", &format!("scale={cote}:-2"), "-f", "image2pipe", "-c:v", "mjpeg", "-"])
                .stdin(std::process::Stdio::null())
                .output()
                .ok()?;
            (!sortie.stdout.is_empty()).then_some(sortie.stdout)
        }
        _ => None,
    }
}
