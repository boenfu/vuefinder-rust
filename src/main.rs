use actix_cors::Cors;
use actix_multipart::Multipart;
use actix_web::{web, App, HttpResponse, HttpServer};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use futures_util::TryStreamExt;
use serde_json::json;
use std::io::Write;
use std::io::Cursor;
use zip::{ZipWriter, write::FileOptions};
use actix_web::middleware::Logger;
use env_logger::Env;

mod storage;
use storage::{LocalStorage, StorageAdapter};

// 在文件开头添加新的 trait 定义
pub trait StorageAdapterDebug: StorageAdapter + std::fmt::Debug {}
impl<T: StorageAdapter + std::fmt::Debug> StorageAdapterDebug for T {}

// 基础配置结构体
#[derive(Clone, Debug)]
pub struct VueFinder {
    storages: Arc<std::collections::HashMap<String, Arc<dyn StorageAdapterDebug>>>,
    config: Arc<Config>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    public_links: Option<std::collections::HashMap<String, String>>,
    #[serde(default = "default_cors_config")]
    cors: CorsConfig,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CorsConfig {
    #[serde(default = "default_allowed_origins")]
    allowed_origins: Vec<String>,
    #[serde(default = "default_allowed_methods")]
    allowed_methods: Vec<String>,
    #[serde(default = "default_allowed_headers")]
    allowed_headers: Vec<String>,
    #[serde(default = "default_max_age")]
    max_age: u32,
}

// 默认配置函数
fn default_cors_config() -> CorsConfig {
    CorsConfig {
        allowed_origins: default_allowed_origins(),
        allowed_methods: default_allowed_methods(),
        allowed_headers: default_allowed_headers(),
        max_age: default_max_age(),
    }
}

fn default_allowed_origins() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_allowed_methods() -> Vec<String> {
    vec![
        "GET".to_string(),
        "POST".to_string(),
        "PUT".to_string(),
        "DELETE".to_string(),
        "OPTIONS".to_string(),
    ]
}

fn default_allowed_headers() -> Vec<String> {
    vec![
        "Origin".to_string(),
        "X-Requested-With".to_string(),
        "Content-Type".to_string(),
        "Accept".to_string(),
        "Authorization".to_string(),
    ]
}

fn default_max_age() -> u32 {
    3600
}

impl Config {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }
}

// 响应结构体
#[derive(Debug, Serialize)]
struct FileNode {
    #[serde(rename = "type")]
    node_type: String,
    path: String,
    basename: String,
    extension: Option<String>,
    storage: String,
    mime_type: Option<String>,
    url: Option<String>,
}

// 请求处理函数
impl VueFinder {
    fn set_public_links(&self, node: &mut FileNode) {
        if let Some(public_links) = &self.config.public_links {
            if node.node_type != "dir" {
                for (public_link, domain) in public_links {
                    if node.path.starts_with(public_link) {
                        node.url = Some(node.path.replace(public_link, domain));
                        break;
                    }
                }
            }
        }
    }

