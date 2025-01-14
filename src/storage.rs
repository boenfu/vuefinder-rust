use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
pub trait StorageAdapter: Send + Sync {
    async fn list_contents(&self, path: &str) -> Result<Vec<StorageFile>, StorageError>;
    async fn read(&self, path: &str) -> Result<Vec<u8>, StorageError>;
    async fn write(&self, path: &str, contents: Vec<u8>) -> Result<(), StorageError>;
    async fn delete(&self, path: &str) -> Result<(), StorageError>;
    async fn create_dir(&self, path: &str) -> Result<(), StorageError>;
    async fn exists(&self, path: &str) -> Result<bool, StorageError>;
}

pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, StorageError> {
        let clean_path = path.trim_start_matches("local://");
        let full_path = self.root.join(clean_path);
        
        // 安全检查：确保路径在 root 目录下
        if !full_path.starts_with(&self.root) {
            return Err(StorageError::InvalidPath(path.to_string()));
        }
        
        Ok(full_path)
    }
}

#[async_trait]
impl StorageAdapter for LocalStorage {
    async fn list_contents(&self, path: &str) -> Result<Vec<StorageFile>, StorageError> {
        let full_path = self.resolve_path(path)?;
        let mut entries = Vec::new();
        
        let mut read_dir = fs::read_dir(&full_path).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let metadata = entry.metadata().await?;
            let file_type = if metadata.is_dir() { "dir" } else { "file" };
            
            let path = entry
                .path()
                .strip_prefix(&self.root)
                .unwrap()
                .to_string_lossy()
                .into_owned();

            entries.push(StorageFile {
                path,
                file_type: file_type.to_string(),
                size: metadata.len(),
                last_modified: metadata.modified().ok().map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                }),
            });
        }
        
        Ok(entries)
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, StorageError> {
        let full_path = self.resolve_path(path)?;
        Ok(fs::read(&full_path).await?)
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
        let metadata = fs::metadata(&full_path).await?;
        
        if metadata.is_dir() {
            fs::remove_dir_all(&full_path).await?;
        } else {
            fs::remove_file(&full_path).await?;
        }
        
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> Result<(), StorageError> {
        let full_path = self.resolve_path(path)?;
        fs::create_dir_all(&full_path).await?;
        Ok(())
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
        let storage = LocalStorage::new(temp_dir.path());

        // 测试创建目录
        storage.create_dir("test_dir").await.unwrap();
        assert!(storage.exists("test_dir").await.unwrap());

        // 测试写入文件
        storage.write("test_dir/test.txt", b"Hello".to_vec()).await.unwrap();
        assert!(storage.exists("test_dir/test.txt").await.unwrap());

        // 测试读取文件
        let contents = storage.read("test_dir/test.txt").await.unwrap();
        assert_eq!(contents, b"Hello");

        // 测试列出内容
        let entries = storage.list_contents("test_dir").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "test_dir/test.txt");
        assert_eq!(entries[0].file_type, "file");

        // 测试删除
        storage.delete("test_dir/test.txt").await.unwrap();
        assert!(!storage.exists("test_dir/test.txt").await.unwrap());
    }
} 