#[tokio::main]
async fn main() -> std::process::ExitCode {
    match ariai::cli::run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ariai: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
