use actix_cors::Cors;
use actix_web::middleware::Logger;
use actix_web::{web, App, HttpServer};
use env_logger::Env;
use std::sync::Arc;

use vuefinder::{
    finder::{VueFinder, VueFinderConfig},
    router::finder_router,
    storages::{local::LocalStorage, StorageAdapter},
};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(Env::default().default_filter_or("info"));

    // Ensure storage directory exists
    let storage_path = "./storage";
    tokio::fs::create_dir_all(storage_path).await?;

    let config = VueFinderConfig::from_file("config.json").unwrap_or_else(|_| VueFinderConfig {
        public_links: None,
        // cors: default_cors_config(),
    });

    let mut storages = std::collections::HashMap::new();
    storages.insert(
        "local".to_string(),
        Arc::new(LocalStorage::new(storage_path)) as Arc<dyn StorageAdapter>,
    );

    let vue_finder = web::Data::new(VueFinder {
        storages: Arc::new(storages),
        config: Arc::new(config.clone()),
    });

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        App::new()
            .wrap(Logger::default())
            .wrap(cors)
            .app_data(web::JsonConfig::default().limit(100 * 1024 * 1024)) // 100MB JSON limit
            .app_data(web::PayloadConfig::default().limit(100 * 1024 * 1024)) // 100MB payload limit
            .app_data(vue_finder.clone())
            .service(web::resource("/api").route(web::route().to(finder_router)))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
