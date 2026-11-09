//! Entry point for the Spaceships game server.
//!
//! Replaces `node server/index.js`. Same port, same routes, same WebSocket
//! endpoint, same database. See the crate docs in `lib.rs` for the module map
//! and the list of deliberate behavioural differences.

use std::process::ExitCode;

use spaceships_server::{build, dir_exists, serve, Config};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let built = match build(&config) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("failed to open {}: {e}", config.db_path.display());
            return ExitCode::FAILURE;
        }
    };

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4000);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("Spaceships server listening on port {port}");
    println!("Local:  http://localhost:{port}");
    println!("LAN:    http://<your-ip>:{port}");
    // The JS prefers dist/ and silently falls through to public/; say which one
    // is actually there, because "I edited public/ and nothing changed" is the
    // usual confusion.
    match (dir_exists(&config.dist_dir), dir_exists(&config.public_dir)) {
        (true, _) => println!("Static: {} (built)", config.dist_dir.display()),
        (false, true) => println!("Static: {} (unbundled)", config.public_dir.display()),
        (false, false) => println!(
            "Static: none — neither {} nor {} exists",
            config.dist_dir.display(),
            config.public_dir.display()
        ),
    }
    println!("DB:     {}", config.db_path.display());

    if let Err(e) = serve(listener, built.router).await {
        eprintln!("server error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
