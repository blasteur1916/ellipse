//! Mémoire vive (pics) et mémoire de la carte graphique (nvidia-smi).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Pic de mémoire vive (working set) d'un processus Windows, en octets.
#[cfg(windows)]
fn pic_memoire(processus: windows::Win32::Foundation::HANDLE) -> u64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    let mut c = PROCESS_MEMORY_COUNTERS::default();
    let taille = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY : la poignée est valide tant que le processus l'est ; la structure est locale.
    match unsafe { GetProcessMemoryInfo(processus, &mut c, taille) } {
        Ok(()) => c.PeakWorkingSetSize as u64,
        Err(_) => 0,
    }
}

/// Pic de mémoire vive d'un programme lancé par Ellipse, en octets (0 si inconnu).
pub fn pic_memoire_enfant(enfant: &std::process::Child) -> u64 {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        pic_memoire(windows::Win32::Foundation::HANDLE(enfant.as_raw_handle()))
    }
    // Linux : « VmHWM », le plus haut de la mémoire résidente, en Ko.
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{}/status", enfant.id()))
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()))
            .map_or(0, |ko| ko << 10)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = enfant;
        0
    }
}

pub fn pic_memoire_programme() -> u64 {
    #[cfg(windows)]
    {
        // SAFETY : la pseudo-poignée du processus courant est toujours valide.
        pic_memoire(unsafe { windows::Win32::System::Threading::GetCurrentProcess() })
    }
    #[cfg(unix)]
    {
        // SAFETY : `getrusage` remplit une structure locale.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
            return 0;
        }
        // En Ko sous Linux, en octets sur macOS.
        let max = u.ru_maxrss as u64;
        if cfg!(target_os = "macos") { max } else { max << 10 }
    }
}

/// Mémoire utilisée sur la première carte NVIDIA, en octets (None sans nvidia-smi).
fn vram_utilisee() -> Option<u64> {
    let sortie = crate::processus::commande("nvidia-smi")
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits", "-i", "0"])
        .output()
        .ok()?;
    let mio: u64 = String::from_utf8_lossy(&sortie.stdout).trim().parse().ok()?;
    Some(mio << 20)
}

/// Relève la mémoire de la carte graphique chaque seconde et garde le pic.
pub struct SuiviVram {
    pub depart: Option<u64>,
    pic: Arc<AtomicU64>,
    arret: Arc<AtomicBool>,
    fil: Option<JoinHandle<()>>,
}

impl SuiviVram {
    pub fn demarrer() -> Self {
        let depart = vram_utilisee();
        let pic = Arc::new(AtomicU64::new(depart.unwrap_or(0)));
        let arret = Arc::new(AtomicBool::new(false));
        let fil = depart.map(|_| {
            let (pic, arret) = (pic.clone(), arret.clone());
            std::thread::spawn(move || {
                while !arret.load(Ordering::Relaxed) {
                    if let Some(v) = vram_utilisee() {
                        pic.fetch_max(v, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            })
        });
        SuiviVram { depart, pic, arret, fil }
    }

    /// Pic au-dessus de la mémoire déjà utilisée au départ, en octets.
    pub fn arreter(mut self) -> Option<u64> {
        self.arret.store(true, Ordering::Relaxed);
        if let Some(f) = self.fil.take() {
            let _ = f.join();
        }
        self.depart.map(|d| self.pic.load(Ordering::Relaxed).saturating_sub(d))
    }
}

/// Place prise par l'index (la base et ses fichiers de journal SQLite), en octets.
pub fn taille_base(chemin: &std::path::Path) -> u64 {
    ["", "-wal", "-shm"]
        .iter()
        .filter_map(|s| std::fs::metadata(format!("{}{s}", chemin.display())).ok())
        .map(|m| m.len())
        .sum()
}

pub fn mo(octets: u64) -> String {
    if octets < 10 << 20 {
        format!("{} Ko", octets >> 10)
    } else {
        format!("{} Mo", octets >> 20)
    }
}

pub fn secondes(ms: f64) -> String {
    format!("{:.2} s", ms / 1000.0).replace('.', ",")
}