    pub async fn index(
        data: web::Data<VueFinder>,
        query: web::Query<IndexQuery>,
    ) -> HttpResponse {
        let adapter = query.adapter.clone().unwrap_or_else(|| {
            data.storages
                .keys()
                .next()
                .map(|s| s.clone())
                .unwrap_or_default()
        });

        let dirname = query
            .path
            .clone()
            .unwrap_or_else(|| format!("{}://", adapter));

        // 获取目录内容
        let storage = match data.storages.get(&adapter) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "Invalid storage adapter"
            }))
        };

        let list_contents = match storage.list_contents(&dirname).await {
            Ok(contents) => contents,
            Err(e) => return HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        };

        // 转换为 FileNode
        let files: Vec<FileNode> = list_contents.into_iter()
            .map(|item| {
                let mut node = FileNode {
                    node_type: item.node_type,
                    path: item.path,
                    basename: item.basename,
                    extension: item.extension,
                    storage: adapter.clone(),
                    mime_type: item.mime_type,
                    url: None,
                };
                data.set_public_links(&mut node);
                node
            })
            .collect();

        HttpResponse::Ok().json(json!({
            "adapter": adapter,
            "storages": data.storages.keys().collect::<Vec<_>>(),
            "dirname": dirname,
            "files": files
        }))
    }

    pub async fn subfolders(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
    ) -> HttpResponse {
        let adapter = query.adapter.clone().unwrap_or_else(|| {
            data.storages
                .keys()
                .next()
                .map(|s| s.clone())
                .unwrap_or_default()
        });

        let dirname = query.path.clone();

        let storage = match data.storages.get(&adapter) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "Invalid storage adapter"
            }))
        };

        match storage.list_contents(&dirname).await {
            Ok(contents) => {
                let folders: Vec<_> = contents.into_iter()
                    .filter(|item| item.node_type == "dir")
                    .map(|item| json!({
                        "adapter": adapter,
                        "path": item.path,
                        "basename": item.basename,
                    }))
                    .collect();

                HttpResponse::Ok().json(json!({ "folders": folders }))
            }
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        }
    }

    pub async fn download(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        match storage.read(&query.path).await {
            Ok(contents) => {
                let filename = Path::new(&query.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();

                let mime = mime_guess::from_path(&query.path)
                    .first_or_octet_stream();

                HttpResponse::Ok()
                    .content_type(mime.as_ref())
                    .append_header((
                        "Content-Disposition",
                        format!("attachment; filename=\"{}\"", filename)
                    ))
                    .body(contents)
            }
            Err(_) => HttpResponse::NotFound().finish()
        }
    }

    pub async fn preview(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        match storage.read(&query.path).await {
            Ok(contents) => {
                let mime = mime_guess::from_path(&query.path)
                    .first_or_octet_stream();

                HttpResponse::Ok()
                    .content_type(mime.as_ref())
                    .body(contents)
            }
            Err(_) => HttpResponse::NotFound().finish()
        }
    }

    pub async fn search(
        data: web::Data<VueFinder>,
        query: web::Query<SearchQuery>,
    ) -> HttpResponse {
        let adapter = query.adapter.clone().unwrap_or_default();
        let storage = match data.storages.get(&adapter) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        match storage.list_contents(&query.path).await {
            Ok(contents) => {
                let filter = query.filter.to_lowercase();
                let files: Vec<_> = contents.into_iter()
                    .filter(|item| item.node_type == "file" && 
                           item.basename.to_lowercase().contains(&filter))
                    .collect();

                HttpResponse::Ok().json(json!({
                    "adapter": adapter,
                    "storages": data.storages.keys().collect::<Vec<_>>(),
                    "dirname": query.path,
                    "files": files
                }))
            }
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        }
    }

    pub async fn newfolder(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<NewFolderRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        let new_path = format!("{}/{}", query.path, payload.name);
        
        match storage.create_dir(&new_path).await {
            Ok(_) => Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await,
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        }
    }

    pub async fn newfile(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<NewFileRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        let new_path = format!("{}/{}", query.path, payload.name);
        
        match storage.write(&new_path, vec![]).await {
            Ok(_) => Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await,
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        }
    }

    pub async fn rename(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<RenameRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        let new_path = format!("{}/{}", query.path, payload.name);
        
        // 先读取原文件内容
        match storage.read(&payload.item).await {
            Ok(contents) => {
                // 写入新文件
                if let Err(e) = storage.write(&new_path, contents).await {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": e.to_string()
                    }));
                }
                // 删除原文件
                if let Err(e) = storage.delete(&payload.item).await {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": e.to_string()
                    }));
                }
                Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await
            }
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        }
    }

    pub async fn r#move(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<MoveRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        // 检查目标路径是否存在冲突
        for item in &payload.items {
            let target = format!("{}/{}", payload.item, Path::new(&item.path).file_name().unwrap_or_default().to_str().unwrap());
            if storage.exists(&target).await.unwrap_or(false) {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "One of the files already exists."
                }));
            }
        }

        // 执行移动操作
        for item in payload.items {
            let target = format!("{}/{}", payload.item, Path::new(&item.path).file_name().unwrap_or_default().to_str().unwrap());
            
            // 读取源文件内容
            match storage.read(&item.path).await {
                Ok(contents) => {
                    // 写入目标位置
                    if let Err(e) = storage.write(&target, contents).await {
                        return HttpResponse::InternalServerError().json(json!({
                            "status": false,
                            "message": e.to_string()
                        }));
                    }
                    // 删除源文件
                    if let Err(e) = storage.delete(&item.path).await {
                        return HttpResponse::InternalServerError().json(json!({
                            "status": false,
                            "message": e.to_string()
                        }));
                    }
                }
                Err(e) => return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": e.to_string()
                }))
            }
        }

        Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await
    }

    pub async fn delete(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<DeleteRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        for item in payload.items {
            if let Err(e) = storage.delete(&item.path).await {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": e.to_string()
                }));
            }
        }

        Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await
    }

    pub async fn upload(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        mut payload: Multipart,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        while let Ok(Some(mut field)) = payload.try_next().await {
            let content_disposition = field.content_disposition();
            let filename = content_disposition.get_filename().unwrap_or_default();
            let filepath = format!("{}/{}", query.path, filename);

            // 读取文件内容
            let mut bytes = Vec::new();
            while let Ok(Some(chunk)) = field.try_next().await {
                bytes.extend_from_slice(&chunk);
            }

            // 写入存储
            if let Err(e) = storage.write(&filepath, bytes).await {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": e.to_string()
                }));
            }
        }

        Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await
    }

    pub async fn archive(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<ArchiveRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        let zip_path = format!("{}/{}.zip", query.path, payload.name);
        
        // 检查文件是否已存在
        if storage.exists(&zip_path).await.unwrap_or(false) {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "The archive already exists. Try another name."
            }));
        }

        // 创建 ZIP 文件
        let mut zip_buffer = Vec::new();
        let mut zip = ZipWriter::new(Cursor::new(&mut zip_buffer));
        let options = FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);

        for item in &payload.items {
            match storage.read(&item.path).await {
                Ok(contents) => {
                    let relative_path = item.path.trim_start_matches(&query.path).trim_start_matches('/');
                    zip.start_file(relative_path, options)?;
                    zip.write_all(&contents)?;
                }
                Err(e) => return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": e.to_string()
                }))
            }
        }

        zip.finish()?;
        
        // 保存 ZIP 文件
        if let Err(e) = storage.write(&zip_path, zip_buffer).await {
            return HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }));
        }

        Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await
    }

    pub async fn unarchive(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<UnarchiveRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        // 读取 ZIP 文件
        let zip_contents = match storage.read(&payload.item).await {
            Ok(contents) => contents,
            Err(e) => return HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        };

        let cursor = Cursor::new(zip_contents);
        let mut archive = zip::ZipArchive::new(cursor)?;

        // 解压文件
        let extract_path = format!("{}/{}", query.path, 
            Path::new(&payload.item).file_stem().unwrap_or_default().to_str().unwrap());

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let outpath = format!("{}/{}", extract_path, file.name());

            if file.name().ends_with('/') {
                storage.create_dir(&outpath).await?;
            } else {
                if let Some(p) = Path::new(&outpath).parent() {
                    let parent_path = p.to_str().unwrap();
                    storage.create_dir(parent_path).await?;
                }
                let mut buffer = Vec::new();
                std::io::copy(&mut file, &mut buffer)?;
                storage.write(&outpath, buffer).await?;
            }
        }

        Self::index(data, web::Query::from_query(&query.to_string()).unwrap()).await
    }

    pub async fn save(
        data: web::Data<VueFinder>,
        query: web::Query<PathQuery>,
        payload: web::Json<SaveRequest>,
    ) -> HttpResponse {
        let storage = match data.storages.get(&query.adapter.clone().unwrap_or_default()) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish()
        };

        match storage.write(&query.path, payload.content.as_bytes().to_vec()).await {
            Ok(_) => Self::preview(data, query).await,
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }))
        }
    }
}

