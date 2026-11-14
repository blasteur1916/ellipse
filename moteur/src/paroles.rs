//! Deuxième passe : transcrire la parole des audios et des vidéos, seulement là où quelqu'un parle.
//! Tout est déjà cherchable après la première passe ; celle-ci ajoute « ce qui est dit ».

use crate::base;
use crate::fichiers::tranches_audio;
use crate::mesures::secondes;
use crate::pertinence::Banque;
use crate::serveur::Client;
use crate::transcription::Transcripteur;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;
use crate::indexation::{Progres, Suivi};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

/// Le vecteur son d'EmbeddingGemma ressemble plus à ces phrases quand quelqu'un parle…
const PHRASES_PAROLE: [&str; 5] = [
    "une personne qui parle",
    "une conversation entre plusieurs personnes",
    "quelqu'un qui chante avec des paroles",
    "une voix qui explique quelque chose",
    "a person talking",
];
/// … et à celles-ci pour la musique, le bruit ou le silence.
const PHRASES_AUTRE: [&str; 6] = [
    "de la musique instrumentale sans paroles",
    "une mélodie jouée au piano ou au synthétiseur",
    "un bruit de fond",
    "un bip électronique",
    "le silence",
    "des bruits de la nature",
];
/// On transcrit si (ressemblance parole − ressemblance autre) dépasse ce seuil.
/// Mesuré en 256 dimensions : parole de −0,031 à +0,075 ; musique, bruit, bip de −0,048 à −0,098.
/// En cas de doute, on transcrit.
const SEUIL_PAROLE: f32 = -0.040;
const MOTS_MIN: usize = 3;

pub struct Bilan {
    pub fichiers: usize,
    pub secondes_audio: f64,
    pub duree: f64,
    /// Arrêtée avant la fin (pause, arrêt) : le reste sera transcrit la prochaine fois.
    pub interrompue: bool,
}

/// Transcriptions en parallèle et cœurs par transcription, selon le processeur :
/// 4 cœurs logiques → 1 × 2, 8 → 1 × 4, 16 → 2 × 4, 24 et plus → 3 × 4.
pub fn reglage_selon_pc() -> (usize, usize) {
    let coeurs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let requetes = (coeurs / 8).clamp(1, 3);
    (requetes, (coeurs / (2 * requetes)).clamp(1, 4))
}

struct Travail {
    fichier_id: i64,
    chemin: String,
    /// Tranches (début, fin) où quelqu'un parle.
    tranches: Vec<(f64, f64)>,
}

