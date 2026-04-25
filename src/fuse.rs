use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen,
    ReplyEmpty, Request,
};
use libc::{EIO, ENOENT, EEXIST};
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
        name: String,
        physical_path: PathBuf,
        parent: u64,
    },
}

pub struct OmitFs {
    rt: tokio::runtime::Handle,
    db: Arc<OmitDb>,
    engine: Arc<Mutex<EmbeddingEngine>>,
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

    fn alloc_inode(&mut self) -> u64 {
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
                        let duration = mtime
                            .duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .unwrap_or_default();
                        let ts = std::time::UNIX_EPOCH + duration;
                        attr.mtime = ts;
                        attr.atime = ts;
                        attr.ctime = ts;
                    }
                }
            }
        }

        Some(attr)
    }

    /// Resolve (ino, index) of a named child inside a VirtualDir
    fn find_child_in_dir(&self, dir_ino: u64, name: &str) -> Option<(u64, usize)> {
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get(&dir_ino) {
            for (i, (child_ino, child_name)) in children.iter().enumerate() {
                if child_name == name {
                    return Some((*child_ino, i));
                }
            }
        }
        None
    }
}

impl Filesystem for OmitFs {
    // ──────────────────────────────── LOOKUP ────────────────────────────────
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy().to_string();

        // Inside an already-materialised virtual directory
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get(&parent) {
            for &(child_ino, ref child_name) in children {
                if child_name == &name_str {
                    if let Some(attr) = self.get_attr(child_ino) {
                        reply.entry(&TTL, &attr, 0);
                        return;
                    }
                }
            }
            reply.error(ENOENT);
            return;
        }

