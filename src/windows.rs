//! Adaptateur Win32 du composant utilisateur: fenêtre de messages, Raw Input,
//! icône de notification et coordination avec l'API HTTP/WebSocket locale.
//!
//! Le thread principal possède la fenêtre et traite les messages Windows. Un
//! thread séparé exécute Tokio/Axum. Le callback remplace un unique état en attente,
//! puis poste APPLY, il ne manipule pas directement la fenêtre.
//! Le binaire n'installe rien au démarrage, NSIS prend en charge fichiers,
//! entrée Run et désinstallation. Aucun service Windows ou pilote noyau.

use anyhow::Context;
use anyhow::{ensure, Result};
use std::{
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};
use uhc_link_companion::{
    api::{self, LocalState},
    input_code, is_mouse_input, Keyboard, ShortcutConfig, MOUSE_BACK, MOUSE_FORWARD, MOUSE_MIDDLE,
};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::*, Input::*, Shell::*, WindowsAndMessaging::*},
};
// Identité partagée avec NSIS, détection d'une instance et arrêt à la suppression.
const CLASS: &str = "UHCLinkLocalShortcuts";
// Messages privés, distincts des messages WM_INPUT/WM_TIMER du système.
const APPLY: u32 = WM_APP + 1;
const TRAY: u32 = WM_APP + 2;
type PendingUpdate = Arc<Mutex<Option<(ShortcutConfig, bool)>>>;
// Objet partagé dont l'adresse reste stable pendant toute la boucle Windows.
// Les Mutex protègent les données possédées, l'AtomicBool signale l'abonnement.
struct Native {
    state: Arc<LocalState>,
    keys: Mutex<Keyboard>,
    updates: PendingUpdate,
    active: AtomicBool,
}
static APP: OnceLock<Arc<Native>> = OnceLock::new();
// Win32 attend des chaînes UTF-16 terminées par zéro. Les Vec temporaires restent
// vivants pendant chaque appel synchrone utilisant leur pointeur.
fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
// Interface commune pour les erreurs et informations explicites du composant.
fn show_message(text: &str, flags: u32) -> i32 {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(text).as_ptr(),
            wide("Raccourcis vocaux").as_ptr(),
            flags,
        )
    }
}
// Emplacement des réglages, indépendant du dossier choisi pour les exécutables.
fn directory() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA absent")?)
            .join("UHC-Link"),
    )
}
// Lancer l'assistant installé à côté du binaire, sans /S ni élévation forcée.
// Ne pas quitter ici, une annulation doit laisser le composant actif. Après
// confirmation, la section NSIS Uninstall enverra elle-même WM_CLOSE.
fn uninstall() -> Result<()> {
    let uninstaller = std::env::current_exe()
        .context("Impossible de trouver le dossier du composant")?
        .with_file_name("Uninstall.exe");
    ensure!(
        uninstaller.is_file(),
        "Le désinstalleur est introuvable dans le dossier du composant. Réinstallez UHC-Link avec l’installateur."
    );
    unsafe {
        let result = ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            wide(&uninstaller).as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
        // ShellExecute utilise historiquement des codes <= 32 pour les erreurs,
        // même si son type de retour ressemble à un handle.
        ensure!(
            result as isize > 32,
            "Impossible de lancer la désinstallation"
        );
    }
    Ok(())
}
// Instantané initial limité aux raccourcis actifs et aux modificateurs. Une
// touche déjà maintenue au retour sur la page ne doit pas déclencher une action.
fn configured_down(config: &ShortcutConfig) -> Vec<u16> {
    let mut scans: Vec<_> = config
        .active_bindings()
        .filter_map(|b| input_code(&b.code))
        .collect();
    scans.extend([29, 0xE01D, 42, 54, 56, 0xE038, 0xE05B, 0xE05C]);
    scans.retain(|scan| unsafe {
        let vk = input_virtual_key(*scan);
        vk != 0 && GetAsyncKeyState(vk as i32) < 0
    });
    scans
}
// GetAsyncKeyState utilise des VK, pas nos identifiants physiques. Seul son bit
// de poids fort indique l'état actuel, le bit bas n'est pas utilisé comme appui.
unsafe fn input_virtual_key(input: u16) -> u32 {
    match input {
        MOUSE_MIDDLE => VK_MBUTTON as u32,
        MOUSE_BACK => VK_XBUTTON1 as u32,
        MOUSE_FORWARD => VK_XBUTTON2 as u32,
        _ => MapVirtualKeyW(input as u32, MAPVK_VSC_TO_VK_EX),
    }
}
// Usage HID 1/6 = clavier, 1/2 = souris. INPUTSINK reçoit les entrées quand le jeu
// est au premier plan, sans hook et sans supprimer les entrées des autres apps.
// Le clavier reste nécessaire avec une souris seule pour connaître Ctrl/Maj/etc.
// REMOVE exige hwndTarget = NULL. La souris n'est active que si elle est utilisée.
unsafe fn register_inputs(hwnd: HWND, keyboard: bool, mouse: bool) -> bool {
    let devices = [(6, keyboard), (2, mouse)].map(|(usage, enabled)| RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: usage,
        dwFlags: if enabled {
            RIDEV_INPUTSINK
        } else {
            RIDEV_REMOVE
        },
        hwndTarget: if enabled { hwnd } else { null_mut() },
    });
    RegisterRawInputDevices(
        devices.as_ptr(),
        devices.len() as u32,
        size_of::<RAWINPUTDEVICE>() as u32,
    ) != 0
}
// Appelé exclusivement par la boucle Win32. Les lectures de unions RAWINPUT sont
// sélectionnées selon dwType. Les pointeurs passés aux appels synchrones restent
// valides pour leur durée et aucune référence aux données Raw Input n'est conservée.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(app) = APP.get() {
        match message {
            APPLY => {
                // Un seul emplacement, même si le thread Windows est occupé.
                // Libérer son verrou avant tout appel natif ou boîte de dialogue.
                let latest = app.updates.lock().unwrap().take();
                if let Some((config, active)) = latest {
                    let requested = active && config.active_bindings().next().is_some();
                    let mut active = requested;
                    if requested || app.active.load(Ordering::SeqCst) {
                        let mouse = requested
                            && config
                                .active_bindings()
                                .filter_map(|b| input_code(&b.code))
                                .any(is_mouse_input);
                        if !register_inputs(hwnd, requested, mouse) {
                            // En cas d'échec, tenter de tout désinscrire et garder
                            // le PTT fermé plutôt que rester partiellement actif.
                            register_inputs(hwnd, false, false);
                            active = false;
                        }
                    }
                    let down = if active {
                        configured_down(&config)
                    } else {
                        Vec::new()
                    };
                    app.keys.lock().unwrap().configure(config, active, down);
                    app.active.store(active, Ordering::SeqCst);
                    if requested && !active {
                        app.state
                            .action(uhc_link_companion::Action::PushToTalk(false));
                        show_message("Windows ne permet pas de recevoir les raccourcis clavier ou souris. Quittez puis relancez le composant.", MB_OK | MB_ICONERROR);
                    }
                }
                return 0;
            }
            WM_INPUT => {
                if app.active.load(Ordering::SeqCst) {
                    let mut raw: RAWINPUT = zeroed();
                    let mut size = size_of::<RAWINPUT>() as u32;
                    let copied = GetRawInputData(
                        lparam as HRAWINPUT,
                        RID_INPUT,
                        &mut raw as *mut _ as *mut _,
                        &mut size,
                        size_of::<RAWINPUTHEADER>() as u32,
                    );
                    if copied != u32::MAX
                        && copied as usize <= size_of::<RAWINPUT>()
                        && copied as usize >= size_of::<RAWINPUTHEADER>()
                        && raw.header.dwSize == copied
                    {
                        if raw.header.dwType == RIM_TYPEKEYBOARD
                            && copied as usize
                                >= size_of::<RAWINPUTHEADER>() + size_of::<RAWKEYBOARD>()
                        {
                            // E0 distingue, par exemple, Entrée du pavé et Entrée
                            // principale. RI_KEY_BREAK représente le relâchement.
                            let key = raw.data.keyboard;
                            let scan = key.MakeCode
                                | if key.Flags & RI_KEY_E0 as u16 != 0 {
                                    0xE000
                                } else {
                                    0
                                };
                            let pressed = key.Flags & RI_KEY_BREAK as u16 == 0;
                            for event in app.keys.lock().unwrap().input(scan, pressed) {
                                app.state.action(event);
                            }
                        } else if raw.header.dwType == RIM_TYPEMOUSE
                            && copied as usize
                                >= size_of::<RAWINPUTHEADER>() + size_of::<RAWMOUSE>()
                        {
                            // Seuls les trois boutons autorisés sont décodés.
                            // Coordonnées, roue et clics gauche/droit sont ignorés,
                            // même s'ils figurent dans le paquet Raw Input reçu.
                            let flags = raw.data.mouse.Anonymous.Anonymous.usButtonFlags as u32;
                            for (code, down, up) in [
                                (
                                    MOUSE_MIDDLE,
                                    RI_MOUSE_MIDDLE_BUTTON_DOWN,
                                    RI_MOUSE_MIDDLE_BUTTON_UP,
                                ),
                                (MOUSE_BACK, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP),
                                (MOUSE_FORWARD, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP),
                            ] {
                                for (flag, pressed) in [(down, true), (up, false)] {
                                    if flags & flag != 0 {
                                        for event in app.keys.lock().unwrap().input(code, pressed) {
                                            app.state.action(event);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            WM_TIMER => {
                // Secours si PostMessage a échoué, aucun état sensible ne reste
                // en attente uniquement parce que la file Windows était pleine.
                window_proc(hwnd, APPLY, 0, 0);
                if !app.active.load(Ordering::SeqCst) {
                    return 0;
                }
                // Récupérer les relâchements perdus (changement de bureau/verrouillage).
                // Ce filet de sécurité n'interroge que les entrées déjà maintenues
                // et ne produit aucun nouvel appui. L'état vide ne sonde rien.
                let pressed = app.keys.lock().unwrap().pressed_scans();
                for scan in pressed {
                    let vk = input_virtual_key(scan);
                    if vk == 0 || GetAsyncKeyState(vk as i32) >= 0 {
                        for event in app.keys.lock().unwrap().input(scan, false) {
                            app.state.action(event);
                        }
                    }
                }
                return 0;
            }
            TRAY if lparam as u32 == WM_RBUTTONUP => {
                // Menu standard, construit à la demande puis détruit après usage.
                // RETURNCMD renvoie l'identifiant choisi, sans WM_COMMAND séparé.
                let menu = CreatePopupMenu();
                AppendMenuW(menu, MF_STRING, 1, wide("Informations").as_ptr());
                AppendMenuW(menu, MF_STRING, 2, wide("Quitter").as_ptr());
                AppendMenuW(menu, MF_STRING, 3, wide("Désinstaller").as_ptr());
                let mut point: POINT = zeroed();
                GetCursorPos(&mut point);
                SetForegroundWindow(hwnd);
                let choice = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_NONOTIFY,
                    point.x,
                    point.y,
                    0,
                    hwnd,
                    null(),
                );
                DestroyMenu(menu);
                if choice == 1 {
                    show_message("UHC-Link - https://uhc-link.fr\n\nLe composant des raccourcis est actif, celui-ci vous permet de configurer des raccourcis sur le site même lorsque vous êtes sur Minecraft.\n\nLe code source de ce composant est disponible ici: https://github.com/UHC-Link/Keybinds",MB_OK|MB_ICONINFORMATION);
                }
                if choice == 2 {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                if choice == 3 {
                    if let Err(error) = uninstall() {
                        show_message(&error.to_string(), MB_OK | MB_ICONERROR);
                    }
                }
                return 0;
            }
            WM_CLOSE => {
                // Chemin commun au menu Quitter et à l'arrêt demandé par NSIS.
                DestroyWindow(hwnd);
                return 0;
            }
            WM_DESTROY => {
                // Retirer l'icône avant de terminer la boucle de messages.
                let mut icon: NOTIFYICONDATAW = zeroed();
                icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
                icon.hWnd = hwnd;
                icon.uID = 1;
                Shell_NotifyIconW(NIM_DELETE, &icon);
                PostQuitMessage(0);
                return 0;
            }
            _ => {}
        }
    }
    // Le traitement par défaut assure notamment le nettoyage WM_INPUT Windows.
    DefWindowProcW(hwnd, message, wparam, lparam)
}
fn start() -> Result<()> {
    // Les chargements dynamiques suivants ne cherchent pas de DLL dans le
    // répertoire courant ni dans PATH. Les imports initiaux précèdent ce point.
    ensure!(
        unsafe {
            windows_sys::Win32::System::LibraryLoader::SetDefaultDllDirectories(
                windows_sys::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        } != 0,
        "Impossible de sécuriser la recherche des bibliothèques Windows"
    );
    // --background est utilisé par NSIS et l'entrée de démarrage utilisateur.
    // Le runtime ne se recopie pas et n'écrit pas de clés d'installation.
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.is_empty() || args == ["--background"] || args == ["--run"],
        "Arguments inconnus"
    );
    unsafe {
        // Détection par classe de fenêtre, aussi employée par l'installateur.
        // C'est une coordination d'instances, pas une frontière de sécurité.
        if !FindWindowW(wide(CLASS).as_ptr(), null()).is_null() {
            if args.is_empty() {
                show_message(
                    "Le composant des raccourcis est déjà actif, vous configurez vos raccourcis depuis le site.\n\nPour installer une nouvelle version, fermez ce composant via la liste des programmes dans votre barre des tâches.",
                    MB_OK | MB_ICONINFORMATION,
                );
            }
            return Ok(());
        }
    }
    unsafe {
        // L'icône/manifest/version sont incorporés par build.rs sous la ressource 1.
        let instance = GetModuleHandleW(null());
        let class = wide(CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hIcon: LoadIconW(instance, std::ptr::without_provenance(1)),
            ..zeroed()
        };
        ensure!(RegisterClassW(&wc) != 0, "Création du composant impossible");
        // Fenêtre non visible qui reçoit messages privés et Raw Input. L'icône
        // dans la zone de notification reste le point d'accès utilisateur.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Raccourcis vocaux").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        ensure!(!hwnd.is_null(), "Création de la fenêtre de fond impossible");
        // État partagé borné, aucun empilement de configurations ou de messages.
        // PostMessage ne transporte jamais un pointeur de configuration.
        let updates: PendingUpdate = Arc::new(Mutex::new(None));
        let pending = updates.clone();
        let handle = hwnd as usize;
        let state = LocalState::new(
            directory()?.join("settings.json"),
            api::origins(),
            move |config, active| {
                let notify = {
                    let mut slot = pending.lock().unwrap();
                    let notify = slot.is_none();
                    *slot = Some((config, active));
                    notify
                };
                if notify {
                    PostMessageW(handle as HWND, APPLY, 0, 0);
                }
            },
        )?;
        APP.set(Arc::new(Native {
            state: state.clone(),
            keys: Mutex::new(Keyboard::default()),
            updates,
            active: AtomicBool::new(false),
        }))
        .ok();
        // Le timer 100 ms sert aux seuls relâchements de secours, les appuis
        // arrivent directement via WM_INPUT et n'attendent pas ce timer.
        SetTimer(hwnd, 1, 100, None);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        // Le serveur n'est prêt qu'après réservation effective d'un port de la
        // plage. Si tous sont occupés, ne pas afficher une icône opérationnelle.
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        // Un runtime Tokio à thread courant suffit pour cette petite API locale.
        // Le canal oneshot permet l'arrêt coordonné sans tuer le thread de force.
        let worker = std::thread::Builder::new()
            .name("UHC-Link local".into())
            .spawn(move || {
                runtime.block_on(async {
                    tokio::select! {
                        _ = api::serve_with_ready(state, Some(ready_tx)) => {},
                        _ = stopped => {},
                    }
                });
            })?;
        ensure!(
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "Le service local ne peut pas démarrer. Vérifiez que la plage locale 32145–32245 n’est pas entièrement occupée ou bloquée."
        );
        // NIF_MESSAGE dirige les clics de l'icône vers TRAY sur notre fenêtre.
        let mut icon: NOTIFYICONDATAW = zeroed();
        icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon.hWnd = hwnd;
        icon.uID = 1;
        icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        icon.uCallbackMessage = TRAY;
        icon.hIcon = LoadIconW(instance, std::ptr::without_provenance(1));
        let tip = wide("UHC-Link - Raccourcis vocaux");
        icon.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(NIM_ADD, &icon);
        if args.is_empty() {
            show_message("Le composant des raccourcis est actif.\n\nRetournez sur le site de UHC-Link pour configurer vos touches.",MB_OK|MB_ICONINFORMATION);
        }
        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        // WM_QUIT a terminé la boucle, arrêter l'API et attendre la fin du worker
        // pour qu'aucun serveur local ne reste vivant après Quitter.
        let _ = shutdown.send(());
        let _ = worker.join();
    }
    Ok(())
}
/// Lance le composant et affiche les erreurs avant de rendre la main à Windows.
pub fn run() {
    if let Err(error) = start() {
        show_message(
            &format!("Impossible de démarrer le composant.\n\n{error}"),
            MB_OK | MB_ICONERROR,
        );
    }
}
