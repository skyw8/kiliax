use std::path::PathBuf;

pub fn parse_run_args(args: &[String]) -> kiliax_server::runner::ServerRunOptions {
    let mut out = kiliax_server::runner::ServerRunOptions::default();

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--host" => {
                if let Some(v) = iter.next() {
                    out.host = Some(v.to_string());
                }
            }
            "--port" => {
                if let Some(v) = iter.next() {
                    out.port = v.parse().ok();
                }
            }
            "--workspace-root" => {
                if let Some(v) = iter.next() {
                    out.workspace_root = Some(PathBuf::from(v));
                }
            }
            "--config" => {
                if let Some(v) = iter.next() {
                    out.config_path = Some(PathBuf::from(v));
                }
            }
            "--token" => {
                if let Some(v) = iter.next() {
                    out.token = Some(v.to_string());
                }
            }
            _ => {}
        }
    }

    out
}

pub fn print_run_help() {
    let bin = env!("CARGO_PKG_NAME");
    println!("{bin} server run");
    println!("  --host <ip>             (default: 127.0.0.1)");
    println!("  --port <port>           (default: 8123)");
    println!("  --workspace-root <dir>  (default: cwd)");
    println!("  --config <path>         (default: auto-detect kiliax.yaml)");
    println!("  --token <token>         (required bearer/web auth)");
}
