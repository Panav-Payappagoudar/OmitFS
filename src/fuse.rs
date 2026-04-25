use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyDirectory, ReplyEntry, Request,
};
use libc::ENOENT;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};
use tracing::{error, info};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

use crate::db::OmitDb;
use crate::embedding::EmbeddingEngine;

const TTL: Duration = Duration::from_secs(1);

pub struct OmitFs {
    rt: tokio::runtime::Handle,
    db: Arc<OmitDb>,
    engine: Arc<Mutex<EmbeddingEngine>>,
    raw_dir: PathBuf,
    // Inode mappings for simple virtual POSIX structure
    next_inode: u64,
}

impl OmitFs {
    pub fn new(db: Arc<OmitDb>, engine: Arc<Mutex<EmbeddingEngine>>, raw_dir: PathBuf) -> Self {
        Self {
            rt: tokio::runtime::Handle::current(),
            db,
            engine,
            raw_dir,
            next_inode: 2,
        }
    }
}

impl Filesystem for OmitFs {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent != 1 {
            reply.error(ENOENT);
            return;
        }

        let query = name.to_string_lossy().to_string();
        info!("FUSE lookup query: {}", query);

        let mut engine = self.engine.lock().unwrap();
        let vector = match engine.embed(&query) {
            Ok(v) => v,
            Err(e) => {
                error!("Embedding failed: {}", e);
                reply.error(libc::EIO);
                return;
            }
        };

        let db_clone = self.db.clone();
        let results = self.rt.block_on(async {
            db_clone.search(vector, 10).await
        });

        match results {
            Ok(_files) => {
                // In a complete implementation we would dynamically assign inodes to the 10 files
                // inside this hallucinated directory folder structure.
                let attr = FileAttr {
                    ino: 2,
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
                reply.entry(&TTL, &attr, 0);
            }
            Err(e) => {
                error!("DB search failed: {}", e);
                reply.error(libc::EIO);
            }
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let is_dir = ino == 1 || ino == 2;
        let attr = FileAttr {
            ino,
            size: 4096,
            blocks: 1,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: if is_dir { FileType::Directory } else { FileType::RegularFile },
            perm: if is_dir { 0o755 } else { 0o644 },
            nlink: if is_dir { 2 } else { 1 },
            uid: 501,
            gid: 20,
            rdev: 0,
            flags: 0,
            blksize: 512,
        };
        reply.attr(&TTL, &attr);
    }
    
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino == 1 {
            if offset == 0 {
                let _ = reply.add(1, 0, FileType::Directory, ".");
                let _ = reply.add(1, 1, FileType::Directory, "..");
            }
            reply.ok();
            return;
        }
        
        if ino == 2 {
            if offset == 0 {
                let _ = reply.add(2, 0, FileType::Directory, ".");
                let _ = reply.add(1, 1, FileType::Directory, "..");
                // Ideally list hallucinated files here
            }
            reply.ok();
            return;
        }
        
        reply.error(ENOENT);
    }
}
