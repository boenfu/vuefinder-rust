use actix_multipart::form::MultipartForm;
use actix_web::{web, HttpResponse};
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::io::Cursor;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use zip::{write::FileOptions, ZipWriter};

use crate::payload::{
    ArchiveRequest, CopyRequest, DeleteRequest, MoveRequest, NewFileRequest, NewFolderRequest,
    Query, RenameRequest, SaveRequest, SearchQuery, UnarchiveRequest, UploadForm,
};
use crate::storages::StorageItem;
use crate::storages::{StorageAdapter, StorageError};

// Default configuration functions
#[derive(Clone, Debug, Default, Deserialize)]
pub struct VueFinderConfig {
    pub public_links: Option<std::collections::HashMap<String, String>>,
}

impl VueFinderConfig {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: VueFinderConfig = serde_json::from_str(&content)?;
        Ok(config)
    }
}

#[derive(Debug, Serialize)]
struct FileNode {
    #[serde(flatten)]
    storage_item: StorageItem,
    url: Option<String>,
    // search result supported
    dir: Option<String>,
}

#[derive(Clone)]
pub struct VueFinder {
    pub storages: Arc<std::collections::HashMap<String, Arc<dyn StorageAdapter>>>,
    pub config: Arc<VueFinderConfig>,
}

// Request handling functions
impl VueFinder {
    fn get_default_adapter(&self, adapter: Option<String>) -> String {
        // If adapter is empty, return the first available adapter
        if let Some(adapter) = adapter {
            if self.storages.contains_key(&adapter) {
                return adapter;
            }
        }

        // Return the first available adapter
        self.storages.keys().next().cloned().unwrap_or_default()
    }

    /// Parses a storage name from a path URI like "storage-name://path"
    fn parse_storage_name_from_path(&self, path: &str) -> Option<String> {
        if let Some(pos) = path.find("://") {
            let storage_name = &path[..pos];
            if !storage_name.is_empty() && self.storages.contains_key(storage_name) {
                return Some(storage_name.to_string());
            }
        }
        None
    }

    fn join_storage_path(base: &str, child: &str) -> String {
        if child.contains("://") {
            return child.to_string();
        }

        if base.ends_with("://") || base.ends_with('/') {
            format!("{base}{child}")
        } else {
            format!("{base}/{child}")
        }
    }

    fn is_plain_name(name: &str) -> bool {
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains("://")
            && !name.contains('/')
            && !name.contains('\\')
    }

    fn storage_path_parts(path: &str) -> Option<(&str, &str)> {
        path.find("://").map(|pos| (&path[..pos], &path[pos + 3..]))
    }

