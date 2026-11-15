//! Lancement des programmes du moteur (llama-server, ffmpeg, sherpa-onnx…).

use std::ffi::OsStr;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Sans fenêtre de console : l'appli n'en a pas, et chaque programme lancé en ouvrirait une sinon.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Priorité basse : l'indexation passe après tout ce que fait l'utilisateur.
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// Hors de Windows, la « gentillesse » (nice) d'un programme en priorité basse.
#[cfg(unix)]
const GENTILLESSE: libc::c_int = 10;

pub fn commande(programme: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(programme);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    lier_a_ellipse(&mut c);
    c
}

static RACINE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Le dossier d'Ellipse (outils, modèles), pour trouver ses programmes.
pub fn definir_racine(racine: &std::path::Path) {
    let _ = RACINE.set(racine.to_path_buf());
}

/// ffmpeg ou ffprobe : celui installé avec Ellipse (`outils/ffmpeg`), sinon celui du système.
pub fn ffmpeg(nom: &str) -> std::path::PathBuf {
    let exe = if cfg!(windows) { format!("{nom}.exe") } else { nom.to_owned() };
    RACINE
        .get()
        .map(|r| r.join("outils").join("ffmpeg").join(&exe))
        .filter(|p| p.exists())
        .unwrap_or_else(|| exe.into())
}

/// Pour le travail d'indexation (ffmpeg, llama-server d'indexation, transcription) : sans fenêtre et
/// en priorité basse, pour que le PC — et la barre de recherche — restent fluides.
pub fn commande_basse(programme: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(programme);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    #[cfg(unix)]
    {
        lier_a_ellipse(&mut c);
        use std::os::unix::process::CommandExt;
        // SAFETY : `setpriority` ne fait qu'appeler le système, ce qui est permis entre fork et exec.
        unsafe {
            c.pre_exec(|| {
                libc::setpriority(libc::PRIO_PROCESS, 0, GENTILLESSE);
                Ok(())
            });
        }
    }
    c
}

/// Linux : le programme lancé est arrêté si Ellipse disparaît, même brutalement (l'équivalent de
/// `lier_au_programme` sous Windows). macOS n'a pas cette possibilité : les serveurs sont arrêtés quand
/// Ellipse s'en sépare (Drop).
#[cfg(unix)]
fn lier_a_ellipse(c: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY : `prctl` ne fait qu'appeler le système, ce qui est permis entre fork et exec.
        unsafe {
            c.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = c;
}

/// Tout le programme en priorité basse (le prototype en ligne de commande, qui ne fait qu'indexer).
pub fn programme_en_priorite_basse() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass};
        // SAFETY : la pseudo-poignée du processus courant est toujours valide.
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }
    }
    #[cfg(unix)]
    // SAFETY : simple appel au système pour le processus courant.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, GENTILLESSE);
    }
}

/// Le fil qui appelle passe en priorité basse (fils de préparation de l'indexation).
pub fn fil_en_priorite_basse() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        // SAFETY : la pseudo-poignée du fil courant est toujours valide.
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
    }
    // Linux : la priorité s'applique au fil (identifiant du fil, pas du processus).
    #[cfg(target_os = "linux")]
    // SAFETY : simple appel au système pour le fil courant.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, GENTILLESSE);
    }
}

/// Lie les programmes lancés à celui-ci : quand il s'arrête, même brutalement (plantage, Ctrl+C,
/// fin de tâche), Windows arrête aussi llama-server, ffmpeg et la transcription. Sans cela, un
/// llama-server de 1,5 Go pourrait rester en mémoire. À appeler une fois, au démarrage. (Linux : chaque
/// programme est lié à son lancement, voir `lier_a_ellipse`.)
pub fn lier_au_programme() {
    #[cfg(windows)]
    {
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        // SAFETY : la poignée du job reste ouverte jusqu'à la fin du programme (c'est voulu : sa
        // fermeture, par Windows à la sortie, est ce qui arrête les programmes enfants).
        unsafe {
            let Ok(job) = CreateJobObjectW(None, None) else { return };
            let mut limites = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limites.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let _ = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limites as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            let _ = AssignProcessToJobObject(job, GetCurrentProcess());
        }
    }
}
