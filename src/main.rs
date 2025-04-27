mod server;
mod state;
mod tools;
mod types;
mod utils;

use std::env;
use std::process;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse arguments
    let args: Vec<String> = env::args().collect();
    let verbose = args.iter().any(|arg| arg == "--verbose" || arg == "-v");
    let debug = args.iter().any(|arg| arg == "--debug");
    let version = args.iter().any(|arg| arg == "--version" || arg == "-V");
    let test_json = args.iter().any(|arg| arg == "--test-json");

    // Handle version flag
    if version {
        eprintln!("winx version {}", env!("CARGO_PKG_VERSION"));
        process::exit(0);
    }

    // Set log level based on flags
    let log_level = if debug {
        tracing::Level::DEBUG
    } else if verbose {
        tracing::Level::INFO
    } else {
        tracing::Level::WARN
    };

    // Initialize logger with proper stderr output
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(log_level.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    // Log startup message to stderr
    tracing::info!("Starting winx server version {}", env!("CARGO_PKG_VERSION"));
    tracing::debug!("Debug logging enabled");

    // Run JSON tests if requested
    if test_json {
        eprintln!("Running JSON parsing tests to validate Initialize struct...");
        let results = utils::run_json_tests();
        for result in results {
            eprintln!("{}", result);
        }
        eprintln!("JSON tests completed.");
        return Ok(());
    }

    // Set up cleanup for graceful shutdown
    let term_signal = tokio::signal::ctrl_c();
    tokio::pin!(term_signal);

    // Example JSON format for reference
    if debug {
        tracing::debug!("Expected Initialize JSON format:");
        tracing::debug!(
            r#"{{"type":"first_call","any_workspace_path":"/path","chat_id":"","code_writer_config":null,"initial_files_to_read":[],"mode_name":"wcgw","task_id_to_resume":""}}"#
        );
    }

    // Run server
    match server::run_server().await {
        Ok(_) => {
            tracing::info!("Server shutting down normally");
            Ok(())
        }
        Err(e) => {
            tracing::error!("Server error: {}", e);
            // Extra error details in debug mode
            if debug {
                tracing::debug!("Error details: {:?}", e);
            }
            Err(e)
        }
    }
}
