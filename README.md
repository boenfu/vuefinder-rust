# VueFinder Rust

> Rust serverside implementation for VueFinder.
  Frontend: https://github.com/n1crack/vuefinder

[![crates.io](https://img.shields.io/crates/v/vuefinder.svg)](https://crates.io/crates/vuefinder)
[![download count badge](https://img.shields.io/crates/d/vuefinder.svg)](https://crates.io/crates/vuefinder)
[![docs.rs](https://img.shields.io/badge/docs-latest-blue.svg)](https://docs.rs/vuefinder)

## Usage

There are three ways to use VueFinder:

### 1. As a Standalone Binary

Install and run VueFinder as a standalone server:
```bash
# Install
cargo install vuefinder --features binary
# Run
vuefinder
```
The server will start at `http://localhost:8080` by default.

### 2. As a Library with Router

Integrate VueFinder into your existing Actix-Web application:
```rust
use actix_web::{web, App, HttpServer};
use vuefinder::{VueFinder, VueFinderConfig, finder_router, storages::local::LocalStorage};
use std::sync::Arc;
#[actix_web::main]
async fn main() -> std::io::Result<()> {
// Setup VueFinder
let storage_path = "./storage";
let mut storages = std::collections::HashMap::new();
storages.insert(
"local".to_string(),
Arc::new(LocalStorage::new(storage_path)) as Arc<dyn StorageAdapter>,
);
let vue_finder = web::Data::new(VueFinder {
storages: Arc::new(storages),
config: Arc::new(VueFinderConfig::default()),
});
// Start server
HttpServer::new(move || {
App::new()
.app_data(vue_finder.clone())
.service(web::resource("/api").route(web::route().to(finder_router)))
})
.bind("127.0.0.1:8080")?
.run()
.await
}
```
### 3. As a Library with Custom Implementation

Use VueFinder's components to build your own file management system:
```rust
use vuefinder::{VueFinder, VueFinderConfig, StorageAdapter};
// Create your own storage adapter
struct CustomStorage;
#[async_trait]
impl StorageAdapter for CustomStorage {
// Implement required methods
}
// Create VueFinder instance with custom storage
let mut storages = std::collections::HashMap::new();
storages.insert("custom".to_string(), Arc::new(CustomStorage));
let vue_finder = VueFinder {
storages: Arc::new(storages),
config: Arc::new(VueFinderConfig::default()),
};
// Use VueFinder methods directly
vue_finder.list_contents("path/to/dir").await?;
```
## Configuration

Create a `config.json` file in your project root:
```json
{
  "public_links": {
    "downloads": "public/downloads"
  }
}
```
## Features

- File operations: upload, download, delete, rename, move
- Directory operations: create, list, delete
- Archive operations: zip, unzip
- Multiple storage adapters support
- Large file support (up to 100MB by default)

## License

MIT