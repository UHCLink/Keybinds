//! Contrat de configuration et machine d'état des raccourcis UHC-Link.
//!
//! Ce module ne dépend pas des API Windows: les tests peuvent donc exercer les
//! mêmes règles sous Linux. `windows.rs` fournit les transitions physiques et
//! [`api`] transmet seulement les actions obtenues à la page vocale propriétaire.
//! Le navigateur reste responsable du son et de ses permissions.
//!
//! Les codes suivent `KeyboardEvent.code` pour le clavier, avec trois noms souris
//! supplémentaires partagés dans `keycodes.json`. Le libellé est destiné à
//! l'affichage, il ne participe jamais à la reconnaissance d'une combinaison.
//! L'état retenu est un ensemble de touches/boutons actuellement enfoncés parmi
//! les raccourcis actifs et leurs modificateurs, jamais un historique de frappe.

pub mod api;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Une touche physique ou un bouton auxiliaire, accompagné de modificateurs.
/// Ctrl, Maj, Alt et Windows peuvent aussi servir de touche principale.
/// Les champs JSON inconnus sont refusés pour détecter les configurations erronées.
pub struct Binding {
    /// Identifiant stable, par exemple `KeyQ`, `F8` ou `MouseBack`.
    pub code: String,
    /// Masque: Ctrl = 1, Maj = 2, Alt = 4, Windows = 8, somme maximale 15.
    pub modifiers: u8,
    /// Texte choisi lors de la capture, limité et validé avant stockage local.
    pub label: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Réglages locaux du PC, indépendants des comptes Minecraft et des rooms.
pub struct ShortcutConfig {
    /// Bascule du micro, `None` signifie que cette action n'a pas de raccourci.
    pub microphone: Option<Binding>,
    /// Bascule du casque et, côté site, de la coupure micro qui l'accompagne.
    pub headphones: Option<Binding>,
    /// Combinaison mémorisée, même quand le mode maintenir-pour-parler est coupé.
    pub push_to_talk: Option<Binding>,
    /// Désactivé si le champ manque dans une ancienne configuration enregistrée.
    #[serde(default)]
    pub push_to_talk_enabled: bool,
}
impl ShortcutConfig {
    /// Énumère les raccourcis qui nécessitent réellement une réception d'entrées.
    /// Un PTT configuré mais désactivé ne doit pas activer l'abonnement Raw Input.
    pub fn active_bindings(&self) -> impl Iterator<Item = &Binding> {
        [
            self.microphone.as_ref(),
            self.headphones.as_ref(),
            self.push_to_talk
                .as_ref()
                .filter(|_| self.push_to_talk_enabled),
        ]
        .into_iter()
        .flatten()
    }
    /// Vérifie le contrat avant toute utilisation ou persistance.
    /// Les doublons sont refusés même pour le PTT actuellement désactivé: son
    /// activation ultérieure ne doit pas créer deux actions sur la même touche.
    pub fn validate(&self) -> bool {
        let mut chords = HashSet::new();
        for binding in [&self.microphone, &self.headphones, &self.push_to_talk]
            .into_iter()
            .flatten()
        {
            let Some(scan) = input_code(&binding.code) else {
                return false;
            };
            if binding.modifiers > 15
                || binding.modifiers & modifier(scan) != 0
                || binding.label.is_empty()
                || binding.label.chars().count() > 32
                || binding.label.chars().any(char::is_control)
                || !chords.insert((binding.code.as_str(), binding.modifiers))
            {
                return false;
            }
        }
        !self.push_to_talk_enabled || self.push_to_talk.is_some()
    }
}
// Cette plage interne n'est ni un scan code clavier ni un numéro de bouton DOM.
// Les clics gauche/droit et le défilement n'ont volontairement aucun identifiant.
/// Clic de la molette (bouton central), sans prise en charge du défilement.
pub const MOUSE_MIDDLE: u16 = 0xF001;
/// Premier bouton latéral standard, généralement « précédent » / Souris 4.
pub const MOUSE_BACK: u16 = 0xF002;
/// Second bouton latéral standard, généralement « suivant » / Souris 5.
pub const MOUSE_FORWARD: u16 = 0xF003;
/// Convertit le nom du contrat web vers l'espace commun clavier/souris.
pub fn input_code(code: &str) -> Option<u16> {
    match code {
        "MouseMiddle" => Some(MOUSE_MIDDLE),
        "MouseBack" => Some(MOUSE_BACK),
        "MouseForward" => Some(MOUSE_FORWARD),
        _ => scan_code(code),
    }
}
/// Identifie exclusivement les trois boutons auxiliaires pris en charge.
pub fn is_mouse_input(code: u16) -> bool {
    matches!(code, MOUSE_MIDDLE | MOUSE_BACK | MOUSE_FORWARD)
}
/// Traduit les positions physiques du clavier vers les scan codes PC set-1.
/// `KeyQ` désigne la même position sur AZERTY et QWERTY, même si la lettre affichée
/// diffère. `0xE0xx` conserve le préfixe étendu pour distinguer pavé et navigation.
/// Les modificateurs peuvent servir de touche principale ou compléter une autre touche.
pub fn scan_code(code: &str) -> Option<u16> {
    Some(match code {
        "Digit1" => 2,
        "Digit2" => 3,
        "Digit3" => 4,
        "Digit4" => 5,
        "Digit5" => 6,
        "Digit6" => 7,
        "Digit7" => 8,
        "Digit8" => 9,
        "Digit9" => 10,
        "Digit0" => 11,
        "Minus" => 12,
        "Equal" => 13,
        "Backspace" => 14,
        "Tab" => 15,
        "KeyQ" => 16,
        "KeyW" => 17,
        "KeyE" => 18,
        "KeyR" => 19,
        "KeyT" => 20,
        "KeyY" => 21,
        "KeyU" => 22,
        "KeyI" => 23,
        "KeyO" => 24,
        "KeyP" => 25,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Enter" => 28,
        "ControlLeft" => 29,
        "KeyA" => 30,
        "KeyS" => 31,
        "KeyD" => 32,
        "KeyF" => 33,
        "KeyG" => 34,
        "KeyH" => 35,
        "KeyJ" => 36,
        "KeyK" => 37,
        "KeyL" => 38,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "ShiftLeft" => 42,
        "Backslash" => 43,
        "KeyZ" => 44,
        "KeyX" => 45,
        "KeyC" => 46,
        "KeyV" => 47,
        "KeyB" => 48,
        "KeyN" => 49,
        "KeyM" => 50,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        "ShiftRight" => 54,
        "AltLeft" => 56,
        "Space" => 57,
        "CapsLock" => 58,
        "F1" => 59,
        "F2" => 60,
        "F3" => 61,
        "F4" => 62,
        "F5" => 63,
        "F6" => 64,
        "F7" => 65,
        "F8" => 66,
        "F9" => 67,
        "F10" => 68,
        "F11" => 87,
        "F12" => 88,
        "Numpad0" => 82,
        "Numpad1" => 79,
        "Numpad2" => 80,
        "Numpad3" => 81,
        "Numpad4" => 75,
        "Numpad5" => 76,
        "Numpad6" => 77,
        "Numpad7" => 71,
        "Numpad8" => 72,
        "Numpad9" => 73,
        "NumpadMultiply" => 55,
        "NumpadSubtract" => 74,
        "NumpadAdd" => 78,
        "NumpadDecimal" => 83,
        "NumpadEnter" => 0xE01C,
        "ControlRight" => 0xE01D,
        "NumpadDivide" => 0xE035,
        "AltRight" => 0xE038,
        "MetaLeft" => 0xE05B,
        "MetaRight" => 0xE05C,
        "Home" => 0xE047,
        "ArrowUp" => 0xE048,
        "PageUp" => 0xE049,
        "ArrowLeft" => 0xE04B,
        "ArrowRight" => 0xE04D,
        "End" => 0xE04F,
        "ArrowDown" => 0xE050,
        "PageDown" => 0xE051,
        "Insert" => 0xE052,
        "Delete" => 0xE053,
        _ => return None,
    })
}
// Les deux côtés d'un modificateur alimentent le même bit. Le calcul depuis
// l'ensemble des touches enfoncées conserve Ctrl si l'autre Ctrl reste pressé.
fn modifier(scan: u16) -> u8 {
    match scan {
        29 | 0xE01D => 1,
        42 | 54 => 2,
        56 | 0xE038 => 4,
        0xE05B | 0xE05C => 8,
        _ => 0,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Commande logique envoyée à l'API locale, sans texte ni données audio.
pub enum Action {
    /// Bascule, le site calcule le nouvel état à partir de ses permissions.
    Microphone,
    Headphones,
    /// État absolu du PTT: `false` ferme la transmission de ce mode.
    PushToTalk(bool),
}
#[derive(Default)]
/// Machine commune aux entrées clavier et souris, nom conservé par convention.
/// Elle élimine l'auto-répétition et exige un nouvel appui après reconfiguration.
pub struct Keyboard {
    config: ShortcutConfig,
    // Seulement les entrées pertinentes actuellement maintenues, pas de journal.
    down: HashSet<u16>,
    active: bool,
    held: bool,
}
impl Keyboard {
    /// Remplace la configuration et repart avec un PTT fermé.
    /// `already_down` vient d'un instantané Windows limité aux touches pertinentes.
    /// Une touche déjà pressée est marquée comme telle pour empêcher une bascule
    /// ou une ouverture PTT lors du simple retour sur la page vocale.
    pub fn configure(
        &mut self,
        config: ShortcutConfig,
        active: bool,
        already_down: impl IntoIterator<Item = u16>,
    ) {
        self.config = config;
        self.active = active;
        self.down = if active {
            already_down
                .into_iter()
                .filter(|scan| self.tracks(*scan))
                .collect()
        } else {
            HashSet::new()
        };
        self.held = false;
    }
    // Filtrer avant l'insertion dans `down` évite de retenir les autres frappes.
    fn tracks(&self, scan: u16) -> bool {
        if modifier(scan) != 0 {
            self.config.active_bindings().next().is_some()
        } else {
            self.config
                .active_bindings()
                .any(|binding| input_code(&binding.code) == Some(scan))
        }
    }
    /// Copie des entrées maintenues pour la récupération des relâchements perdus.
    /// Le minuteur natif n'interroge ainsi pas l'ensemble du clavier.
    pub fn pressed_scans(&self) -> Vec<u16> {
        self.down.iter().copied().collect()
    }
    /// État logique du PTT, distinct de l'autorisation effective du micro côté site.
    pub fn held(&self) -> bool {
        self.held
    }
    /// Applique une transition physique, renvoie zéro, une ou plusieurs actions.
    /// Un relâchement peut fermer le PTT. Les bascules micro/casque et l'ouverture
    /// PTT nécessitent en revanche un nouvel appui sur leur touche de base.
    pub fn input(&mut self, scan: u16, pressed: bool) -> Vec<Action> {
        if !self.active || !self.tracks(scan) {
            return Vec::new();
        }
        let was_down = self.down.contains(&scan);
        if pressed {
            self.down.insert(scan);
        } else {
            self.down.remove(&scan);
        }
        let modifiers = self
            .down
            .iter()
            .fold(0, |mask, code| mask | modifier(*code));
        let mut actions = Vec::new();
        let matches = |binding: &Binding| {
            let Some(key) = input_code(&binding.code) else {
                return false;
            };
            // Le modificateur principal ne se compte pas lui-même dans le masque.
            let mut required = modifiers & !modifier(key);
            // AltGr peut être accompagné du Ctrl gauche synthétisé par Windows.
            // Un Ctrl droit réellement maintenu reste un modificateur distinct.
            if key == 0xE038 && binding.modifiers & 1 == 0 && !self.down.contains(&0xE01D) {
                required &= !1;
            }
            // Égalité exacte: Ctrl+V n'est pas équivalent à Ctrl+Maj+V.
            self.down.contains(&key) && binding.modifiers == required
        };
        // Vérifier la fermeture avant de rejeter répétitions et relâchements,
        // relâcher Ctrl doit fermer Ctrl+PTT même si le bouton de base reste pressé.
        if self.held && !self.config.push_to_talk.as_ref().is_some_and(matches) {
            self.held = false;
            actions.push(Action::PushToTalk(false));
        }
        if !pressed || was_down {
            return actions;
        }
        for (binding, action) in [
            (&self.config.microphone, Action::Microphone),
            (&self.config.headphones, Action::Headphones),
        ] {
            if binding
                .as_ref()
                .is_some_and(|b| input_code(&b.code) == Some(scan) && matches(b))
            {
                actions.push(action);
            }
        }
        if self.config.push_to_talk_enabled
            && self
                .config
                .push_to_talk
                .as_ref()
                .is_some_and(|b| input_code(&b.code) == Some(scan) && matches(b))
        {
            self.held = true;
            actions.push(Action::PushToTalk(true));
        }
        actions
    }
}
#[cfg(test)]
mod tests {
    // Tests de contrat et de transitions, sans API Windows ni périphérique réel.
    // Le cycle Wine teste ensuite le raccordement de ces règles à Raw Input.
    use super::*;
    #[test]
    fn browser_and_windows_share_the_same_physical_keys() {
        let browser: std::collections::HashMap<String, u16> =
            serde_json::from_str(include_str!("../keycodes.json")).unwrap();
        for (code, scan) in browser {
            assert_eq!(input_code(&code), Some(scan), "{code}");
        }
    }
    fn binding(code: &str, modifiers: u8) -> Option<Binding> {
        Some(Binding {
            code: code.into(),
            modifiers,
            label: code.into(),
        })
    }
    #[test]
    fn rejects_duplicate_unknown_chords_and_ptt_without_a_key() {
        let mut c = ShortcutConfig {
            microphone: binding("KeyV", 0),
            headphones: binding("KeyV", 0),
            ..Default::default()
        };
        assert!(!c.validate());
        c.headphones = None;
        assert!(c.validate());
        c.push_to_talk_enabled = true;
        assert!(!c.validate());
        c.push_to_talk = binding("F8", 0);
        assert!(c.validate());
        c.push_to_talk = binding("Escape", 0);
        assert!(!c.validate());
    }
    #[test]
    fn auxiliary_mouse_buttons_are_valid_but_primary_clicks_and_wheel_are_rejected() {
        for code in ["MouseMiddle", "MouseBack", "MouseForward"] {
            assert!(ShortcutConfig {
                microphone: binding(code, 1),
                ..Default::default()
            }
            .validate());
        }
        for code in [
            "MouseLeft",
            "MouseRight",
            "MousePrimary",
            "MouseSecondary",
            "Mouse6",
            "WheelUp",
            "WheelDown",
        ] {
            assert!(!ShortcutConfig {
                microphone: binding(code, 0),
                ..Default::default()
            }
            .validate());
        }
    }
    #[test]
    fn mouse_toggle_repeat_and_push_to_talk_release_with_modifiers() {
        let mut keys = Keyboard::default();
        keys.configure(
            ShortcutConfig {
                microphone: binding("MouseMiddle", 0),
                headphones: binding("MouseBack", 0),
                push_to_talk: binding("MouseForward", 1),
                push_to_talk_enabled: true,
            },
            true,
            [],
        );
        assert_eq!(keys.input(MOUSE_MIDDLE, true), vec![Action::Microphone]);
        assert!(keys.input(MOUSE_MIDDLE, true).is_empty());
        keys.input(MOUSE_MIDDLE, false);
        assert_eq!(keys.input(MOUSE_BACK, true), vec![Action::Headphones]);
        keys.input(MOUSE_BACK, false);
        keys.input(29, true);
        assert_eq!(
            keys.input(MOUSE_FORWARD, true),
            vec![Action::PushToTalk(true)]
        );
        assert!(keys.input(MOUSE_FORWARD, true).is_empty());
        assert_eq!(keys.input(29, false), vec![Action::PushToTalk(false)]);
        keys.input(MOUSE_FORWARD, false);
        keys.input(29, true);
        assert_eq!(
            keys.input(MOUSE_FORWARD, true),
            vec![Action::PushToTalk(true)]
        );
        assert_eq!(
            keys.input(MOUSE_FORWARD, false),
            vec![Action::PushToTalk(false)]
        );
        assert!(!keys.held());
    }
    #[test]
    fn mouse_held_during_activation_requires_a_new_press_and_disabled_mouse_is_ignored() {
        let mut keys = Keyboard::default();
        let config = ShortcutConfig {
            push_to_talk: binding("MouseBack", 0),
            push_to_talk_enabled: true,
            ..Default::default()
        };
        keys.configure(config.clone(), true, [MOUSE_BACK]);
        assert!(keys.input(MOUSE_BACK, true).is_empty());
        keys.input(MOUSE_BACK, false);
        assert_eq!(keys.input(MOUSE_BACK, true), vec![Action::PushToTalk(true)]);
        keys.configure(
            ShortcutConfig {
                push_to_talk_enabled: false,
                ..config
            },
            true,
            [MOUSE_BACK],
        );
        assert!(!keys.held());
        assert!(keys.pressed_scans().is_empty());
        assert!(keys.input(MOUSE_BACK, true).is_empty());
    }
    #[test]
    fn repeat_does_not_toggle_and_modifier_release_closes_ptt() {
        let mut keys = Keyboard::default();
        keys.configure(
            ShortcutConfig {
                microphone: binding("F8", 0),
                push_to_talk: binding("KeyV", 1),
                push_to_talk_enabled: true,
                ..Default::default()
            },
            true,
            [],
        );
        assert_eq!(keys.input(66, true), vec![Action::Microphone]);
        assert!(keys.input(66, true).is_empty());
        keys.input(66, false);
        keys.input(29, true);
        assert_eq!(keys.input(47, true), vec![Action::PushToTalk(true)]);
        assert_eq!(keys.input(29, false), vec![Action::PushToTalk(false)]);
        assert!(!keys.held());
        keys.input(29, true);
        assert!(keys.input(47, true).is_empty());
        keys.input(47, false);
        assert_eq!(keys.input(47, true), vec![Action::PushToTalk(true)]);
    }
    #[test]
    fn activating_or_reconfiguring_while_held_requires_release_then_press() {
        let mut keys = Keyboard::default();
        let config = ShortcutConfig {
            push_to_talk: binding("KeyV", 0),
            push_to_talk_enabled: true,
            ..Default::default()
        };
        keys.configure(config.clone(), true, [47]);
        assert!(keys.input(47, true).is_empty());
        keys.input(47, false);
        assert_eq!(keys.input(47, true), vec![Action::PushToTalk(true)]);
        keys.configure(config, false, []);
        assert!(!keys.held());
        assert!(keys.input(47, true).is_empty());
    }
    #[test]
    fn unrelated_keys_are_not_retained_and_do_not_interrupt_ptt() {
        let mut keys = Keyboard::default();
        keys.configure(
            ShortcutConfig {
                push_to_talk: binding("KeyV", 1),
                push_to_talk_enabled: true,
                ..Default::default()
            },
            true,
            [30],
        );
        assert!(keys.pressed_scans().is_empty());
        keys.input(29, true);
        assert_eq!(keys.input(47, true), vec![Action::PushToTalk(true)]);
        assert!(keys.input(30, true).is_empty());
        assert!(keys.held());
        assert!(!keys.pressed_scans().contains(&30));
        assert!(keys.input(30, false).is_empty());
        // The release-recovery timer can close PTT using only tracked held keys.
        assert_eq!(keys.input(29, false), vec![Action::PushToTalk(false)]);
        keys.input(47, false);
        assert!(keys.pressed_scans().is_empty());
    }
    #[test]
    fn disabled_ptt_and_inactive_shortcuts_do_not_retain_key_state() {
        let mut keys = Keyboard::default();
        let config = ShortcutConfig {
            microphone: binding("F8", 0),
            push_to_talk: binding("KeyV", 0),
            ..Default::default()
        };
        keys.configure(config.clone(), true, [47, 30]);
        assert!(keys.pressed_scans().is_empty());
        assert!(keys.input(47, true).is_empty());
        assert!(keys.pressed_scans().is_empty());
        assert_eq!(keys.input(66, true), vec![Action::Microphone]);
        keys.configure(config, false, [66, 29]);
        keys.input(66, true);
        keys.input(29, true);
        assert!(keys.pressed_scans().is_empty());
        let disabled = ShortcutConfig {
            push_to_talk: binding("KeyV", 0),
            ..Default::default()
        };
        assert!(disabled.active_bindings().next().is_none());
    }
}
