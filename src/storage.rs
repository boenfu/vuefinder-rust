use async_trait::async_trait;
use mime_guess::from_path;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::SystemTime;
use thiserror::Error;
use tokio::fs;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Path not found: {0}")]
    NotFound(String),
    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StorageFile {
    pub path: String,
    pub file_type: String,
    pub size: u64,
    pub last_modified: Option<u64>,
}

#[async_trait]
pub trait StorageAdapter {
    async fn list_contents(
        &self,
        path: &str,
    ) -> Result<Vec<StorageItem>, Box<dyn std::error::Error>>;
    async fn read(&self, path: &str) -> Result<Vec<u8>, StorageError>;
    async fn write(&self, path: &str, contents: Vec<u8>) -> Result<(), StorageError>;
    async fn delete(&self, path: &str) -> Result<(), StorageError>;
    async fn create_dir(&self, path: &str) -> Result<(), StorageError>;
    async fn exists(&self, path: &str) -> Result<bool, StorageError>;
}

#[derive(Debug, Serialize)]
pub struct StorageItem {
    pub node_type: String,
    pub path: String,
    pub basename: String,
    pub extension: Option<String>,
    pub mime_type: Option<String>,
    pub last_modified: Option<u64>,
}

#[derive(Debug)]
pub struct LocalStorage {
    root: String,
}

impl LocalStorage {
    pub fn new(root: &str) -> Self {
        Self {
            root: root.to_string(),
        }
    }

    // 解析并验证路径
    fn resolve_path(&self, path: &str) -> Result<PathBuf, StorageError> {
        let clean_path = path.trim_start_matches("local://");
        let full_path = PathBuf::from(&self.root).join(clean_path);

        // 安全检查：确保路径在 root 目录下
        if !full_path.starts_with(&self.root) {
            return Err(StorageError::InvalidPath(path.to_string()));
        }

        Ok(full_path)
    }
}

#[async_trait]
impl StorageAdapter for LocalStorage {
    async fn list_contents(
        &self,
        path: &str,
    ) -> Result<Vec<StorageItem>, Box<dyn std::error::Error>> {
        let full_path = self.resolve_path(path)?;
        println!("Full path: {:?}", full_path); // 打印 full_path
        let mut entries = Vec::new();

        let mut read_dir = fs::read_dir(&full_path).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let metadata = entry.metadata().await?;
            let path_buf = entry.path();
            let relative_path = path_buf
                .strip_prefix(&self.root)
                .unwrap()
                .to_string_lossy()
                .into_owned();

            let basename = path_buf
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();

            let extension = path_buf
                .extension()
                .map(|ext| ext.to_string_lossy().into_owned());

            let mime_type = if metadata.is_file() {
                Some(
                    from_path(&path_buf)
                        .first_or_octet_stream()
                        .essence_str()
                        .to_owned(),
                )
            } else {
                None
            };

            let last_modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());

            println!("Last modified: {:?}", last_modified);

            entries.push(StorageItem {
                node_type: if metadata.is_dir() {
                    "dir".to_string()
                } else {
                    "file".to_string()
                },
                path: relative_path,
                basename,
                extension,
                mime_type,
                last_modified,
            });
        }

        Ok(entries)
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, StorageError> {
        let full_path = self.resolve_path(path)?;
        match fs::read(&full_path).await {
            Ok(contents) => Ok(contents),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                Err(StorageError::NotFound(path.to_string()))
            }
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn write(&self, path: &str, contents: Vec<u8>) -> Result<(), StorageError> {
        let full_path = self.resolve_path(path)?;

        // 确保父目录存在
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&full_path, contents).await?;
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<(), StorageError> {
        let full_path = self.resolve_path(path)?;

        match fs::metadata(&full_path).await {
            Ok(metadata) => {
                if metadata.is_dir() {
                    fs::remove_dir_all(&full_path).await?;
                } else {
                    fs::remove_file(&full_path).await?;
                }
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                Err(StorageError::NotFound(path.to_string()))
            }
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn create_dir(&self, path: &str) -> Result<(), StorageError> {
        let full_path = self.resolve_path(path)?;

        match fs::create_dir_all(&full_path).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn exists(&self, path: &str) -> Result<bool, StorageError> {
        let full_path = self.resolve_path(path)?;
        Ok(fs::try_exists(&full_path).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_local_storage() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_str().unwrap());

        // 测试创建目录
        storage.create_dir("test_dir").await.unwrap();
        assert!(storage.exists("test_dir").await.unwrap());

        // 测试写入文件
        storage
            .write("test_dir/test.txt", b"Hello".to_vec())
            .await
            .unwrap();
        assert!(storage.exists("test_dir/test.txt").await.unwrap());

        // 测试读取文件
        let contents = storage.read("test_dir/test.txt").await.unwrap();
        assert_eq!(contents, b"Hello");

        // 测试列出内容
        let entries = storage.list_contents("test_dir").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "test_dir/test.txt");
        assert_eq!(entries[0].node_type, "file");
        assert_eq!(entries[0].extension.as_deref(), Some("txt"));
        assert_eq!(entries[0].mime_type.as_deref(), Some("text/plain"));

        // 测试删除
        storage.delete("test_dir/test.txt").await.unwrap();
        assert!(!storage.exists("test_dir/test.txt").await.unwrap());

        // 测试删除目录
        storage.delete("test_dir").await.unwrap();
        assert!(!storage.exists("test_dir").await.unwrap());
    }

    #[tokio::test]
    async fn test_invalid_paths() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_str().unwrap());

        // 测试路径穿越攻击
        assert!(storage.read("../outside.txt").await.is_err());
        assert!(storage.write("../outside.txt", vec![]).await.is_err());

        // 测试不存在的文件
        assert!(matches!(
            storage.read("nonexistent.txt").await,
            Err(StorageError::NotFound(_))
        ));
    }
}