// 请求和响应结构体
#[derive(Deserialize)]
pub struct IndexQuery {
    path: Option<String>,
    adapter: Option<String>,
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
    adapter: Option<String>,
}

#[derive(Deserialize)]
struct SearchQuery {
    path: String,
    filter: String,
    adapter: Option<String>,
}

#[derive(Deserialize)]
struct NewFolderRequest {
    name: String,
}

#[derive(Deserialize)]
struct NewFileRequest {
    name: String,
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
    item: String,
}

#[derive(Deserialize)]
struct MoveRequest {
    item: String,
    items: Vec<FileItem>,
}

#[derive(Deserialize)]
struct DeleteRequest {
    items: Vec<FileItem>,
}

#[derive(Deserialize)]
struct ArchiveRequest {
    name: String,
    items: Vec<FileItem>,
}

#[derive(Deserialize)]
struct UnarchiveRequest {
    item: String,
}

#[derive(Deserialize)]
struct SaveRequest {
    content: String,
}

#[derive(Deserialize)]
struct FileItem {
    path: String,
    r#type: String,
}

// 主函数
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(Env::default().default_filter_or("info"));

    let config = Config::from_file("config.json").unwrap_or_else(|_| Config {
        public_links: None,
        cors: default_cors_config(),
    });

    let mut storages = std::collections::HashMap::new();
    storages.insert(
        "local".to_string(),
        Arc::new(LocalStorage::new("./storage")) as Arc<dyn StorageAdapterDebug>,
    );

    let vue_finder = web::Data::new(VueFinder {
        storages: Arc::new(storages),
        config: Arc::new(config.clone()),
    });

    HttpServer::new(move || {
        let cors = Cors::default()
            .allowed_origin_fn(move |origin, _req_head| {
                if config.cors.allowed_origins.contains(&"*".to_string()) {
                    return true;
                }
                let origin_str = origin.to_str().unwrap_or_default();
                config.cors.allowed_origins.iter().any(|allowed| allowed == origin_str)
            })
            .allowed_methods(config.cors.allowed_methods.iter().map(|m| m.parse().unwrap()))
            .allowed_headers(config.cors.allowed_headers.iter().map(|h| h.parse().unwrap()))
            .max_age(config.cors.max_age);

        App::new()
            .wrap(Logger::default())
            .wrap(cors)
            .app_data(vue_finder.clone())
            .service(
                web::scope("/api")
                    .route("/index", web::get().to(VueFinder::index))
                    .route("/subfolders", web::get().to(VueFinder::subfolders))
                    .route("/download", web::get().to(VueFinder::download))
                    .route("/preview", web::get().to(VueFinder::preview))
                    .route("/search", web::get().to(VueFinder::search))
                    .route("/newfolder", web::post().to(VueFinder::newfolder))
                    .route("/newfile", web::post().to(VueFinder::newfile))
                    .route("/rename", web::post().to(VueFinder::rename))
                    .route("/move", web::post().to(VueFinder::r#move))
                    .route("/delete", web::post().to(VueFinder::delete))
                    .route("/upload", web::post().to(VueFinder::upload))
                    .route("/archive", web::post().to(VueFinder::archive))
                    .route("/unarchive", web::post().to(VueFinder::unarchive))
                    .route("/save", web::post().to(VueFinder::save)),
            )
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
