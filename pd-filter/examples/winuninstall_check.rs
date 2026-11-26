//! Dev check, run on Windows: lists how many uninstall entries the registry route finds and what it would start for the names given as arguments.
fn main() {
    let e = pd_filter::winuninstall::entries();
    println!("{} entries", e.len());
    for name in std::env::args().skip(1) {
        match pd_filter::winuninstall::command_for(&e, "", &name) {
            Some(c) => { let (p, a) = pd_filter::winuninstall::split_command(&c); println!("{name}: program={p} args={a:?}"); }
            None => println!("{name}: no entry"),
        }
    }
}
