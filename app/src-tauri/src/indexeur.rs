//! L'indexation en arrière-plan. Un gardien la lance (au démarrage, quand les réglages changent, puis
//! toutes les 30 minutes pour attraper les nouveaux fichiers) et la met en pause dès qu'il faut : batterie,
//! jeu ou plein écran, pause demandée. En pause, son llama-server est arrêté (la carte graphique est libérée
//! pendant un jeu) mais rien n'est oublié : elle reprend où elle en était, sans reparcourir les disques.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use ellipse_moteur::indexation::{self, Fin, Progres};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{installation, plateforme, recherche, reglages};
use ellipse_moteur::installation::Groupe;

pub const EVT_ETAT: &str = "ellipse://indexation";
/// Le lancement de l'appli reste léger : l'indexation attend un peu.
const ATTENTE_AU_DEMARRAGE: Duration = Duration::from_secs(8);
/// Pour attraper les fichiers nouveaux ou modifiés (en attendant de suivre le disque en direct, étape 5).
const INTERVALLE: Duration = Duration::from_secs(30 * 60);
/// Après une erreur, on réessaie plus tard.
const APRES_ERREUR: Duration = Duration::from_secs(5 * 60);
/// Batterie, jeu, pause : vérifiés à ce rythme pendant l'indexation.
const VERIFICATION: Duration = Duration::from_secs(3);
/// L'interface reçoit l'avancement au plus 4 fois par seconde.
const PUBLICATION_MIN: Duration = Duration::from_millis(250);
/// Un jeu ne met l'indexation en pause qu'après ce temps (un Alt+Tab ou une vidéo d'un instant ne compte
/// pas), et elle ne reprend qu'après ce temps sans jeu (on revient souvent au jeu juste après).
const JEU_AVANT_PAUSE: Duration = Duration::from_secs(20);
const SANS_JEU_AVANT_REPRISE: Duration = Duration::from_secs(60);

/// Miroir de `EtatIndexation` dans `src/ipc/indexation.ts`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Etat {
    /// Pas encore commencé, ou aucun dossier.
    Attente,
    Parcours,
    Lecture { faits: usize, total: usize },
    Visages { faits: usize, total: usize },
    Transcription { faits: usize, total: usize },
    Pause { raison: String },
    AJour,
    Erreur { message: String },
}

struct Partage {
    etat: Etat,
    publie: Option<Instant>,
    /// Quelque chose a changé (réglages, « indexer maintenant ») : relancer sans attendre.
    demande: bool,
    /// L'indexation en cours : la mettre à vrai l'arrête.
    arret: Option<Arc<AtomicBool>>,
    /// L'indexation en cours : la mettre à vrai la met en pause.
    suspendre: Option<Arc<AtomicBool>>,
    /// « Indexer maintenant » : reparcourir les disques au lieu de reprendre le parcours gardé.
    reparcourir: bool,
}

static PARTAGE: LazyLock<(Mutex<Partage>, Condvar)> = LazyLock::new(|| {
    let p = Partage { etat: Etat::Attente, publie: None, demande: false, arret: None, suspendre: None, reparcourir: false };
    (Mutex::new(p), Condvar::new())
});
static PAUSE_MANUELLE: AtomicBool = AtomicBool::new(false);

/// Où en est la détection des jeux : en pause pour un jeu, et depuis quand la mesure dit le contraire.
struct Jeu {
    en_pause: bool,
    depuis: Option<Instant>,
}
static JEU: Mutex<Jeu> = Mutex::new(Jeu { en_pause: false, depuis: None });

fn partage() -> std::sync::MutexGuard<'static, Partage> {
    PARTAGE.0.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn demarrer(app: &AppHandle) {
    // Mode de test (index d'essai) : rien n'est indexé, et la barre n'affiche aucun état d'indexation.
    if std::env::var_os("ELLIPSE_TEST").is_some() {
        return;
    }
    let app = app.clone();
    let lance = std::thread::Builder::new().name("ellipse-indexation".into()).spawn(move || gardien(&app));
    if let Err(e) = lance {
        log::error!("l'indexation n'a pas pu démarrer : {e}");
    }
}

/// Publie l'état à l'interface (les étapes d'avancement au plus 4 fois par seconde).
fn publier(app: &AppHandle, etat: Etat) {
    let mut p = partage();
    let meme_phase = std::mem::discriminant(&p.etat) == std::mem::discriminant(&etat);
    let trop_tot = p.publie.is_some_and(|t| t.elapsed() < PUBLICATION_MIN);
    let fin = matches!(etat, Etat::Lecture { faits, total } | Etat::Visages { faits, total }
        | Etat::Transcription { faits, total } if faits == total);
    p.etat = etat.clone();
    if meme_phase && trop_tot && !fin {
        return;
    }
    p.publie = Some(Instant::now());
    drop(p);
    let _ = app.emit(EVT_ETAT, etat);
}