        // Root directory — semantic query
        if parent == 1 {
            info!("Semantic FUSE query: «{}»", name_str);

            // Return cached result if query already exists
            let cached: Option<u64> = self.nodes.iter().find_map(|(&ino, node)| {
                if let Node::VirtualDir { name: dir_name, .. } = node {
                    if dir_name == &name_str { Some(ino) } else { None }
                } else {
                    None
                }
            });
            if let Some(ino) = cached {
                if let Some(attr) = self.get_attr(ino) {
                    reply.entry(&TTL, &attr, 0);
                    return;
                }
            }

            // Embed query locally
            let vector = {
                let mut engine = self.engine.lock().unwrap();
                match engine.embed(&name_str) {
                    Ok(v) => v,
                    Err(e) => { error!("Embedding failed: {}", e); reply.error(EIO); return; }
                }
            };

            // Search LanceDB
            let db = self.db.clone();
            let results = self.rt.block_on(async { db.search(vector, 10).await });

            match results {
                Ok(files) => {
                    let dir_ino = self.alloc_inode();
                    let mut children = Vec::new();
                    for (filename, physical_path) in files {
                        let file_ino = self.alloc_inode();
                        children.push((file_ino, filename.clone()));
                        self.nodes.insert(file_ino, Node::VirtualFile {
                            name: filename,
                            physical_path: PathBuf::from(physical_path),
                            parent: dir_ino,
                        });
                    }
                    self.nodes.insert(dir_ino, Node::VirtualDir { name: name_str, children });
                    if let Some(attr) = self.get_attr(dir_ino) {
                        reply.entry(&TTL, &attr, 0);
                    } else {
                        reply.error(EIO);
                    }
                }
                Err(e) => { error!("DB search error: {}", e); reply.error(EIO); }
            }
        } else {
            reply.error(ENOENT);
        }
    }

    // ──────────────────────────────── GETATTR ───────────────────────────────
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match self.get_attr(ino) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(ENOENT),
        }
    }

    // ──────────────────────────────── READDIR ───────────────────────────────
    fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let mut entries: Vec<(u64, FileType, String)> = vec![(ino, FileType::Directory, ".".into())];

        if ino == 1 {
            entries.push((1, FileType::Directory, "..".into()));
        } else if let Some(node) = self.nodes.get(&ino) {
            match node {
                Node::VirtualDir { children, .. } => {
                    entries.push((1, FileType::Directory, "..".into()));
                    for (child_ino, child_name) in children {
                        entries.push((*child_ino, FileType::RegularFile, child_name.clone()));
                    }
                }
                _ => { reply.error(ENOENT); return; }
            }
        } else {
            reply.error(ENOENT);
            return;
        }

        for (i, (e_ino, e_kind, e_name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(e_ino, (i + 1) as i64, e_kind, e_name) {
                break;
            }
        }
        reply.ok();
    }

    // ──────────────────────────────── OPEN ──────────────────────────────────
    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.nodes.get(&ino) {
            Some(Node::VirtualFile { .. }) => reply.opened(0, 0),
            _ => reply.error(ENOENT),
        }
    }

    // ──────────────────────────────── READ ──────────────────────────────────
    fn read(
        &mut self, _req: &Request, ino: u64, _fh: u64,
        offset: i64, size: u32, _flags: i32, _lock: Option<u64>, reply: ReplyData,
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
                Err(e) => { error!("read passthrough failed: {}", e); reply.error(EIO); }
            }
        } else {
            reply.error(ENOENT);
        }
    }

    // ──────────────────────────────── UNLINK (delete) ───────────────────────
    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = name.to_string_lossy().to_string();
        if let Some((ino, idx)) = self.find_child_in_dir(parent, &name_str) {
            if let Some(Node::VirtualFile { physical_path, .. }) = self.nodes.get(&ino) {
                if let Err(e) = std::fs::remove_file(physical_path) {
                    error!("Failed to delete {:?}: {}", physical_path, e);
                    reply.error(EIO);
                    return;
                }
                info!("Deleted {:?}", physical_path);
            }
            if let Some(Node::VirtualDir { children, .. }) = self.nodes.get_mut(&parent) {
                children.remove(idx);
            }
            self.nodes.remove(&ino);
            reply.ok();
        } else {
            reply.error(ENOENT);
        }
    }

    // ──────────────────────────────── RENAME (move / copy-path) ─────────────
    // Invoked by: `mv file.txt /new/path/file.txt` (or any rename syscall).
    // OmitFS moves the physical file on disk and updates internal inode map.
    fn rename(
        &mut self, _req: &Request,
        parent: u64, name: &OsStr,
        newparent: u64, newname: &OsStr,
        _flags: u32, reply: ReplyEmpty,
    ) {
        let name_str    = name.to_string_lossy().to_string();
        let newname_str = newname.to_string_lossy().to_string();

        // Resolve the inode and index of the file being moved
        let Some((ino, old_idx)) = self.find_child_in_dir(parent, &name_str) else {
            reply.error(ENOENT);
            return;
        };

        // Compute destination physical path
        let new_phys = if let Some(Node::VirtualDir { .. }) = self.nodes.get(&newparent) {
            // Move inside a virtual dir → land in raw_dir
            self.raw_dir.join(&newname_str)
        } else if newparent == 1 {
            self.raw_dir.join(&newname_str)
        } else {
            reply.error(ENOENT);
            return;
        };

        // Check collision
        if new_phys.exists() {
            reply.error(EEXIST);
            return;
        }

        // Perform OS-level move
        let old_phys = if let Some(Node::VirtualFile { physical_path, .. }) = self.nodes.get(&ino) {
            physical_path.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        if let Err(e) = std::fs::rename(&old_phys, &new_phys) {
            error!("rename failed {:?} → {:?}: {}", old_phys, new_phys, e);
            reply.error(EIO);
            return;
        }
        info!("Renamed {:?} → {:?}", old_phys, new_phys);

        // Update inode map
        if let Some(Node::VirtualFile { name, physical_path, .. }) = self.nodes.get_mut(&ino) {
            *name = newname_str.clone();
            *physical_path = new_phys;
        }

        // Update child list in old parent
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get_mut(&parent) {
            children.remove(old_idx);
        }

        // Add to new parent (if it's a VirtualDir)
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get_mut(&newparent) {
            children.push((ino, newname_str));
        }

        reply.ok();
    }
}