    fn normalized_storage_path_parts(path: &str) -> Option<(String, String)> {
        let (storage, raw_path) = Self::storage_path_parts(path)?;
        let mut parts = Vec::new();

        for part in raw_path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                _ => parts.push(part),
            }
        }

        Some((storage.to_string(), parts.join("/")))
    }

    fn is_same_or_descendant_storage_path(parent: &str, candidate: &str) -> bool {
        let Some((parent_storage, parent_path)) = Self::normalized_storage_path_parts(parent)
        else {
            return false;
        };
        let Some((candidate_storage, candidate_path)) =
            Self::normalized_storage_path_parts(candidate)
        else {
            return false;
        };

        if parent_storage != candidate_storage {
            return false;
        }

        parent_path.is_empty()
            || candidate_path == parent_path
            || candidate_path
                .strip_prefix(&parent_path)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    fn storage_path_basename(path: &str) -> Option<String> {
        let (_, path) = Self::normalized_storage_path_parts(path)?;
        path.rsplit('/')
            .find(|part| !part.is_empty())
            .map(ToString::to_string)
    }

    fn transfer_target_path(destination: &str, source_path: &str) -> Option<String> {
        let basename = Self::storage_path_basename(source_path)?;
        Some(Self::join_storage_path(destination, &basename))
    }

    async fn is_dir_item(
        storage: &Arc<dyn StorageAdapter>,
        item: &crate::payload::FileItem,
    ) -> Result<bool, StorageError> {
        match item.r#type.as_str() {
            "dir" => Ok(true),
            "file" => Ok(false),
            _ => storage.is_dir(&item.path).await,
        }
    }

    fn set_public_links(&self, node: &mut FileNode) {
        if let Some(public_links) = &self.config.public_links {
            if node.storage_item.node_type != "dir" {
                for (public_link, domain) in public_links {
                    if node.storage_item.path.starts_with(public_link) {
                        node.url = Some(node.storage_item.path.replace(public_link, domain));
                        break;
                    }
                }
            }
        }
    }

    pub async fn index(data: web::Data<VueFinder>, query: web::Query<Query>) -> HttpResponse {
        let path = &query.path;

        let (storage_name, path_to_list) = if path.is_empty() {
            let default_storage = data.get_default_adapter(None);
            (default_storage, String::new())
        } else {
            match data.parse_storage_name_from_path(path) {
                Some(name) => (name, path.clone()),
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid path format. Path should include storage prefix (e.g., 'local://')."
                    }))
                }
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        let list_contents = match storage.list_contents(&path_to_list).await {
            Ok(contents) => contents,
            Err(e) => {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": e.to_string()
                }))
            }
        };

        let files: Vec<FileNode> = list_contents
            .into_iter()
            .map(|item| {
                let mut node = FileNode {
                    storage_item: item,
                    url: None,
                    dir: None,
                };
                data.set_public_links(&mut node);
                node
            })
            .collect();

        let response_path = if path.is_empty() {
            format!("{}://", storage_name)
        } else {
            path.clone()
        };

        HttpResponse::Ok().json(json!({
            "storages": data.storages.keys().collect::<Vec<_>>(),
            "dirname": response_path,
            "files": files,
            "read_only": false
        }))
    }

    pub async fn sub_folders(data: web::Data<VueFinder>, query: web::Query<Query>) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&query.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        match storage.list_contents(&query.path).await {
            Ok(contents) => {
                let folders: Vec<_> = contents
                    .into_iter()
                    .filter(|item| item.node_type == "dir")
                    .map(|item| {
                        json!({
                            "path": item.path,
                            "basename": item.basename,
                        })
                    })
                    .collect();

                HttpResponse::Ok().json(json!({ "folders": folders }))
            }
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            })),
        }
    }

    pub async fn download(data: web::Data<VueFinder>, query: web::Query<Query>) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&query.path) {
            Some(name) => name,
            None => return HttpResponse::BadRequest().finish(),
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish(),
        };

        match storage.read(&query.path).await {
            Ok(contents) => {
                let filename = Path::new(&query.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();

                let mime = mime_guess::from_path(&query.path).first_or_octet_stream();

                HttpResponse::Ok()
                    .content_type(mime.as_ref())
                    .append_header((
                        "Content-Disposition",
                        format!("attachment; filename=\"{}\"", filename),
                    ))
                    .body(contents)
            }
            Err(_) => HttpResponse::NotFound().finish(),
        }
    }

    pub async fn preview(data: web::Data<VueFinder>, query: web::Query<Query>) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&query.path) {
            Some(name) => name,
            None => return HttpResponse::BadRequest().finish(),
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => return HttpResponse::BadRequest().finish(),
        };

        match storage.read(&query.path).await {
            Ok(contents) => {
                let mime = mime_guess::from_path(&query.path).first_or_octet_stream();

                HttpResponse::Ok()
                    .content_type(mime.as_ref())
                    .body(contents)
            }
            Err(_) => HttpResponse::NotFound().finish(),
        }
    }

    pub async fn search(
        data: web::Data<VueFinder>,
        query: web::Query<SearchQuery>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&query.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format. Path should include storage prefix (e.g., 'local://')."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        let base_path = query.path.clone();
        let filter = query.filter.clone().unwrap_or_default().to_lowercase();
        let deep = query.deep.as_ref().map(|b| b.0).unwrap_or(false);

        let size_filter = match query.size.as_deref() {
            Some("small") => Some(0..1024),                 // < 1KB
            Some("medium") => Some(1024..1024 * 1024),      // 1KB - 1MB
            Some("large") => Some(1024 * 1024..usize::MAX), // >= 1MB
            _ => None,                                      // "all" or None
        };

        fn matches_size(size: Option<u64>, filter: &Option<std::ops::Range<usize>>) -> bool {
            match (size, filter) {
                (Some(s), Some(r)) => r.contains(&(s as usize)),
                (None, Some(_)) => false,
                _ => true,
            }
        }

        async fn search_dir(
            storage: &Arc<dyn StorageAdapter>,
            current_path: String,
            filter: &str,
            deep: bool,
            size_filter: &Option<std::ops::Range<usize>>,
            results: &mut Vec<FileNode>,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let contents = storage.list_contents(&current_path).await?;

            for item in contents {
                if item.node_type == "file" && item.basename.to_lowercase().contains(filter) {
                    if matches_size(item.size, size_filter) {
                        let dir = if let Some(parent) = Path::new(&item.path).parent() {
                            parent.to_string_lossy().to_string()
                        } else {
                            String::new()
                        };

                        results.push(FileNode {
                            storage_item: item,
                            url: None,
                            dir: Some(dir),
                        });
                    }
                } else if item.node_type == "dir" && deep {
                    let sub_path = if current_path.is_empty() {
                        item.basename
                    } else {
                        format!("{current_path}/{}", item.basename)
                    };
                    Box::pin(search_dir(
                        storage,
                        sub_path,
                        filter,
                        deep,
                        size_filter,
                        results,
                    ))
                    .await?;
                }
            }
            Ok(())
        }

        let mut files = Vec::new();
        match search_dir(storage, base_path, &filter, deep, &size_filter, &mut files).await {
            Ok(_) => HttpResponse::Ok().json(json!({
                "storages": data.storages.keys().collect::<Vec<_>>(),
                "dirname": query.path,
                "files": files
            })),
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            })),
        }
    }

    pub async fn new_folder(
        data: web::Data<VueFinder>,
        payload: web::Json<NewFolderRequest>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format. Path should include storage prefix (e.g., 'local://')."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        let new_path = format!("{}/{}", payload.path, payload.name);

        if storage.exists(&new_path).await.unwrap_or(false) {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "A file or directory with this name already exists"
            }));
        }

        match storage.create_dir(&new_path).await {
            Ok(_) => {
                let query = web::Query(Query {
                    path: payload.path.clone(),
                });
                Self::index(data, query).await
            }

            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            })),
        }
    }

    pub async fn new_file(
        data: web::Data<VueFinder>,
        payload: web::Json<NewFileRequest>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format. Path should include storage prefix (e.g., 'local://')."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        let new_path = format!("{}/{}", payload.path, payload.name);

        if storage.exists(&new_path).await.unwrap_or(false) {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "A file or directory with this name already exists"
            }));
        }

        match storage.write(&new_path, vec![]).await {
            Ok(_) => {
                let query = web::Query(Query {
                    path: payload.path.clone(),
                });
                Self::index(data, query).await
            }
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            })),
        }
    }

    pub async fn rename(
        data: web::Data<VueFinder>,
        payload: web::Json<RenameRequest>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        if !Self::is_plain_name(&payload.name) {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "Invalid new name."
            }));
        }

        let old_path = Self::join_storage_path(&payload.path, &payload.item);
        let new_path = Self::join_storage_path(&payload.path, &payload.name);

        // Use storage rename which handles both files and directories
        match storage.rename(&old_path, &new_path).await {
            Ok(_) => {
                let query = web::Query(Query {
                    path: payload.path.clone(),
                });
                Self::index(data, query).await
            }
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            })),
        }
    }

    async fn copy_dir_recursive(
        source_storage: &Arc<dyn StorageAdapter>,
        dest_storage: &Arc<dyn StorageAdapter>,
        source_path: &str,
        dest_path: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        dest_storage.create_dir(dest_path).await?;
        let contents = source_storage.list_contents(source_path).await?;
        for item in contents {
            let item_source = format!("{}/{}", source_path, item.basename);
            let item_dest = format!("{}/{}", dest_path, item.basename);
            if item.node_type == "dir" {
                Box::pin(Self::copy_dir_recursive(
                    source_storage,
                    dest_storage,
                    &item_source,
                    &item_dest,
                ))
                .await?;
            } else {
                let contents = source_storage.read(&item_source).await?;
                dest_storage.write(&item_dest, contents).await?;
            }
        }
        Ok(())
    }

    pub async fn r#move(
        data: web::Data<VueFinder>,
        payload: web::Json<MoveRequest>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.destination) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid destination path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        // Check if the target path conflicts with existing files
        let items = payload.resolve_items();
        for item in &items {
            let target = match Self::transfer_target_path(&payload.destination, &item.path) {
                Some(target) => target,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source path format."
                    }))
                }
            };
            if storage.exists(&target).await.unwrap_or(false) {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "One of the files already exists."
                }));
            }
        }

        // Execute move operation
        for item in &items {
            let source_storage_name = match data.parse_storage_name_from_path(&item.path) {
                Some(name) => name,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source path format."
                    }))
                }
            };
            let source_storage = match data.storages.get(&source_storage_name) {
                Some(s) => s,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source storage adapter"
                    }))
                }
            };

            let target = match Self::transfer_target_path(&payload.destination, &item.path) {
                Some(target) => target,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source path format."
                    }))
                }
            };

            let item_is_dir = match Self::is_dir_item(source_storage, item).await {
                Ok(is_dir) => is_dir,
                Err(e) => {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": e.to_string()
                    }))
                }
            };

            if item_is_dir {
                if source_storage_name == storage_name
                    && Self::is_same_or_descendant_storage_path(&item.path, &payload.destination)
                {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Cannot move a directory into itself."
                    }));
                }

                if let Err(e) =
                    Self::copy_dir_recursive(source_storage, storage, &item.path, &target).await
                {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to move directory: {}", e)
                    }));
                }
                if let Err(e) = source_storage.delete(&item.path).await {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to delete source directory: {}", e)
                    }));
                }
            } else {
                match source_storage.read(&item.path).await {
                    Ok(contents) => {
                        if let Err(e) = storage.write(&target, contents).await {
                            return HttpResponse::InternalServerError().json(json!({
                                "status": false,
                                "message": e.to_string()
                            }));
                        }
                        if let Err(e) = source_storage.delete(&item.path).await {
                            return HttpResponse::InternalServerError().json(json!({
                                "status": false,
                                "message": e.to_string()
                            }));
                        }
                    }
                    Err(e) => {
                        return HttpResponse::InternalServerError().json(json!({
                            "status": false,
                            "message": e.to_string()
                        }))
                    }
                }
            }
        }

        let query = web::Query(Query {
            path: payload.destination.clone(),
        });
        Self::index(data, query).await
    }

    pub async fn copy(data: web::Data<VueFinder>, payload: web::Json<CopyRequest>) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.destination) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid destination path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        // Check if the target path conflicts with existing files
        let items = payload.resolve_items();
        for item in &items {
            let target = match Self::transfer_target_path(&payload.destination, &item.path) {
                Some(target) => target,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source path format."
                    }))
                }
            };
            if storage.exists(&target).await.unwrap_or(false) {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "One of the files already exists."
                }));
            }
        }

        // Execute copy operation
        for item in &items {
            let source_storage_name = match data.parse_storage_name_from_path(&item.path) {
                Some(name) => name,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source path format."
                    }))
                }
            };
            let source_storage = match data.storages.get(&source_storage_name) {
                Some(s) => s,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source storage adapter"
                    }))
                }
            };

            let target = match Self::transfer_target_path(&payload.destination, &item.path) {
                Some(target) => target,
                None => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid source path format."
                    }))
                }
            };

            let item_is_dir = match Self::is_dir_item(source_storage, item).await {
                Ok(is_dir) => is_dir,
                Err(e) => {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": e.to_string()
                    }))
                }
            };

            if item_is_dir {
                if source_storage_name == storage_name
                    && Self::is_same_or_descendant_storage_path(&item.path, &payload.destination)
                {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Cannot copy a directory into itself."
                    }));
                }

                if let Err(e) =
                    Self::copy_dir_recursive(source_storage, storage, &item.path, &target).await
                {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to copy directory: {}", e)
                    }));
                }
            } else {
                match source_storage.read(&item.path).await {
                    Ok(contents) => {
                        if let Err(e) = storage.write(&target, contents).await {
                            return HttpResponse::InternalServerError().json(json!({
                                "status": false,
                                "message": e.to_string()
                            }));
                        }
                    }
                    Err(e) => {
                        return HttpResponse::InternalServerError().json(json!({
                            "status": false,
                            "message": e.to_string()
                        }))
                    }
                }
            }
        }

        let query = web::Query(Query {
            path: payload.destination.clone(),
        });
        Self::index(data, query).await
    }

    pub async fn delete(
        data: web::Data<VueFinder>,
        payload: web::Json<DeleteRequest>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        for item in &payload.items {
            if let Err(e) = storage.delete(&item.path).await {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": e.to_string()
                }));
            }
        }

        let query = web::Query(Query {
            path: payload.path.clone(),
        });
        Self::index(data, query).await
    }

    pub async fn upload(
        data: web::Data<VueFinder>,
        payload: MultipartForm<UploadForm>,
    ) -> HttpResponse {
        let path = payload.path.to_string();
        let filename = payload.name.to_string();

        if path.is_empty() {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "Missing path in request body."
            }));
        }

        let storage_name = match data.parse_storage_name_from_path(&path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        // Read file data from TempFile
        let file_path = payload.file.file.path();
        let file_data = match std::fs::read(file_path) {
            Ok(data) => data,
            Err(e) => {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": format!("Failed to read uploaded file: {}", e)
                }))
            }
        };

        if filename.is_empty() || file_data.is_empty() {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "Missing file or filename"
            }));
        }

        // Build file path and save file
        let filepath = format!("{}/{}", path, filename);
        if let Err(e) = storage.write(&filepath, file_data).await {
            return HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            }));
        }

        let query = web::Query(Query { path: path.clone() });
        Self::index(data, query).await
    }

    pub async fn archive(
        data: web::Data<VueFinder>,
        payload: web::Json<ArchiveRequest>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        if !Self::is_plain_name(&payload.name) {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "Invalid archive name."
            }));
        }

        let archive_name = if payload.name.ends_with(".zip") {
            payload.name.clone()
        } else {
            format!("{}.zip", payload.name)
        };
        let zip_path = Self::join_storage_path(&payload.path, &archive_name);

        // Check if file already exists
        if storage.exists(&zip_path).await.unwrap_or(false) {
            return HttpResponse::BadRequest().json(json!({
                "status": false,
                "message": "Zip file already exists. Please use a different name."
            }));
        }

        async fn add_dir_to_zip(
            storage: &Arc<dyn StorageAdapter>,
            zip: &mut ZipWriter<std::io::BufWriter<std::fs::File>>,
            storage_path: &str,
            zip_path: &str,
            options: FileOptions,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let contents = storage.list_contents(storage_path).await?;
            for item in contents {
                let item_storage_path = format!("{storage_path}/{}", item.basename);
                let item_zip_path = if zip_path.is_empty() {
                    item.basename.clone()
                } else {
                    format!("{zip_path}/{}", item.basename)
                };

                if item.node_type == "dir" {
                    zip.add_directory(&item_zip_path, options)?;
                    Box::pin(add_dir_to_zip(
                        storage,
                        zip,
                        &item_storage_path,
                        &item_zip_path,
                        options,
                    ))
                    .await?;
                } else {
                    let contents = storage.read(&item_storage_path).await?;
                    zip.start_file(&item_zip_path, options)?;
                    zip.write_all(&contents)?;
                }
            }
            Ok(())
        }

        // Create ZIP file in temp file to avoid borrow issues
        let temp_path = std::env::temp_dir().join(format!(
            "vuefinder_archive_{}.zip",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = match std::fs::File::create(&temp_path) {
            Ok(f) => f,
            Err(e) => {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": format!("Failed to create temp archive file: {}", e)
                }));
            }
        };
        let buf_writer = std::io::BufWriter::new(file);
        let mut zip = ZipWriter::new(buf_writer);

        let options = FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);

        for item in &payload.items {
            if item.r#type == "dir" {
                let dir_name = Path::new(&item.path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();

                if let Err(e) = zip.add_directory(dir_name, options) {
                    let _ = std::fs::remove_file(&temp_path);
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to add directory to ZIP: {}", e)
                    }));
                }
                if let Err(e) = Box::pin(add_dir_to_zip(
                    storage, &mut zip, &item.path, dir_name, options,
                ))
                .await
                {
                    let _ = std::fs::remove_file(&temp_path);
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to archive directory contents: {}", e)
                    }));
                }
            } else {
                match storage.read(&item.path).await {
                    Ok(contents) => {
                        let file_name = Path::new(&item.path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default();

                        if let Err(e) = zip.start_file(file_name, options) {
                            let _ = std::fs::remove_file(&temp_path);
                            return HttpResponse::InternalServerError().json(json!({
                                "status": false,
                                "message": format!("Failed to add file to ZIP: {}", e)
                            }));
                        }

                        if let Err(e) = zip.write_all(&contents) {
                            let _ = std::fs::remove_file(&temp_path);
                            return HttpResponse::InternalServerError().json(json!({
                                "status": false,
                                "message": format!("Failed to write file content: {}", e)
                            }));
                        }
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&temp_path);
                        return HttpResponse::InternalServerError().json(json!({
                            "status": false,
                            "message": format!("Failed to read source file: {}", e)
                        }));
                    }
                }
            }
        }

        if let Err(e) = zip.finish() {
            let _ = std::fs::remove_file(&temp_path);
            return HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": format!("Failed to finalize ZIP file: {}", e)
            }));
        }

        let zip_buffer = match std::fs::read(&temp_path) {
            Ok(b) => b,
            Err(e) => {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": format!("Failed to read archive: {}", e)
                }));
            }
        };
        let _ = std::fs::remove_file(&temp_path);

        // Save ZIP file
        if let Err(e) = storage.write(&zip_path, zip_buffer).await {
            return HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": format!("Failed to save ZIP file: {}", e)
            }));
        }

        let query = web::Query(Query {
            path: payload.path.clone(),
        });
        Self::index(data, query).await
    }

    pub async fn unarchive(
        data: web::Data<VueFinder>,
        payload: web::Json<UnarchiveRequest>,
    ) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        // Read ZIP file
        let zip_contents = match storage.read(&payload.item).await {
            Ok(contents) => contents,
            Err(e) => {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": format!("Failed to read ZIP file: {}", e)
                }));
            }
        };

        let cursor = Cursor::new(zip_contents);
        let mut archive = match zip::ZipArchive::new(cursor) {
            Ok(archive) => archive,
            Err(e) => {
                return HttpResponse::InternalServerError().json(json!({
                    "status": false,
                    "message": format!("Failed to open ZIP file: {}", e)
                }));
            }
        };

        // Extract files
        let extract_path = payload.path.clone();

        // Create extraction target directory
        if let Err(e) = storage.create_dir(&extract_path).await {
            return HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": format!("Failed to create extraction directory: {}", e)
            }));
        }

        for i in 0..archive.len() {
            let mut file = match archive.by_index(i) {
                Ok(file) => file,
                Err(e) => {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to read ZIP file entry: {}", e)
                    }));
                }
            };

            let enclosed_name = match file.enclosed_name() {
                Some(path) if !path.as_os_str().is_empty() => path.to_string_lossy().to_string(),
                _ => {
                    return HttpResponse::BadRequest().json(json!({
                        "status": false,
                        "message": "Invalid ZIP entry path."
                    }));
                }
            };
            if enclosed_name.contains("://") {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid ZIP entry path."
                }));
            }
            let outpath = Self::join_storage_path(&extract_path, &enclosed_name);

            if file.name().ends_with('/') {
                // Create directory
                if let Err(e) = storage.create_dir(&outpath).await {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to create directory: {}", e)
                    }));
                }
            } else {
                // Ensure parent directory exists
                if let Some(p) = Path::new(&outpath).parent() {
                    if let Some(parent_path) = p.to_str() {
                        if let Err(e) = storage.create_dir(parent_path).await {
                            return HttpResponse::InternalServerError().json(json!({
                                "status": false,
                                "message": format!("Failed to create parent directory: {}", e)
                            }));
                        }
                    }
                }

                // Read and write file contents
                let mut buffer = Vec::new();
                if let Err(e) = std::io::copy(&mut file, &mut buffer) {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to read ZIP file content: {}", e)
                    }));
                }

                if let Err(e) = storage.write(&outpath, buffer).await {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": false,
                        "message": format!("Failed to write extracted file: {}", e)
                    }));
                }
            }
        }

        let query = web::Query(Query {
            path: payload.path.clone(),
        });
        Self::index(data, query).await
    }

    pub async fn save(data: web::Data<VueFinder>, payload: web::Json<SaveRequest>) -> HttpResponse {
        let storage_name = match data.parse_storage_name_from_path(&payload.path) {
            Some(name) => name,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid path format."
                }))
            }
        };

        let storage = match data.storages.get(&storage_name) {
            Some(s) => s,
            None => {
                return HttpResponse::BadRequest().json(json!({
                    "status": false,
                    "message": "Invalid storage adapter"
                }))
            }
        };

        match storage
            .write(&payload.path, payload.content.as_bytes().to_vec())
            .await
        {
            Ok(_) => {
                let query = web::Query(Query {
                    path: payload.path.clone(),
                });
                Self::preview(data, query).await
            }
            Err(e) => HttpResponse::InternalServerError().json(json!({
                "status": false,
                "message": e.to_string()
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::body::to_bytes;
    use actix_web::http::StatusCode;
    use tempfile::TempDir;

    use crate::payload::{CopyRequest, MoveRequest};
    use crate::storages::local::LocalStorage;

    fn finder_for(root: &TempDir) -> web::Data<VueFinder> {
        web::Data::new(VueFinder {
            storages: LocalStorage::setup(root.path().to_str().unwrap()),
            config: Arc::new(VueFinderConfig::default()),
        })
    }

    fn assert_ok(response: &HttpResponse) {
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn assert_bad_request(response: &HttpResponse) {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    async fn response_json(response: HttpResponse) -> serde_json::Value {
        let body = to_bytes(response.into_body()).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn rename_uses_current_path_for_relative_item() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("documents")).unwrap();
        std::fs::write(root.path().join("documents/old-name.txt"), b"hello").unwrap();

        let response = VueFinder::rename(
            finder_for(&root),
            web::Json(RenameRequest {
                path: "local://documents".to_string(),
                item: "old-name.txt".to_string(),
                name: "new-name.txt".to_string(),
            }),
        )
        .await;

        assert_ok(&response);
        assert!(!root.path().join("documents/old-name.txt").exists());
        assert_eq!(
            std::fs::read(root.path().join("documents/new-name.txt")).unwrap(),
            b"hello"
        );
    }

    #[tokio::test]
    async fn rename_rejects_path_like_new_name() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents")).unwrap();
        std::fs::create_dir_all(root.path().join("other")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let response = VueFinder::rename(
            finder_for(&root),
            web::Json(RenameRequest {
                path: "local://documents".to_string(),
                item: "file.txt".to_string(),
                name: "local://other/new.txt".to_string(),
            }),
        )
        .await;

        assert_bad_request(&response);
        assert!(root.path().join("documents/file.txt").exists());
        assert!(!root.path().join("other/new.txt").exists());
    }

    #[tokio::test]
    async fn copy_accepts_transfer_params_and_detects_directory_sources() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/subdir/file.txt"), b"nested").unwrap();

        let payload: CopyRequest = serde_json::from_value(json!({
            "sources": ["local://documents/subdir"],
            "destination": "local://backup"
        }))
        .unwrap();

        let response = VueFinder::copy(finder_for(&root), web::Json(payload)).await;

        assert_ok(&response);
        assert_eq!(
            std::fs::read(root.path().join("backup/subdir/file.txt")).unwrap(),
            b"nested"
        );
        assert!(root.path().join("documents/subdir/file.txt").exists());
    }

    #[tokio::test]
    async fn move_accepts_transfer_params_and_detects_directory_sources() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/subdir/file.txt"), b"nested").unwrap();

        let payload: MoveRequest = serde_json::from_value(json!({
            "sources": ["local://documents/subdir"],
            "destination": "local://backup"
        }))
        .unwrap();

        let response = VueFinder::r#move(finder_for(&root), web::Json(payload)).await;

        assert_ok(&response);
        assert_eq!(
            std::fs::read(root.path().join("backup/subdir/file.txt")).unwrap(),
            b"nested"
        );
        assert!(!root.path().join("documents/subdir").exists());
    }

    #[tokio::test]
    async fn copy_uses_normalized_source_basename_for_target() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: CopyRequest = serde_json::from_value(json!({
            "sources": ["local://documents/subdir/.."],
            "destination": "local://backup"
        }))
        .unwrap();

        let response = VueFinder::copy(finder_for(&root), web::Json(payload)).await;

        assert_ok(&response);
        assert_eq!(
            std::fs::read(root.path().join("backup/documents/file.txt")).unwrap(),
            b"hello"
        );
        assert!(!root.path().join("backup/file.txt").exists());
    }

    #[tokio::test]
    async fn move_uses_normalized_source_basename_for_target() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: MoveRequest = serde_json::from_value(json!({
            "sources": ["local://documents/subdir/.."],
            "destination": "local://backup"
        }))
        .unwrap();

        let response = VueFinder::r#move(finder_for(&root), web::Json(payload)).await;

        assert_ok(&response);
        assert_eq!(
            std::fs::read(root.path().join("backup/documents/file.txt")).unwrap(),
            b"hello"
        );
        assert!(!root.path().join("backup/file.txt").exists());
        assert!(!root.path().join("documents").exists());
    }

    #[tokio::test]
    async fn copy_rejects_directory_into_own_descendant() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: CopyRequest = serde_json::from_value(json!({
            "sources": ["local://documents"],
            "destination": "local://documents/subdir"
        }))
        .unwrap();

        let response = VueFinder::copy(finder_for(&root), web::Json(payload)).await;

        assert_bad_request(&response);
        assert!(!root.path().join("documents/subdir/documents").exists());
    }

    #[tokio::test]
    async fn copy_rejects_directory_into_own_descendant_with_dot_component() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: CopyRequest = serde_json::from_value(json!({
            "sources": ["local://documents"],
            "destination": "local://./documents/subdir"
        }))
        .unwrap();

        let response = VueFinder::copy(finder_for(&root), web::Json(payload)).await;

        assert_bad_request(&response);
        assert!(!root.path().join("documents/subdir/documents").exists());
    }

    #[tokio::test]
    async fn move_rejects_directory_into_own_descendant() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: MoveRequest = serde_json::from_value(json!({
            "sources": ["local://documents"],
            "destination": "local://documents/subdir"
        }))
        .unwrap();

        let response = VueFinder::r#move(finder_for(&root), web::Json(payload)).await;

        assert_bad_request(&response);
        assert!(root.path().join("documents/file.txt").exists());
        assert!(!root.path().join("documents/subdir/documents").exists());
    }

    #[tokio::test]
    async fn move_rejects_directory_into_own_descendant_with_parent_component() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("documents/subdir")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: MoveRequest = serde_json::from_value(json!({
            "sources": ["local://documents"],
            "destination": "local://documents/../documents/subdir"
        }))
        .unwrap();

        let response = VueFinder::r#move(finder_for(&root), web::Json(payload)).await;

        assert_bad_request(&response);
        assert!(root.path().join("documents/file.txt").exists());
        assert!(!root.path().join("documents/subdir/documents").exists());
    }

    #[tokio::test]
    async fn copy_returns_destination_listing_when_path_is_present() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("documents")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: CopyRequest = serde_json::from_value(json!({
            "path": "local://documents",
            "sources": ["local://documents/file.txt"],
            "destination": "local://backup"
        }))
        .unwrap();

        let response = VueFinder::copy(finder_for(&root), web::Json(payload)).await;

        assert_ok(&response);
        let body = response_json(response).await;
        assert_eq!(body["dirname"], "local://backup");
    }

    #[tokio::test]
    async fn move_returns_destination_listing_when_path_is_present() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("documents")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let payload: MoveRequest = serde_json::from_value(json!({
            "path": "local://documents",
            "sources": ["local://documents/file.txt"],
            "destination": "local://backup"
        }))
        .unwrap();

        let response = VueFinder::r#move(finder_for(&root), web::Json(payload)).await;

        assert_ok(&response);
        let body = response_json(response).await;
        assert_eq!(body["dirname"], "local://backup");
    }

    #[tokio::test]
    async fn archive_keeps_existing_zip_extension() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("documents")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let response = VueFinder::archive(
            finder_for(&root),
            web::Json(ArchiveRequest {
                path: "local://documents".to_string(),
                name: "bundle.zip".to_string(),
                items: vec![crate::payload::FileItem {
                    path: "local://documents/file.txt".to_string(),
                    r#type: "file".to_string(),
                }],
            }),
        )
        .await;

        assert_ok(&response);
        assert!(root.path().join("documents/bundle.zip").exists());
        assert!(!root.path().join("documents/bundle.zip.zip").exists());
    }

    #[tokio::test]
    async fn archive_rejects_path_like_name() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("documents")).unwrap();
        std::fs::create_dir(root.path().join("other")).unwrap();
        std::fs::write(root.path().join("documents/file.txt"), b"hello").unwrap();

        let response = VueFinder::archive(
            finder_for(&root),
            web::Json(ArchiveRequest {
                path: "local://documents".to_string(),
                name: "local://other/evil.zip".to_string(),
                items: vec![crate::payload::FileItem {
                    path: "local://documents/file.txt".to_string(),
                    r#type: "file".to_string(),
                }],
            }),
        )
        .await;

        assert_bad_request(&response);
        assert!(!root.path().join("other/evil.zip").exists());
    }

    #[tokio::test]
    async fn unarchive_extracts_into_requested_path() {
        let root = TempDir::new().unwrap();
        let archive_path = root.path().join("source.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("file.txt", FileOptions::default()).unwrap();
        zip.write_all(b"hello").unwrap();
        zip.finish().unwrap();

        let response = VueFinder::unarchive(
            finder_for(&root),
            web::Json(UnarchiveRequest {
                item: "local://source.zip".to_string(),
                path: "local://extracted".to_string(),
            }),
        )
        .await;

        assert_ok(&response);
        assert_eq!(
            std::fs::read(root.path().join("extracted/file.txt")).unwrap(),
            b"hello"
        );
        assert!(!root.path().join("extracted/source/file.txt").exists());
    }

    #[tokio::test]
    async fn unarchive_rejects_entries_that_escape_requested_path() {
        let root = TempDir::new().unwrap();
        let archive_path = root.path().join("evil.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("../evil.txt", FileOptions::default())
            .unwrap();
        zip.write_all(b"escape").unwrap();
        zip.finish().unwrap();

        let response = VueFinder::unarchive(
            finder_for(&root),
            web::Json(UnarchiveRequest {
                item: "local://evil.zip".to_string(),
                path: "local://extracted".to_string(),
            }),
        )
        .await;

        assert_bad_request(&response);
        assert!(!root.path().join("evil.txt").exists());
        assert!(!root.path().join("extracted/evil.txt").exists());
    }

    #[tokio::test]
    async fn unarchive_rejects_entries_with_storage_scheme() {
        let root = TempDir::new().unwrap();
        let archive_path = root.path().join("evil.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("local://evil.txt", FileOptions::default())
            .unwrap();
        zip.write_all(b"escape").unwrap();
        zip.finish().unwrap();

        let response = VueFinder::unarchive(
            finder_for(&root),
            web::Json(UnarchiveRequest {
                item: "local://evil.zip".to_string(),
                path: "local://extracted".to_string(),
            }),
        )
        .await;

        assert_bad_request(&response);
        assert!(!root.path().join("evil.txt").exists());
        assert!(!root.path().join("extracted/local:/evil.txt").exists());
    }
}
