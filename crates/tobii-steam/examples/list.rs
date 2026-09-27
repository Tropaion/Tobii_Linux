//! Print what this crate sees, so it can be diffed against `tobii bridge games`.
fn main() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .expect("HOME");
    for a in tobii_steam::apps(&home) {
        let pfx = tobii_steam::prefix(&home, &a.appid).is_some();
        println!(
            "{:<10} {:<48} proton={} tool={}",
            a.appid,
            a.name,
            if pfx { "yes" } else { "--" },
            tobii_steam::looks_like_tool(&a.name)
        );
    }
}