/// Pourquoi il faut être en pause maintenant, s'il le faut. `pendant` : une indexation tourne, un jeu ne
/// compte qu'après `JEU_AVANT_PAUSE` et la pause dure jusqu'à `SANS_JEU_AVANT_REPRISE` sans jeu.
fn raison_de_pause(r: &reglages::Reglages, pendant: bool) -> Option<&'static str> {
    if PAUSE_MANUELLE.load(Ordering::Relaxed) {
        Some("manuelle")
    } else if r.pause_batterie && plateforme::sur_batterie() {
        Some("batterie")
    } else if r.pause_jeu && jeu(pendant) {
        Some("jeu")
    } else {
        None
    }
}

fn jeu(pendant: bool) -> bool {
    let mesure = plateforme::jeu_en_cours();
    let mut j = JEU.lock().unwrap_or_else(|e| e.into_inner());
    if !pendant || mesure == j.en_pause {
        *j = Jeu { en_pause: mesure, depuis: None };
        return mesure;
    }
    let delai = if j.en_pause { SANS_JEU_AVANT_REPRISE } else { JEU_AVANT_PAUSE };
    if j.depuis.get_or_insert_with(Instant::now).elapsed() >= delai {
        *j = Jeu { en_pause: mesure, depuis: None };
    }
    j.en_pause
}

/// Attend au plus `duree`, ou qu'on demande une indexation. Vrai si elle a été demandée.
fn attendre(duree: Duration) -> bool {
    let (verrou, cond) = &*PARTAGE;
    let p = verrou.lock().unwrap_or_else(|e| e.into_inner());
    let (mut p, _) = cond.wait_timeout_while(p, duree, |p| !p.demande).unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut p.demande)
}

fn gardien(app: &AppHandle) {
    let mut demande = attendre(ATTENTE_AU_DEMARRAGE);
    let mut prochaine = Instant::now();
    loop {
        let r = reglages::lire();
        // Premier lancement : rien à faire tant que les outils et le modèle ne sont pas téléchargés.
        if !installation::essentiel_pret() {
            publier(app, Etat::Attente);
            demande |= attendre(Duration::from_secs(5));
            continue;
        }
        // Un réglage vient d'activer la transcription ou les visages : leurs modèles arrivent en arrière-plan.
        if matches!(installation::etat_installation(), installation::Etat::AInstaller { .. }) {
            installation::installer();
        }
        if let Some(raison) = raison_de_pause(&r, false) {
            publier(app, Etat::Pause { raison: raison.into() });
            demande |= attendre(VERIFICATION);
            continue;
        }
        if r.dossiers.is_empty() {
            publier(app, Etat::Attente);
            demande = attendre(Duration::from_secs(3600));
            continue;
        }
        if !demande && Instant::now() < prochaine {
            demande = attendre(prochaine - Instant::now());
            continue;
        }
        demande = false;

        let arret = Arc::new(AtomicBool::new(false));
        let suspendre = Arc::new(AtomicBool::new(false));
        let reparcourir = {
            let mut p = partage();
            p.arret = Some(arret.clone());
            p.suspendre = Some(suspendre.clone());
            std::mem::take(&mut p.reparcourir)
        };
        // Le dernier avancement, remontré à l'interface quand la pause finit.
        let dernier: Arc<Mutex<Etat>> = Arc::new(Mutex::new(Etat::Parcours));
        // Le guetteur met l'indexation en pause dès qu'une raison apparaît, et la fait reprendre ensuite.
        let fini = Arc::new(AtomicBool::new(false));
        {
            let (suspendre, fini, dernier, app) = (suspendre.clone(), fini.clone(), dernier.clone(), app.clone());
            std::thread::spawn(move || {
                while !fini.load(Ordering::Relaxed) {
                    std::thread::sleep(VERIFICATION);
                    let raison = raison_de_pause(&reglages::lire(), true);
                    let avant = suspendre.swap(raison.is_some(), Ordering::Relaxed);
                    match raison {
                        Some(raison) if !avant => {
                            log::info!("indexation en pause ({raison})");
                            publier(&app, Etat::Pause { raison: raison.into() });
                        }
                        None if avant => {
                            log::info!("indexation reprise");
                            let etat = dernier.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            publier(&app, etat);
                        }
                        _ => {}
                    }
                }
            });
        }
        let (suivi_app, suivi_suspendre) = (app.clone(), suspendre.clone());
        let suivi: indexation::Suivi = Arc::new(move |p: Progres| {
            let etat = match p {
                Progres::Parcours => Etat::Parcours,
                Progres::Lecture { faits, total } => Etat::Lecture { faits, total },
                Progres::Visages { faits, total } => Etat::Visages { faits, total },
                Progres::Transcription { faits, total } => Etat::Transcription { faits, total },
            };
            *dernier.lock().unwrap_or_else(|e| e.into_inner()) = etat.clone();
            // En pause, l'interface garde « en pause » : les derniers fichiers finis ne la remplacent pas.
            if !suivi_suspendre.load(Ordering::Relaxed) {
                publier(&suivi_app, etat);
            }
        });
        let (racine, base) = recherche::racine_et_base();
        let debut = Instant::now();
        let resultat = indexation::indexer(
            &racine,
            &base,
            &r.dossiers,
            indexation::Options {
                cpu: false,
                tout: false,
                avec_ocr: r.ocr,
                avec_transcription: r.transcription && installation::groupe_pret(Groupe::Transcription),
                avec_visages: r.visages && installation::groupe_pret(Groupe::Visages),
                fils: 0,
                jetons_image: None,
                arret: arret.clone(),
                suspendre: suspendre.clone(),
                // Reprendre le parcours gardé par une indexation interrompue (sauf « Indexer maintenant »).
                reprise: !reparcourir,
                suivi: Some(suivi),
                bibliotheques: r.bibliotheques,
                noms: false,
            },
        );
        fini.store(true, Ordering::Relaxed);
        {
            let mut p = partage();
            p.arret = None;
            p.suspendre = None;
        }
        recherche::rafraichir_si_change();
        match resultat {
            // Arrêtée ou en pause (réglages changés, transcription en pause) : elle reprendra dès que possible,
            // là où elle en était.
            Ok(Fin::Interrompue) => {
                log::info!("indexation interrompue après {} s", debut.elapsed().as_secs());
                demande = true;
            }
            Ok(Fin::Complete) => {
                log::info!("indexation à jour en {} s", debut.elapsed().as_secs());
                publier(app, Etat::AJour);
                prochaine = Instant::now() + INTERVALLE;
            }
            Err(e) => {
                log::error!("indexation : {e:#}");
                publier(app, Etat::Erreur { message: format!("{e:#}") });
                prochaine = Instant::now() + APRES_ERREUR;
            }
        }
    }
}

