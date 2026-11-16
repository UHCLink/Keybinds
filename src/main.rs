//! Point d'entrée du composant utilisateur, distinct de l'installateur NSIS.
//! Le sous-système Windows supprime la console, l'icône de notification fournit
//! les informations, la sortie et l'accès au désinstalleur installé à côté de lui.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[cfg(windows)]
mod windows;
#[cfg(windows)]
fn main() {
    windows::run();
}
#[cfg(not(windows))]
fn main() {
    // La bibliothèque reste testable sous Linux, seule l'intégration native et
    // le composant destiné aux joueurs nécessitent Windows.
    eprintln!("Le composant des raccourcis pour UHC-Link est destiné à Windows 10/11.");
}
