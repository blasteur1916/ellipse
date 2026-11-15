//! Ce que l'indexation doit savoir du système pour se mettre en pause : le PC est-il sur batterie, un jeu
//! (ou une appli en plein écran) est-il au premier plan, un autre programme fait-il travailler la carte
//! graphique ? Windows, et Linux pour la batterie.

/// Le PC fonctionne sur batterie (secteur débranché).
pub fn sur_batterie() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut etat = SYSTEM_POWER_STATUS::default();
        // SAFETY : simple lecture dans une structure locale.
        // ACLineStatus : 0 = sur batterie, 1 = sur secteur, 255 = inconnu (PC fixe : jamais en pause).
        unsafe { GetSystemPowerStatus(&mut etat).is_ok() && etat.ACLineStatus == 0 }
    }
    #[cfg(target_os = "linux")]
    {
        // Une alimentation secteur (« Mains ») débranchée, alors qu'il y a une batterie.
        let Ok(alims) = std::fs::read_dir("/sys/class/power_supply") else { return false };
        let mut secteur_branche = None;
        let mut batterie = false;
        for a in alims.flatten() {
            let lire = |f: &str| std::fs::read_to_string(a.path().join(f)).unwrap_or_default().trim().to_owned();
            match lire("type").as_str() {
                "Mains" => secteur_branche = Some(secteur_branche.unwrap_or(false) || lire("online") == "1"),
                "Battery" => batterie = true,
                _ => {}
            }
        }
        batterie && secteur_branche == Some(false)
    }
    // macOS : « pmset » dit d'où vient le courant (« AC Power » ou « Battery Power »).
    #[cfg(target_os = "macos")]
    {
        ellipse_moteur::processus::commande("pmset")
            .args(["-g", "batt"])
            .output()
            .is_ok_and(|s| String::from_utf8_lossy(&s.stdout).contains("Battery Power"))
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// Un jeu, une vidéo ou une présentation occupe tout l'écran de l'île, ou un autre programme fait travailler
/// la carte graphique (un jeu en fenêtre ou en arrière-plan). Ailleurs que sous Windows : pas encore détecté.
pub fn jeu_en_cours() -> bool {
    #[cfg(windows)]
    {
        windows_plein_ecran::premier_plan_en_plein_ecran() || carte_graphique::occupee_par_un_autre()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod windows_plein_ecran {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Shell::{QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowLongPtrW,
        GetWindowRect, GetWindowThreadProcessId, IsZoomed, WS_EX_TOOLWINDOW,
    };

    /// Fenêtres de l'interpréteur de commandes qui couvrent l'écran sans être une appli.
    const CLASSES_SHELL: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

    pub fn premier_plan_en_plein_ecran() -> bool {
        // SAFETY : simples requêtes Win32 sur des poignées de fenêtres ; une poignée périmée les fait
        // échouer, ce qui se lit « pas en plein écran ».
        unsafe {
            // Direct3D exclusif ou mode présentation : Windows le dit lui-même.
            if SHQueryUserNotificationState().is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE) {
                return true;
            }
            let fg = GetForegroundWindow();
            if fg.is_invalid() || ignoree(fg) || IsZoomed(fg).as_bool() {
                return false;
            }
            // Une fenêtre sans bord qui couvre tout l'écran (le plein écran des jeux récents).
            let ecran = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
            if ecran.is_invalid() {
                return false;
            }
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let mut rect = RECT::default();
            GetMonitorInfoW(ecran, &mut info).as_bool() && GetWindowRect(fg, &mut rect).is_ok() && couvre(rect, info.rcMonitor)
        }
    }

    fn couvre(f: RECT, e: RECT) -> bool {
        f.left <= e.left && f.top <= e.top && f.right >= e.right && f.bottom >= e.bottom
    }

    /// Le bureau, la barre des tâches, une fenêtre outil (Alt+Tab, menus), ou Ellipse lui-même.
    unsafe fn ignoree(fg: HWND) -> bool {
        unsafe {
            if fg == GetShellWindow() || fg == GetDesktopWindow() {
                return true;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(fg, Some(&mut pid));
            if pid == GetCurrentProcessId() || GetWindowLongPtrW(fg, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                return true;
            }
            let mut classe = [0u16; 64];
            let n = GetClassNameW(fg, &mut classe) as usize;
            CLASSES_SHELL.contains(&String::from_utf16_lossy(&classe[..n]).as_str())
        }
    }
}

/// L'usage de la carte graphique par programme, d'après les compteurs de Windows (« GPU Engine », ceux du
/// Gestionnaire des tâches). Un jeu en fenêtre ou en arrière-plan n'est pas au premier plan en plein écran,
/// mais il fait travailler le moteur 3D : c'est ce qu'on regarde.
#[cfg(windows)]
mod carte_graphique {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW,
        PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::core::{PCWSTR, PWSTR, w};

    /// Part du moteur 3D (en %) au-delà de laquelle un autre programme compte comme un jeu. Mesuré le
    /// 14 novembre 2026 : le bureau (dwm) 5 %, un fond d'écran animé ou une vidéo dans le navigateur moins de
    /// 15 % ; un jeu bien plus.
    const SEUIL: f64 = 30.0;
    /// Les compteurs donnent un usage moyen entre deux mesures : on ne remesure pas plus souvent.
    const ECART_MIN: Duration = Duration::from_secs(1);
    /// Les programmes d'Ellipse (le modèle tourne sur la carte graphique) et le bureau de Windows.
    const IGNORES: [&str; 5] = ["ellipse.exe", "llama-server.exe", "ffmpeg.exe", "ffprobe.exe", "dwm.exe"];

    struct Requete {
        requete: PDH_HQUERY,
        compteur: PDH_HCOUNTER,
        mesure: Instant,
        occupee: bool,
        /// Nom de l'exécutable de chaque processus déjà vu.
        noms: HashMap<u32, String>,
    }
    // SAFETY : les poignées PDH ne servent que sous le verrou ci-dessous, depuis un seul fil à la fois.
    unsafe impl Send for Requete {}

    static REQUETE: LazyLock<Mutex<Option<Requete>>> = LazyLock::new(|| Mutex::new(ouvrir()));

    fn ouvrir() -> Option<Requete> {
        // SAFETY : la requête et le compteur sont créés ici et gardés pour toute la vie de l'appli.
        unsafe {
            let mut requete = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut requete) != 0 {
                return None;
            }
            let mut compteur = PDH_HCOUNTER::default();
            let chemin = w!("\\GPU Engine(*engtype_3D)\\Utilization Percentage");
            if PdhAddEnglishCounterW(requete, chemin, 0, &mut compteur) != 0 {
                return None;
            }
            // Première mesure : l'usage se calcule entre celle-ci et la suivante.
            let _ = PdhCollectQueryData(requete);
            Some(Requete { requete, compteur, mesure: Instant::now(), occupee: false, noms: HashMap::new() })
        }
    }

    pub fn occupee_par_un_autre() -> bool {
        let mut verrou = REQUETE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = verrou.as_mut() else { return false };
        if r.mesure.elapsed() < ECART_MIN {
            return r.occupee;
        }
        r.mesure = Instant::now();
        r.occupee = usage_par_processus(r)
            .into_iter()
            .any(|(pid, part)| part >= SEUIL && !IGNORES.contains(&nom(&mut r.noms, pid).as_str()));
        r.occupee
    }

    /// Part du moteur 3D utilisée par chaque processus depuis la mesure précédente.
    fn usage_par_processus(r: &Requete) -> HashMap<u32, f64> {
        let mut parts = HashMap::new();
        // SAFETY : le tampon est alloué à la taille demandée par PDH, et lu seulement après son remplissage.
        unsafe {
            if PdhCollectQueryData(r.requete) != 0 {
                return parts;
            }
            let (mut taille, mut nombre) = (0u32, 0u32);
            if PdhGetFormattedCounterArrayW(r.compteur, PDH_FMT_DOUBLE, &mut taille, &mut nombre, None) != PDH_MORE_DATA {
                return parts;
            }
            // Des u64 : le tampon est assez aligné pour les éléments (qui contiennent un f64 et des pointeurs).
            let mut tampon = vec![0u64; (taille as usize).div_ceil(8)];
            let elements = tampon.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            if PdhGetFormattedCounterArrayW(r.compteur, PDH_FMT_DOUBLE, &mut taille, &mut nombre, Some(elements)) != 0 {
                return parts;
            }
            for e in std::slice::from_raw_parts(elements, nombre as usize) {
                // Instance : « pid_1234_luid_0x…_phys_0_eng_0_engtype_3D ».
                let instance = e.szName.to_string().unwrap_or_default();
                let pid = instance.strip_prefix("pid_").and_then(|r| r.split('_').next()).and_then(|p| p.parse().ok());
                if let Some(pid) = pid
                    && e.FmtValue.CStatus == 0
                {
                    *parts.entry(pid).or_insert(0.0) += e.FmtValue.Anonymous.doubleValue;
                }
            }
        }
        parts
    }

    /// Nom de l'exécutable (en minuscules), gardé pour les fois suivantes.
    fn nom(noms: &mut HashMap<u32, String>, pid: u32) -> String {
        noms.entry(pid)
            .or_insert_with(|| {
                // SAFETY : la poignée est fermée aussitôt lue ; un processus disparu donne un nom vide.
                unsafe {
                    let Ok(processus) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                        return String::new();
                    };
                    let mut chemin = [0u16; 512];
                    let mut longueur = chemin.len() as u32;
                    let lu = QueryFullProcessImageNameW(processus, PROCESS_NAME_WIN32, PWSTR(chemin.as_mut_ptr()), &mut longueur);
                    let _ = CloseHandle(processus);
                    if lu.is_err() {
                        return String::new();
                    }
                    let chemin = String::from_utf16_lossy(&chemin[..longueur as usize]);
                    chemin.rsplit('\\').next().unwrap_or_default().to_lowercase()
                }
            })
            .clone()
    }
}
