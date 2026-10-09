use clap::Parser;
use rsdeps2obsidian::config::{resolve_config, CliArgs};

// Exit 2 is reserved for protocol/compat failures; the MVP never emits it
// (all fatal conditions map to exit 1).

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
    match rsdeps2obsidian::run(&config) {
        Ok(summary) => {
            // Stdout is the human summary only: counts, the dry-run plan,
            // and the status line. Diagnostics live on stderr (emitted by
            // run()); stdout is never JSON and never carries warning[ lines.
            println!("workspace: {}", summary.workspace.display());
            println!("packages: {}", summary.packages);
            println!("targets: {}", summary.targets);
            println!("modules: {}", summary.modules);
            println!("dependencies: {}", summary.dependencies);
            println!("external: {}", summary.external);
            println!("create: {}", summary.create.len());
            println!("update: {}", summary.update.len());
            println!("delete: {}", summary.delete.len());
            println!("warnings: {}", summary.warnings);
            if config.dry_run {
                for rel in &summary.create {
                    println!("plan create: {rel}");
                }
                for rel in &summary.update {
                    println!("plan update: {rel}");
                }
                for rel in &summary.delete {
                    println!("plan delete: {rel}");
                }
            }
            println!("status: success");
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