/// Les réglages ont changé : l'indexation en cours s'arrête et repart avec les nouveaux.
pub fn reglages_changes() {
    let (_, cond) = &*PARTAGE;
    let mut p = partage();
    if let Some(a) = &p.arret {
        a.store(true, Ordering::Relaxed);
    }
    p.demande = true;
    cond.notify_all();
}

/// Un dossier retiré des réglages : ses fichiers sortent de l'index (sur un fil à part).
pub fn oublier(dossier: PathBuf) {
    std::thread::spawn(move || {
        let (_, base) = recherche::racine_et_base();
        let r = ellipse_moteur::base::ouvrir(&base)
            .and_then(|mut b| ellipse_moteur::base::oublier_dossier(&mut b, &dossier.to_string_lossy()));
        if let Err(e) = r {
            log::warn!("dossier retiré mais pas oublié de l'index : {e:#}");
        }
        recherche::rafraichir_si_change();
    });
}

#[tauri::command]
pub fn etat_indexation() -> Etat {
    partage().etat.clone()
}

/// Relance l'indexation tout de suite, en reparcourant les disques (nouveaux fichiers).
#[tauri::command]
pub fn indexer_maintenant() {
    PAUSE_MANUELLE.store(false, Ordering::Relaxed);
    partage().reparcourir = true;
    reglages_changes();
}

/// Met l'indexation en pause, ou la reprend.
#[tauri::command]
pub fn basculer_pause(app: AppHandle) -> Etat {
    let pause = !PAUSE_MANUELLE.fetch_xor(true, Ordering::Relaxed);
    let (_, cond) = &*PARTAGE;
    let mut p = partage();
    match &p.suspendre {
        // La pause, tout de suite. La reprise, c'est le guetteur qui la fait (et remontre l'avancement).
        Some(s) if pause => s.store(true, Ordering::Relaxed),
        Some(_) => {}
        // Pas d'indexation en cours (la pause l'avait empêchée de commencer) : le gardien repart.
        None if !pause => {
            p.demande = true;
            cond.notify_all();
        }
        None => {}
    }
    drop(p);
    if pause {
        publier(&app, Etat::Pause { raison: "manuelle".into() });
    }
    etat_indexation()
}