#[allow(clippy::too_many_arguments)]
pub fn transcrire(
    base: &mut Connection,
    modele: &Client,
    banque: &Banque,
    racine: &Path,
    journal: &Path,
    arret: &Arc<AtomicBool>,
    suspendre: &Arc<AtomicBool>,
    suivi: Option<&Suivi>,
) -> Result<Option<Bilan>> {
    let candidats = base::a_transcrire(base)?;
    if candidats.is_empty() {
        return Ok(None);
    }
    // L'avancement compte aussi ce qui a été transcrit les fois précédentes.
    let deja = base::nb_transcrits(base)?;
    let a_faire = candidats.len();
    let debut = Instant::now();
    let phrases = |liste: &[&str]| -> Result<Vec<Vec<f32>>> {
        modele.vecteurs(liste.iter().map(|p| Value::String(format!("task: search result | query: {p}"))).collect())
    };
    let (parole, autre) = (phrases(&PHRASES_PAROLE)?, phrases(&PHRASES_AUTRE)?);
    let ressemblance = |v: &[f32], liste: &[Vec<f32>]| {
        liste.iter().map(|p| p.iter().zip(v).map(|(a, b)| a * b).sum::<f32>()).fold(f32::MIN, f32::max)
    };

    let mut tranches = 0;
    let mut travaux = Vec::new();
    for c in candidats {
        tranches += c.tranches.len();
        let gardees: Vec<(f64, f64)> = c
            .tranches
            .iter()
            .filter(|(_, _, v)| ressemblance(v, &parole) - ressemblance(v, &autre) > SEUIL_PAROLE)
            .map(|&(d, f, _)| (d, f))
            .collect();
        if gardees.is_empty() {
            base::ajouter_paroles(base, c.fichier_id, &[], 0.0, banque)?;
        } else {
            travaux.push(Travail { fichier_id: c.fichier_id, chemin: c.chemin, tranches: gardees });
        }
    }
    let avec_parole: usize = travaux.iter().map(|t| t.tranches.len()).sum();
    let secondes_audio: f64 = travaux.iter().flat_map(|t| &t.tranches).map(|(d, f)| f - d).sum();
    let reglage = reglage_selon_pc();
    println!(
        "\n== Passe 2 : transcription ==\n{avec_parole} tranche(s) de 30 s avec de la parole sur {tranches} ; le reste (musique, bruit) est sauté"
    );
    let fichiers = travaux.len();
    let sans_parole = a_faire - fichiers;
    let bilan = |duree, interrompue| Bilan { fichiers, secondes_audio, duree, interrompue };
    if travaux.is_empty() {
        return Ok(Some(bilan(debut.elapsed().as_secs_f64(), false)));
    }

    let (requetes, coeurs) = reglage;
    let transcripteur = Transcripteur::demarrer(racine, requetes, coeurs, journal)?;
    println!(
        "Parakeet TDT v3 chargé en {}, {requetes} transcription(s) en parallèle × {coeurs} cœurs",
        secondes(transcripteur.demarrage.as_secs_f64() * 1000.0)
    );

    // Chaque fil décode un fichier, transcrit ses tranches de parole et renvoie les textes.
    let travaux = Arc::new(travaux);
    let suivant = Arc::new(AtomicUsize::new(0));
    let (envoi, reception) = mpsc::channel::<(usize, Result<Vec<(f64, f64, String)>>, f64)>();
    for _ in 0..requetes {
        let (travaux, suivant, envoi, client, arret, suspendre) =
            (travaux.clone(), suivant.clone(), envoi.clone(), transcripteur.client(), arret.clone(), suspendre.clone());
        std::thread::spawn(move || {
            crate::processus::fil_en_priorite_basse();
            loop {
                // Arrêt ou pause : les fichiers déjà transcrits sont enregistrés, les autres le seront la prochaine
                // fois (en pause, rien ne doit rester chargé : le transcripteur et le modèle sont libérés).
                if arret.load(Ordering::Relaxed) || suspendre.load(Ordering::Relaxed) {
                    break;
                }
                let i = suivant.fetch_add(1, Ordering::Relaxed);
                let Some(t) = travaux.get(i) else { break };
                let debut = Instant::now();
                let resultat = tranches_audio(Path::new(&t.chemin)).and_then(|audio| {
                    let mut textes = Vec::new();
                    for (d, f, echantillons) in audio {
                        if t.tranches.iter().any(|&(dt, _)| (dt - d).abs() < 0.05) {
                            let texte = client.transcrire(&echantillons)?;
                            if texte.split_whitespace().count() >= MOTS_MIN {
                                textes.push((d, f, texte));
                            }
                        }
                    }
                    Ok(textes)
                });
                if envoi.send((i, resultat, debut.elapsed().as_secs_f64() * 1000.0)).is_err() {
                    break;
                }
            }
        });
    }
    drop(envoi);

    let total = travaux.len();
    let mut recus = 0;
    for (k, (i, resultat, ms)) in reception.into_iter().enumerate() {
        recus += 1;
        let t = &travaux[i];
        let rang = format!("[{:>w$}/{total}]", k + 1, w = total.to_string().len());
        if let Some(s) = suivi {
            s(Progres::Transcription { faits: deja + sans_parole + k + 1, total: deja + a_faire });
        }
        match resultat.and_then(|textes| {
            let titre = Path::new(&t.chemin).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut paroles = Vec::new();
            for lot in textes.chunks(8) {
                let entrees = lot.iter().map(|(_, _, x)| Value::String(format!("title: {titre} | text: {x}"))).collect();
                let vecteurs = modele.vecteurs(entrees)?;
                paroles.extend(lot.iter().zip(vecteurs).map(|((d, f, x), v)| (*d, *f, v, x.clone())));
            }
            base::ajouter_paroles(base, t.fichier_id, &paroles, ms, banque)?;
            Ok(paroles.len())
        }) {
            Ok(n) => println!("{rang} transcription {}  {n} passage(s) avec des mots  {}", secondes(ms), t.chemin),
            // Le fichier reste « à transcrire » : on réessaiera à la prochaine indexation.
            Err(e) => println!("{rang} transcription ERREUR  {} : {e:#}", t.chemin),
        }
    }
    Ok(Some(bilan(debut.elapsed().as_secs_f64(), recus < total)))
}
