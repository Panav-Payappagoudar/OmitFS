use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request,
};
use libc::{EIO, ENOENT};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use tracing::{error, info};

use crate::db::OmitDb;
use crate::embedding::EmbeddingEngine;

const TTL: Duration = Duration::from_secs(1);

enum Node {
    Root,
    VirtualDir {
        name: String,
        children: Vec<(u64, String)>, // ino, filename
    },
    VirtualFile {
        #[allow(dead_code)]
        name: String,
        physical_path: PathBuf,
        #[allow(dead_code)]
        parent: u64,
    },
}

pub struct OmitFs {
    rt: tokio::runtime::Handle,
    db: Arc<OmitDb>,
    engine: Arc<Mutex<EmbeddingEngine>>,
    #[allow(dead_code)]
    raw_dir: PathBuf,
    
    nodes: HashMap<u64, Node>,
    next_inode: u64,
}

impl OmitFs {
    pub fn new(db: Arc<OmitDb>, engine: Arc<Mutex<EmbeddingEngine>>, raw_dir: PathBuf) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(1, Node::Root);

        Self {
            rt: tokio::runtime::Handle::current(),
            db,
            engine,
            raw_dir,
            nodes,
            next_inode: 2,
        }
    }

    fn generate_inode(&mut self) -> u64 {
        let ino = self.next_inode;
        self.next_inode += 1;
        ino
    }

    fn get_attr(&self, ino: u64) -> Option<FileAttr> {
        let node = self.nodes.get(&ino)?;
        
        let mut attr = FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: 501,
            gid: 20,
            rdev: 0,
            flags: 0,
            blksize: 512,
        };

        match node {
            Node::Root | Node::VirtualDir { .. } => {
                attr.kind = FileType::Directory;
                attr.perm = 0o755;
                attr.size = 4096;
            }
            Node::VirtualFile { physical_path, .. } => {
                attr.kind = FileType::RegularFile;
                attr.perm = 0o644;
                attr.nlink = 1;
                if let Ok(metadata) = std::fs::metadata(physical_path) {
                    attr.size = metadata.len();
                    if let Ok(mtime) = metadata.modified() {
                        // A rough approximation to UNIX time for the FUSE bridge
                        let duration = mtime.duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap_or_default();
                        let time_struct = std::time::UNIX_EPOCH + duration;
                        attr.mtime = time_struct;
                        attr.atime = time_struct;
                        attr.ctime = time_struct;
                    }
                }
            }
        }
        
        Some(attr)
    }
}

impl Filesystem for OmitFs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy().to_string();
        
        // 1. If it's a lookup inside a virtual directory (looking for a file)
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get(&parent) {
            for (child_ino, child_name) in children {
                if child_name == &name_str {
                    if let Some(attr) = self.get_attr(*child_ino) {
                        reply.entry(&TTL, &attr, 0);
                        return;
                    }
                }
            }
            reply.error(ENOENT);
            return;
        }

        // 2. If it's a lookup in the Root directory (initiating a semantic query)
        if parent == 1 {
            info!("FUSE semantic query initiated: {}", name_str);
            
            // Return cached query if it already exists
            for (&ino, node) in &self.nodes {
                if let Node::VirtualDir { name: dir_name, .. } = node {
                    if dir_name == &name_str {
                        if let Some(attr) = self.get_attr(ino) {
                            reply.entry(&TTL, &attr, 0);
                            return;
                        }
                    }
                }
            }

            // Perform Local Neural Network Semantic Search
            let vector = {
                let mut engine = self.engine.lock().unwrap();
                match engine.embed(&name_str) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("Embedding failed: {}", e);
                        reply.error(EIO);
                        return;
                    }
                }
            };

            let db_clone = self.db.clone();
            let results = self.rt.block_on(async {
                db_clone.search(vector, 10).await
            });

            match results {
                Ok(files) => {
                    let dir_ino = self.generate_inode();
                    let mut children = Vec::new();

                    for (filename, physical_path) in files {
                        let file_ino = self.generate_inode();
                        children.push((file_ino, filename.clone()));
                        
                        self.nodes.insert(file_ino, Node::VirtualFile {
                            name: filename,
                            physical_path: PathBuf::from(physical_path),
                            parent: dir_ino,
                        });
                    }

                    self.nodes.insert(dir_ino, Node::VirtualDir {
                        name: name_str,
                        children,
                    });

                    if let Some(attr) = self.get_attr(dir_ino) {
                        reply.entry(&TTL, &attr, 0);
                    } else {
                        reply.error(EIO);
                    }
                }
                Err(e) => {
                    error!("DB search failed: {}", e);
                    reply.error(EIO);
                }
            }
        } else {
            reply.error(ENOENT);
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if let Some(attr) = self.get_attr(ino) {
            reply.attr(&TTL, &attr);
        } else {
            reply.error(ENOENT);
        }
    }
    
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let mut entries = vec![
            (ino, FileType::Directory, ".".to_string()),
        ];
        
        if ino == 1 {
            entries.push((1, FileType::Directory, "..".to_string()));
            // The Root void is kept empty intentionally to force intent-based `cd` routing
        } else if let Some(node) = self.nodes.get(&ino) {
            match node {
                Node::VirtualDir { children, .. } => {
                    entries.push((1, FileType::Directory, "..".to_string()));
                    for (child_ino, child_name) in children {
                        entries.push((*child_ino, FileType::RegularFile, child_name.clone()));
                    }
                }
                _ => {
                    reply.error(ENOENT);
                    return;
                }
            }
        } else {
            reply.error(ENOENT);
            return;
        }

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            // reply.add returns true if the buffer is full
            if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                break;
            }
        }
        reply.ok();
    }
    
    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        if let Some(Node::VirtualFile { .. }) = self.nodes.get(&ino) {
            reply.opened(0, 0); // Assign default file handle 0
        } else {
            reply.error(ENOENT);
        }
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if let Some(Node::VirtualFile { physical_path, .. }) = self.nodes.get(&ino) {
            match std::fs::read(physical_path) {
                Ok(data) => {
                    let start = offset as usize;
                    if start >= data.len() {
                        reply.data(&[]);
                    } else {
                        let end = std::cmp::min(start + size as usize, data.len());
                        reply.data(&data[start..end]);
                    }
                }
                Err(e) => {
                    error!("Failed to read physical file mapping: {}", e);
                    reply.error(EIO);
                }
            }
        } else {
            reply.error(ENOENT);
        }
    }
}
