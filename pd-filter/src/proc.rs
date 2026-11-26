//! Child programs a Windows build starts for its own purposes (PowerShell for the vault, a version check, an unpacker) must not open a console window of their own:
//! Zest itself is a window-only program there, so every console child would otherwise flash a black window. `.hidden()` sets CREATE_NO_WINDOW on Windows and does nothing elsewhere.
//! Never use it for something the player launches (a game, a launcher): that one may need its console or its own window.
use std::process::Command;

pub trait Hide { fn hidden(&mut self) -> &mut Self; }

impl Hide for Command {
    #[cfg(windows)]
    fn hidden(&mut self) -> &mut Self { use std::os::windows::process::CommandExt; self.creation_flags(0x0800_0000) }
    #[cfg(not(windows))]
    fn hidden(&mut self) -> &mut Self { self }
}
