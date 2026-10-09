//! The `joseon-probe` command. See the library for what it does.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = joseon_probe::run(&args, &mut std::io::stdout().lock(), &mut std::io::stderr().lock());
    std::process::exit(code);
}
