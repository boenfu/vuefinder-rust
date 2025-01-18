use actix_cors::Cors;
use actix_web::middleware::Logger;
use actix_web::{App, HttpServer};
use clap::Parser;
use env_logger::Env;
use std::sync::Arc;

use vuefinder::{
    app_config::{VueFinderAppConfig, VueFinderAppExt},
    finder::VueFinderConfig,
    storages::local::LocalStorage,
};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// Server listening port
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Server binding address
    #[arg(short = 'b', long, default_value = "127.0.0.1")]
    host: String,

    /// Local storage path
    #[arg(short = 'l', long, default_value = "./storage")]
    local_storage: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    env_logger::init_from_env(Env::default().default_filter_or("info"));

    // Ensure storage directory exists
    tokio::fs::create_dir_all(&args.local_storage).await?;

    let app_config = VueFinderAppConfig {
        storages: LocalStorage::setup(&args.local_storage),
        finder_config: Arc::new(
            VueFinderConfig::from_file("config.json")
                .unwrap_or_else(|_| VueFinderConfig { public_links: None }),
        ),
        ..VueFinderAppConfig::default()
    };

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        App::new()
            .wrap(Logger::default())
            .wrap(cors)
            .configure_vuefinder(app_config.clone())
    })
    .bind(format!("{}:{}", args.host, args.port))?
    .run()
    .await
}
