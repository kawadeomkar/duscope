//! The panic hook: gives the terminal back before a panic's message prints,
//! so a crash in the viewer never leaves a broken shell (raw mode, the
//! alternate screen, the mouse reported). It does nothing while the viewer
//! does not hold the terminal, so every verb runs under it.

use std::sync::Once;

/// Installs the hook, in front of the one already installed, which still
/// prints the message: once the terminal is back, on the main screen.
pub fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            duscope_tui::restore();
            previous(info);
        }));
    });
}
