//! Print what this crate sees, so it can be diffed against `tobii bridge games`.
fn main() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .expect("HOME");
    // One `Steam`, asked everything — the shape every caller in this workspace
    // uses. Built a question at a time, this listing walked every root's
    // `libraryfolders.vdf` once per installed title.
    let steam = tobii_steam::Steam::at(&home);
    for lib in steam.libraries() {
        println!("library: {}", lib.display());
    }
    for path in steam.missing_libraries() {
        println!("absent:  {} (named by libraryfolders.vdf)", path.display());
    }
    for a in steam.apps() {
        let pfx = steam.prefixes(&a.appid);
        println!(
            "{:<10} {:<48} proton={} tool={}",
            a.appid,
            a.name,
            match pfx.len() {
                0 => "--".to_string(),
                1 => "yes".to_string(),
                n => format!("yes({n})"),
            },
            tobii_steam::looks_like_tool(&a.name)
        );
    }
}
