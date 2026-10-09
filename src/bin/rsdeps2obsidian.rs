use clap::Parser;
use rsdeps2obsidian::config::{CliArgs, resolve_config};

fn main() {
    let args = CliArgs::parse();
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: cannot determine current directory: {e}");
        std::process::exit(1);
    });
    let config = match resolve_config(&args, &cwd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    println!("workspace: {}", config.workspace_input.display());
    println!("output: {}", config.output_raw.display());
    println!("status: success");
}
