//! Dev check: installs a store's launcher the way the Plugins dialog does (on macOS this downloads Wine first when there is none). Usage: launcher_install_check <platform> <config dir>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (platform, dir) = (a.get(1).cloned().unwrap_or_else(|| "legacy_games".into()), std::path::PathBuf::from(a.get(2).cloned().unwrap_or_else(|| ".".into())));
    println!("wine before: {:?}", pd_filter::wine::resolve_system().ok());
    let cancel = pd_filter::net::Cancel::new();
    let r = pd_filter::launcher_installer::install_here(&platform, &dir, &cancel, &mut |phase, detail| println!("[{phase}] {detail}"));
    println!("result: {r:?}");
    println!("wine after: {:?}", pd_filter::wine::resolve_system().ok());
    println!("launcher exe: {:?}", pd_filter::launcher_installer::launcher_exe(&dir, &platform));
}
