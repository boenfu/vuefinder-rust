# VueFinder Rust

> Rust serverside implementation for VueFinder.
  Frontend: https://github.com/n1crack/vuefinder

[![crates.io](https://img.shields.io/crates/v/vuefinder.svg)](https://crates.io/crates/vuefinder)
[![download count badge](https://img.shields.io/crates/d/vuefinder.svg)](https://crates.io/crates/vuefinder)
[![docs.rs](https://img.shields.io/badge/docs-latest-blue.svg)](https://docs.rs/vuefinder)

## Installation

### As a Binary

Install the standalone server using cargo:
```bash
cargo install vuefinder
```

### As a Library

Add to your project's `Cargo.toml`:
```toml
[dependencies]
vuefinder = "0.1"
```

Or using cargo add:
```bash
cargo add vuefinder
```

## Usage

There are three ways to use VueFinder:

### 1. As a Standalone Binary

Install and run VueFinder as a standalone server:
```bash
# Development
cargo run
cargo run -- --port 3000
cargo run -- --host 0.0.0.0 --port 3000

# Production
vuefinder
vuefinder --port 3000
vuefinder --host 0.0.0.0 --port 3000
```

The server will start at `http://localhost:8080` by default. You can customize using command line options:

- `-p, --port <PORT>`: Specify server port [default: 8080]
- `-b, --host <HOST>`: Specify binding address [default: 127.0.0.1]
- `-l, --local-storage <PATH>`: Specify local storage path [default: ./storage]

```bash
# Examples
vuefinder --local-storage /path/to/storage
vuefinder --host 0.0.0.0 --port 3000 --local-storage /data/files
```

### 2. As a Library with Router

Integrate VueFinder into your existing Actix-Web application:
```rust
use actix_web::{App, HttpServer};
use std::collections::HashMap;
use std::sync::Arc;
use vuefinder::{
    app_config::{VueFinderAppConfig, VueFinderAppExt},
    finder::VueFinderConfig,
    storages::{local::LocalStorage, StorageAdapter},
};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Configure VueFinder
    let app_config = VueFinderAppConfig {
        api_path: "/custom/api".to_string(),  // Optional: customize API path
        json_limit: 50 * 1024 * 1024,         // Optional: 50MB limit
        storages: LocalStorage::setup("./storage"),
        finder_config: Arc::new(VueFinderConfig::default()),
        ..VueFinderAppConfig::default()
    };

    // Start server
    HttpServer::new(move || {
        App::new()
            .configure_vuefinder(app_config.clone())
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
let mut storages = HashMap::new();
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
- Configurable API endpoints and limits

## License

MIT